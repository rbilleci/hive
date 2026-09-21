//! The row-level layer every command, the worker and the approval surface build on: the batched
//! `Deployment` loader and the approval-requirement mappers on the read side, and the
//! timeline/audit/outbox/insert helpers on the write side — `audit`, `stage`, `enqueue`,
//! `touch_projection`, the timeline-sequence allocators, and the new-deployment-cycle inserts
//! (`insert_deployment`/`insert_plan`/.../`insert_approval_requirement`).
//!
//! Every write here is a SeaORM `ActiveModel` insert, an `update_many` with the guard in its
//! `WHERE` clause, or an `on_conflict` upsert. The policy snapshot and the approval requirement
//! read the deployment row first and insert by key: both run inside the command's transaction,
//! after that transaction created the deployment row itself, so no other writer can see or change
//! it in between.

use crate::audit::context::request_metadata;
use crate::entity::enums::{
    DeploymentAttemptStatus, DeploymentAuditAction, DeploymentEvidenceKind,
    DeploymentLifecycleStatus as EntityLifecycleStatus, DeploymentOutboxEventType,
    DeploymentOutboxStatus, DeploymentRuntimeHealthStatus, DeploymentStage, DeploymentStageStatus,
    EvaluationTargetKind, EvidenceInvalidationKind, LogicalEnvironmentClass,
};
use crate::entity::{
    agent_versions, agents, deployment_approval_decisions, deployment_approval_requirements,
    deployment_attempts, deployment_audit_events, deployment_evidence_invalidations,
    deployment_evidence_snapshots, deployment_outbox_events, deployment_plan_review_facts,
    deployment_plan_versions, deployment_policy_snapshots, deployment_runtime_health,
    deployment_stage_events, deployment_timeline_counters, deployments,
    environment_definition_versions, evaluation_target_projections,
};
use chrono::{DateTime, Utc};
use hive_application::deployment::{
    ApprovalEvidenceState, ApprovalRequirementStatus, CompiledRequest, Deployment,
    DeploymentAttempt, DeploymentEnvironment, DeploymentEvidence, DeploymentPlan,
    DeploymentPlanReview, DeploymentPolicy, DeploymentRollbackTarget, DeploymentRuntimeHealth,
};
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::sea_query::{Expr, ExprTrait, Func, IntoTableRef, LockType, OnConflict, Query};
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, FromQueryResult,
    JoinType, NotSet, QueryFilter, QueryOrder, QuerySelect, QueryTrait, RelationTrait, Set,
};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

pub struct RawRequirement {
    pub id: Uuid,
    pub deployment_id: Uuid,
    pub project_id: Uuid,
    pub requester_id: Uuid,
    pub requested_at: DateTime<Utc>,
    pub revision: i64,
    pub status: ApprovalRequirementStatus,
    pub expires_at: Option<DateTime<Utc>>,
    pub satisfied_at: Option<DateTime<Utc>>,
    pub rejected_at: Option<DateTime<Utc>>,
    pub invalidated_at: Option<DateTime<Utc>>,
    pub invalidation_code: Option<String>,
    pub satisfied_participants: Vec<Uuid>,
    pub required_approvers: i32,
}

/// The requirement with the deployment facts the approval surface reads next to it. The inner join
/// to `deployment_policy_snapshots` is an existence test only — it selects no column — so a
/// deployment with no frozen policy snapshot answers "no requirement".
#[derive(FromQueryResult)]
struct RequirementRow {
    id: Uuid,
    deployment_id: Uuid,
    project_id: Uuid,
    requester_id: Uuid,
    requested_at: DateTimeWithTimeZone,
    revision: i64,
    status: crate::entity::enums::ApprovalRequirementStatus,
    expires_at: DateTimeWithTimeZone,
    satisfied_at: Option<DateTimeWithTimeZone>,
    rejected_at: Option<DateTimeWithTimeZone>,
    invalidated_at: Option<DateTimeWithTimeZone>,
    invalidation_code: Option<crate::entity::enums::ApprovalInvalidationCode>,
    satisfied_participants: serde_json::Value,
    required_approvers: i32,
}

impl From<RequirementRow> for RawRequirement {
    fn from(row: RequirementRow) -> Self {
        RawRequirement {
            id: row.id,
            deployment_id: row.deployment_id,
            project_id: row.project_id,
            requester_id: row.requester_id,
            requested_at: row.requested_at.with_timezone(&Utc),
            revision: row.revision,
            status: row.status.into(),
            expires_at: Some(row.expires_at.with_timezone(&Utc)),
            satisfied_at: row.satisfied_at.map(|value| value.with_timezone(&Utc)),
            rejected_at: row.rejected_at.map(|value| value.with_timezone(&Utc)),
            invalidated_at: row.invalidated_at.map(|value| value.with_timezone(&Utc)),
            invalidation_code: row.invalidation_code.map(|code| code.to_value()),
            satisfied_participants: string_list(&row.satisfied_participants)
                .into_iter()
                .filter_map(|value| Uuid::parse_str(&value).ok())
                .collect(),
            required_approvers: row.required_approvers,
        }
    }
}

/// The requirement joined to its deployment and its frozen policy snapshot, locked
/// `FOR UPDATE OF requirement, deployment` when `lock` is set.
fn requirement_select() -> sea_orm::Select<deployment_approval_requirements::Entity> {
    deployment_approval_requirements::Entity::find()
        .join(
            JoinType::InnerJoin,
            deployment_approval_requirements::Relation::Deployments.def(),
        )
        .join(
            JoinType::InnerJoin,
            deployments::Relation::DeploymentPolicySnapshots.def(),
        )
        .select_only()
        .column(deployment_approval_requirements::Column::Id)
        .column(deployment_approval_requirements::Column::DeploymentId)
        .column(deployments::Column::ProjectId)
        .column_as(deployments::Column::RequestedBy, "requester_id")
        .column_as(deployments::Column::RequestedAt, "requested_at")
        .column(deployment_approval_requirements::Column::Revision)
        .column(deployment_approval_requirements::Column::Status)
        .column(deployment_approval_requirements::Column::ExpiresAt)
        .column(deployment_approval_requirements::Column::SatisfiedAt)
        .column(deployment_approval_requirements::Column::RejectedAt)
        .column(deployment_approval_requirements::Column::InvalidatedAt)
        .column(deployment_approval_requirements::Column::InvalidationCode)
        .column(deployment_approval_requirements::Column::SatisfiedParticipants)
        .column(deployment_approval_requirements::Column::RequiredApprovers)
}

fn requirement_lock(
    mut select: sea_orm::Select<deployment_approval_requirements::Entity>,
    lock: bool,
) -> sea_orm::Select<deployment_approval_requirements::Entity> {
    if lock {
        QuerySelect::query(&mut select).lock_with_tables(
            LockType::Update,
            [
                deployment_approval_requirements::Entity.into_table_ref(),
                deployments::Entity.into_table_ref(),
            ],
        );
    }
    select
}

pub async fn raw_requirement(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
    lock: bool,
) -> Result<Option<RawRequirement>, DbErr> {
    Ok(requirement_lock(
        requirement_select()
            .filter(deployment_approval_requirements::Column::Id.eq(requirement_id)),
        lock,
    )
    .into_model::<RequirementRow>()
    .one(db)
    .await?
    .map(RawRequirement::from))
}

/// Loads a requirement by deployment only for commands that already hold the deployment row lock.
pub async fn raw_requirement_by_deployment(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    lock: bool,
) -> Result<Option<RawRequirement>, DbErr> {
    Ok(requirement_lock(
        requirement_select()
            .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id)),
        lock,
    )
    .into_model::<RequirementRow>()
    .one(db)
    .await?
    .map(RawRequirement::from))
}

pub struct ApprovalDecisionRow {
    pub id: Uuid,
    pub requirement_id: Uuid,
    pub actor_principal_id: Uuid,
    pub value: String,
    pub comment: Option<String>,
    pub rejection_reason: Option<String>,
    pub eligibility_checked_at: DateTime<Utc>,
    pub decided_at: DateTime<Utc>,
}

const REVIEW_CODES: [&str; 4] = [
    "REVIEWED_CHANGE_SCOPE",
    "AUTHORIZATION_GRANTED",
    "UNACCEPTABLE_CHANGE_SCOPE",
    "CHANGE_SCOPE_NOT_APPROVED",
];

/// Projects only M14's finite review-code vocabulary from immutable predecessor rows.
pub fn review_text(value: Option<String>) -> Option<String> {
    let value = value?;
    let normalized = value.trim().to_string();
    REVIEW_CODES
        .contains(&normalized.as_str())
        .then_some(normalized)
}

pub fn decision_row(row: deployment_approval_decisions::Model) -> ApprovalDecisionRow {
    ApprovalDecisionRow {
        id: row.id,
        requirement_id: row.approval_requirement_id,
        actor_principal_id: row.actor_principal_id,
        value: row.decision.to_value(),
        comment: review_text(row.comment),
        rejection_reason: review_text(row.rejection_reason),
        eligibility_checked_at: row.eligibility_checked_at.with_timezone(&Utc),
        decided_at: row.decided_at.with_timezone(&Utc),
    }
}

/// The evidence state, decided against the deployment's frozen policy, its recorded invalidations
/// and the clock.
pub(super) fn evidence_state(
    evidence: &deployment_evidence_snapshots::Model,
    policy: &deployment_policy_snapshots::Model,
    invalidations: &[deployment_evidence_invalidations::Model],
    now: DateTimeWithTimeZone,
) -> ApprovalEvidenceState {
    if invalidations.iter().any(|row| {
        row.evidence_snapshot_id == evidence.id && row.kind == EvidenceInvalidationKind::Failed
    }) {
        return ApprovalEvidenceState::Failed;
    }
    if invalidations.iter().any(|row| {
        row.evidence_snapshot_id == evidence.id && row.kind == EvidenceInvalidationKind::Revoked
    }) {
        return ApprovalEvidenceState::Revoked;
    }
    if evidence
        .expires_at
        .is_some_and(|expires_at| expires_at <= now)
    {
        return ApprovalEvidenceState::Expired;
    }
    let matches = sql_eq(&evidence.binding_digest, &policy.binding_digest)
        && sql_eq(&evidence.agent_version_id, &policy.agent_version_id)
        && sql_eq(
            &evidence.environment_definition_version_id,
            &policy.environment_definition_version_id,
        )
        && sql_eq(&evidence.target_digest, &policy.target_digest)
        && sql_eq(&evidence.plan_digest, &policy.plan_digest)
        && sql_eq(&evidence.package_digest, &policy.package_digest);
    if matches {
        ApprovalEvidenceState::Valid
    } else {
        ApprovalEvidenceState::Mismatch
    }
}

/// SQL equality, where a comparison with `NULL` is never true.
fn sql_eq<T: PartialEq>(left: &Option<T>, right: &Option<T>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left == right)
}

/// A `jsonb` array of strings as the column holds it.
pub(super) fn string_list(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Reads a nullable column into a non-null field, naming the column when it is null.
fn required<T>(value: Option<T>, column: &str) -> Result<T, DbErr> {
    value.ok_or_else(|| DbErr::Type(format!("deployment column `{column}` is null")))
}

fn by_id<M, K: std::hash::Hash + Eq, F: Fn(&M) -> K>(rows: Vec<M>, key: F) -> HashMap<K, M> {
    rows.into_iter().map(|row| (key(&row), row)).collect()
}

/// The prior active deployment of the same agent and environment, as one ordered single-row query.
/// A candidate without a published version, a frozen plan or observed runtime health is passed
/// over. The read takes no lock.
async fn rollback_target(
    db: &impl ConnectionTrait,
    deployment: &deployments::Model,
) -> Result<Option<DeploymentRollbackTarget>, DbErr> {
    let Some(environment_id) = deployment.environment_definition_version_id else {
        return Ok(None);
    };
    // `(candidate.requested_at, candidate.id) < (deployment.requested_at, deployment.id)`.
    let before = Condition::any()
        .add(deployments::Column::RequestedAt.lt(deployment.requested_at))
        .add(
            Condition::all()
                .add(deployments::Column::RequestedAt.eq(deployment.requested_at))
                .add(deployments::Column::Id.lt(deployment.id)),
        );
    let Some(candidate) = deployments::Entity::find()
        .join(
            JoinType::InnerJoin,
            deployments::Relation::AgentVersions.def(),
        )
        .join(
            JoinType::InnerJoin,
            deployments::Relation::DeploymentPlanVersions
                .def()
                .on_condition(|_left, right| {
                    Condition::all().add(
                        Expr::col((right, deployment_plan_versions::Column::VersionNumber))
                            .eq(1_i64),
                    )
                }),
        )
        .join(
            JoinType::InnerJoin,
            deployments::Relation::DeploymentRuntimeHealth.def(),
        )
        .filter(deployments::Column::ProjectId.eq(deployment.project_id))
        .filter(deployments::Column::AgentId.eq(deployment.agent_id))
        .filter(deployments::Column::EnvironmentDefinitionVersionId.eq(environment_id))
        .filter(deployments::Column::LifecycleStatus.eq(EntityLifecycleStatus::Active))
        .filter(deployments::Column::Id.ne(deployment.id))
        .filter(before)
        .order_by_desc(deployments::Column::RequestedAt)
        .order_by_desc(deployments::Column::Id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let version = agent_versions::Entity::find_by_id(candidate.agent_version_id)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no agent version for {}", candidate.id)))?;
    let plan = deployment_plan_versions::Entity::find()
        .filter(deployment_plan_versions::Column::DeploymentId.eq(candidate.id))
        .filter(deployment_plan_versions::Column::VersionNumber.eq(1_i64))
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no frozen plan for {}", candidate.id)))?;
    let health = deployment_runtime_health::Entity::find_by_id(candidate.id)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no runtime health for {}", candidate.id)))?;
    Ok(Some(DeploymentRollbackTarget {
        deployment_id: candidate.id,
        agent_version_id: candidate.agent_version_id,
        agent_version_number: version.version_number,
        target_digest: required(plan.target_digest, "target_digest")?,
        runtime_health: DeploymentRuntimeHealth {
            status: health.status.to_value(),
            summary: health.summary,
            observed_at: Some(health.observed_at.to_utc()),
            generation: health.generation,
        },
    }))
}

/// The application `Deployment` for each of `ids`, newest request first.
///
/// Seven tables, read as entities and assembled in Rust: a deployment whose agent, published
/// version, environment definition version, frozen plan, policy snapshot or runtime health row is
/// missing is left out, and `include_canonical_plan: false` also requires the retained review
/// facts. The evidence list is the deployment's snapshots ordered by kind, each with its computed
/// state.
pub async fn deployments(
    db: &impl ConnectionTrait,
    ids: &[Uuid],
    include_canonical_plan: bool,
) -> Result<Vec<Deployment>, DbErr> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = ids.to_vec();
    let rows = deployments::Entity::find()
        .filter(deployments::Column::Id.is_in(ids.clone()))
        .order_by_desc(deployments::Column::RequestedAt)
        .order_by_desc(deployments::Column::Id)
        .all(db)
        .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let found: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let agents = by_id(
        agents::Entity::find()
            .filter(agents::Column::Id.is_in(rows.iter().map(|row| row.agent_id)))
            .all(db)
            .await?,
        |row| row.id,
    );
    let versions = by_id(
        agent_versions::Entity::find()
            .filter(agent_versions::Column::Id.is_in(rows.iter().map(|row| row.agent_version_id)))
            .all(db)
            .await?,
        |row| row.id,
    );
    let environments = by_id(
        environment_definition_versions::Entity::find()
            .filter(
                environment_definition_versions::Column::Id.is_in(
                    rows.iter()
                        .filter_map(|row| row.environment_definition_version_id),
                ),
            )
            .all(db)
            .await?,
        |row| row.id,
    );
    let plans = by_id(
        deployment_plan_versions::Entity::find()
            .filter(deployment_plan_versions::Column::DeploymentId.is_in(found.clone()))
            .filter(deployment_plan_versions::Column::VersionNumber.eq(1_i64))
            .all(db)
            .await?,
        |row| row.deployment_id,
    );
    let reviews = by_id(
        deployment_plan_review_facts::Entity::find()
            .filter(
                deployment_plan_review_facts::Column::PlanId
                    .is_in(plans.values().map(|plan| plan.id).collect::<Vec<_>>()),
            )
            .all(db)
            .await?,
        |row| row.plan_id,
    );
    let policies = by_id(
        deployment_policy_snapshots::Entity::find()
            .filter(deployment_policy_snapshots::Column::DeploymentId.is_in(found.clone()))
            .all(db)
            .await?,
        |row| row.deployment_id,
    );
    let health = by_id(
        deployment_runtime_health::Entity::find()
            .filter(deployment_runtime_health::Column::DeploymentId.is_in(found.clone()))
            .all(db)
            .await?,
        |row| row.deployment_id,
    );
    let mut attempts: HashMap<Uuid, deployment_attempts::Model> = HashMap::new();
    for attempt in deployment_attempts::Entity::find()
        .filter(deployment_attempts::Column::DeploymentId.is_in(found.clone()))
        .all(db)
        .await?
    {
        match attempts.get(&attempt.deployment_id) {
            Some(current) if current.attempt_number >= attempt.attempt_number => {}
            _ => {
                attempts.insert(attempt.deployment_id, attempt);
            }
        }
    }
    let evidence_rows = deployment_evidence_snapshots::Entity::find()
        .filter(deployment_evidence_snapshots::Column::DeploymentId.is_in(found.clone()))
        .order_by_asc(deployment_evidence_snapshots::Column::EvidenceKind)
        .all(db)
        .await?;
    let invalidations = deployment_evidence_invalidations::Entity::find()
        .filter(
            deployment_evidence_invalidations::Column::EvidenceSnapshotId
                .is_in(evidence_rows.iter().map(|row| row.id).collect::<Vec<_>>()),
        )
        .all(db)
        .await?;
    let now = chrono::Utc::now().fixed_offset();

    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        let (
            Some(agent),
            Some(version),
            Some(environment_id),
            Some(plan),
            Some(policy),
            Some(runtime_health),
        ) = (
            agents.get(&row.agent_id),
            versions.get(&row.agent_version_id),
            row.environment_definition_version_id,
            plans.get(&row.id),
            policies.get(&row.id),
            health.get(&row.id),
        )
        else {
            continue;
        };
        let Some(environment) = environments.get(&environment_id) else {
            continue;
        };
        let review = reviews.get(&plan.id);
        // `include_canonical_plan: false` is the summary read, which requires the retained review
        // facts.
        if review.is_none() && !include_canonical_plan {
            continue;
        }
        let evidence: Vec<DeploymentEvidence> = evidence_rows
            .iter()
            .filter(|evidence| evidence.deployment_id == row.id)
            .map(|evidence| DeploymentEvidence {
                kind: evidence.evidence_kind.to_value(),
                digest: Some(evidence.evidence_digest.clone()),
                binding_digest: evidence.binding_digest.clone(),
                expires_at: evidence.expires_at.map(|value| value.to_utc()),
                state: evidence_state(evidence, policy, &invalidations, now),
            })
            .collect();
        values.push(Deployment {
            id: row.id,
            project_id: row.project_id,
            agent_id: row.agent_id,
            agent_display_name: agent.display_name.clone(),
            agent_version_id: row.agent_version_id,
            agent_version_number: version.version_number,
            environment: DeploymentEnvironment {
                id: environment.id,
                stable_definition_id: environment.stable_definition_id.clone(),
                version: environment.version.clone(),
                display_name: environment.display_name.clone(),
                logical_environment_class: environment.logical_environment_class.to_value(),
                catalog_release_id: environment.catalog_release_id.clone(),
                catalog_release_digest: environment.catalog_release_digest.clone(),
                content_digest: environment.content_digest.clone(),
            },
            strategy: row.strategy.into(),
            lifecycle_status: row.lifecycle_status.into(),
            revision: row.revision,
            projection_revision: row.projection_revision.unwrap_or_default(),
            requested_by: row.requested_by,
            requested_at: row.requested_at.to_utc(),
            plan: DeploymentPlan {
                agent_version_id: row.agent_version_id,
                agent_content_digest: required(
                    plan.agent_content_digest.clone(),
                    "agent_content_digest",
                )?,
                environment_definition_version_id: environment.id,
                target_digest: required(plan.target_digest.clone(), "target_digest")?,
                plan_digest: plan.plan_digest.clone(),
                package_digest: plan.package_digest.clone(),
                package_reference: Some(plan.package_reference.clone()),
                compiler_version: plan.compiler_version.clone(),
                catalog_release_id: plan.catalog_release_id.clone(),
                catalog_release_digest: required(
                    plan.catalog_release_digest.clone(),
                    "catalog_release_digest",
                )?,
                canonical_plan: include_canonical_plan.then(|| plan.canonical_plan.to_string()),
                review: DeploymentPlanReview {
                    active_agent_version_number: review
                        .and_then(|review| review.active_agent_version_number),
                    change_summary: review.map_or_else(
                        || REVIEW_UNAVAILABLE.to_string(),
                        |review| review.change_summary.clone(),
                    ),
                    added_dependency_versions: review
                        .map(|review| string_list(&review.added_dependency_versions))
                        .unwrap_or_default(),
                    removed_dependency_versions: review
                        .map(|review| string_list(&review.removed_dependency_versions))
                        .unwrap_or_default(),
                },
            },
            policy: DeploymentPolicy {
                policy_digest: policy.policy_digest.clone(),
                policy_revision: policy.policy_revision,
                logical_environment_class: policy.logical_environment_class.to_value(),
                risk: policy.risk.into(),
                binding_digest: required(policy.binding_digest.clone(), "binding_digest")?,
                required_evidence: string_list(&policy.required_evidence),
                required_approvers: policy.required_approvers,
                evaluation_requirement_expires_at: policy
                    .evaluation_requirement_expires_at
                    .map(|value| value.to_utc()),
                evidence,
            },
            current_attempt: attempts.get(&row.id).map(|attempt| DeploymentAttempt {
                id: attempt.id,
                number: attempt.attempt_number,
                status: attempt.status.into(),
                generation: attempt.generation,
                started_at: attempt.started_at.map(|value| value.to_utc()),
                completed_at: attempt.completed_at.map(|value| value.to_utc()),
                failure_code: attempt.failure_code.clone(),
                failure_summary: attempt.failure_summary.clone(),
            }),
            runtime_health: DeploymentRuntimeHealth {
                status: runtime_health.status.to_value(),
                summary: runtime_health.summary.clone(),
                observed_at: Some(runtime_health.observed_at.to_utc()),
                generation: runtime_health.generation,
            },
            rollback_target: rollback_target(db, &row).await?,
        });
    }
    Ok(values)
}

/// The sentence a plan with no retained facts reads.
const REVIEW_UNAVAILABLE: &str = "Retained plan review facts are unavailable.";

/// Makes every returned detail/timeline append visible to stale-response protection.
pub async fn touch_projection(db: &impl ConnectionTrait, deployment_id: Uuid) -> Result<(), DbErr> {
    deployments::Entity::update_many()
        .col_expr(
            deployments::Column::ProjectionRevision,
            Expr::col(deployments::Column::ProjectionRevision).add(1),
        )
        .filter(deployments::Column::Id.eq(deployment_id))
        .exec(db)
        .await?;
    Ok(())
}

async fn lock_timeline_deployment(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    let found = deployments::Entity::find_by_id(deployment_id)
        .lock_exclusive()
        .select_only()
        .column(deployments::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?;
    if found.is_none() {
        return Err(DbErr::RecordNotFound(format!(
            "no deployment with id {deployment_id}"
        )));
    }
    Ok(())
}

/// Allocates a cross-table sequence while the deployment row serializes writers for this target.
pub async fn next_timeline_sequence(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    attempt_number: i64,
) -> Result<i64, DbErr> {
    lock_timeline_deployment(db, deployment_id).await?;
    let counter =
        deployment_timeline_counters::Entity::insert(deployment_timeline_counters::ActiveModel {
            deployment_id: Set(deployment_id),
            attempt_number: Set(attempt_number),
            next_sequence: Set(2),
        })
        .on_conflict(
            OnConflict::columns([
                deployment_timeline_counters::Column::DeploymentId,
                deployment_timeline_counters::Column::AttemptNumber,
            ])
            .value(
                deployment_timeline_counters::Column::NextSequence,
                Expr::col((
                    deployment_timeline_counters::Entity,
                    deployment_timeline_counters::Column::NextSequence,
                ))
                .add(1),
            )
            .to_owned(),
        )
        .exec_with_returning(db)
        .await?;
    Ok(counter.next_sequence - 1)
}

pub struct TimelineAnchor {
    pub attempt_id: Option<Uuid>,
    pub attempt_number: i64,
    pub sequence: i64,
}

async fn timeline_anchor(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    supplied_attempt: Option<Uuid>,
) -> Result<TimelineAnchor, DbErr> {
    lock_timeline_deployment(db, deployment_id).await?;
    let mut attempt_id = supplied_attempt;
    let mut attempt_number = 0i64;
    if let Some(id) = attempt_id {
        match deployment_attempts::Entity::find_by_id(id)
            .filter(deployment_attempts::Column::DeploymentId.eq(deployment_id))
            .one(db)
            .await?
        {
            Some(attempt) => attempt_number = attempt.attempt_number,
            None => attempt_id = None,
        }
    }
    if attempt_id.is_none() {
        if let Some(attempt) = deployment_attempts::Entity::find()
            .filter(deployment_attempts::Column::DeploymentId.eq(deployment_id))
            .order_by_desc(deployment_attempts::Column::AttemptNumber)
            .one(db)
            .await?
        {
            attempt_id = Some(attempt.id);
            attempt_number = attempt.attempt_number;
        }
    }
    let sequence = next_timeline_sequence(db, deployment_id, attempt_number).await?;
    Ok(TimelineAnchor {
        attempt_id,
        attempt_number,
        sequence,
    })
}

pub struct AttemptTimelineAnchor {
    pub deployment_id: Uuid,
    pub attempt_number: i64,
}

async fn attempt_timeline_anchor(
    db: &impl ConnectionTrait,
    attempt_id: Uuid,
) -> Result<AttemptTimelineAnchor, DbErr> {
    let attempt = deployment_attempts::Entity::find_by_id(attempt_id)
        .one(db)
        .await?
        .ok_or_else(|| {
            DbErr::RecordNotFound(format!("no deployment_attempts row with id {attempt_id}"))
        })?;
    Ok(AttemptTimelineAnchor {
        deployment_id: attempt.deployment_id,
        attempt_number: attempt.attempt_number,
    })
}

pub async fn stage(
    db: &impl ConnectionTrait,
    attempt_id: Uuid,
    stage: &str,
    status: &str,
    message: &str,
) -> Result<(), DbErr> {
    let anchor = attempt_timeline_anchor(db, attempt_id).await?;
    let timeline_sequence =
        next_timeline_sequence(db, anchor.deployment_id, anchor.attempt_number).await?;
    let stage_value = DeploymentStage::try_from_value(&stage.to_string())?;
    let status_value = DeploymentStageStatus::try_from_value(&status.to_string())?;
    // The attempt's next sequence number is `COALESCE(MAX(sequence_number), 0) + 1` over the rows
    // this attempt already has, taken by the insert itself rather than by a separate read.
    let mut insert = Query::insert();
    insert
        .into_table(deployment_stage_events::Entity)
        .columns([
            deployment_stage_events::Column::Id,
            deployment_stage_events::Column::DeploymentAttemptId,
            deployment_stage_events::Column::SequenceNumber,
            deployment_stage_events::Column::TimelineSequence,
            deployment_stage_events::Column::Stage,
            deployment_stage_events::Column::Status,
            deployment_stage_events::Column::Message,
        ])
        .select_from(
            deployment_stage_events::Entity::find()
                .select_only()
                .expr(Expr::val(Uuid::new_v4()))
                .expr(Expr::val(attempt_id))
                .expr(
                    Expr::from(Func::coalesce([
                        Expr::from(Func::max(Expr::col(
                            deployment_stage_events::Column::SequenceNumber,
                        ))),
                        Expr::val(0_i64),
                    ]))
                    .add(1),
                )
                .expr(Expr::val(timeline_sequence))
                .expr(Expr::val(stage_value.to_value()))
                .expr(Expr::val(status_value.to_value()))
                .expr(Expr::val(message))
                .filter(deployment_stage_events::Column::DeploymentAttemptId.eq(attempt_id))
                .into_query(),
        )
        .map_err(|error| DbErr::Custom(error.to_string()))?;
    db.execute(&insert).await?;
    touch_projection(db, anchor.deployment_id).await
}

pub async fn enqueue(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    event_type: &str,
    mode: &str,
) -> Result<(), DbErr> {
    deployment_outbox_events::Entity::insert(deployment_outbox_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        deployment_id: Set(deployment_id),
        event_type: Set(DeploymentOutboxEventType::try_from_value(
            &event_type.to_string(),
        )?),
        payload: Set(serde_json::json!({ "mode": mode })),
        status: Set(DeploymentOutboxStatus::Pending),
        available_at: NotSet,
        claimed_at: NotSet,
        claimed_by: NotSet,
        attempt_count: NotSet,
        delivered_at: NotSet,
        last_error: NotSet,
        created_at: NotSet,
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

fn attempt_id_from_facts(facts: &Value) -> Option<Uuid> {
    facts
        .get("attemptId")?
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
}

/// `actor` is `None` for a system-attributed event, as every worker-side audit call is. Request
/// metadata (`request_id`, `correlation_id`, `graphql_operation`, `source_ip`, `user_agent`) is
/// bound from `crate::audit::context`'s task-local, which is `None` outside a `/graphql` request.
pub async fn audit(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    actor: Option<Uuid>,
    action: &str,
    facts: Value,
) -> Result<(), DbErr> {
    let attempt_id = attempt_id_from_facts(&facts);
    let anchor = timeline_anchor(db, deployment_id, attempt_id).await?;
    let metadata = request_metadata();
    deployment_audit_events::Entity::insert(deployment_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        deployment_id: Set(deployment_id),
        actor_principal_id: Set(actor),
        action: Set(DeploymentAuditAction::try_from_value(&action.to_string())?),
        facts: Set(facts),
        occurred_at: NotSet,
        deployment_attempt_id: Set(anchor.attempt_id),
        attempt_number: Set(Some(anchor.attempt_number)),
        timeline_sequence: Set(Some(anchor.sequence)),
        request_id: Set(metadata.request_id),
        correlation_id: Set(metadata.correlation_id),
        graphql_operation: Set(metadata.graphql_operation),
        source_ip: Set(metadata.source_ip),
        user_agent: Set(metadata.user_agent),
    })
    .exec_without_returning(db)
    .await?;
    touch_projection(db, deployment_id).await
}

/// The audit row every approval-lifecycle statement wrote by hand: no attempt of its own
/// (`deployment_attempt_id` NULL, `attempt_number` 0) and the sequence the zero-attempt counter
/// allocates, where [`audit`] anchors on the deployment's latest attempt instead. Request metadata
/// is bound the same way.
pub async fn system_audit(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    actor: Option<Uuid>,
    action: &str,
    facts: Value,
) -> Result<(), DbErr> {
    let sequence = next_timeline_sequence(db, deployment_id, 0).await?;
    let metadata = request_metadata();
    deployment_audit_events::Entity::insert(deployment_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        deployment_id: Set(deployment_id),
        actor_principal_id: Set(actor),
        action: Set(DeploymentAuditAction::try_from_value(&action.to_string())?),
        facts: Set(facts),
        occurred_at: NotSet,
        deployment_attempt_id: Set(None),
        attempt_number: Set(Some(0)),
        timeline_sequence: Set(Some(sequence)),
        request_id: Set(metadata.request_id),
        correlation_id: Set(metadata.correlation_id),
        graphql_operation: Set(metadata.graphql_operation),
        source_ip: Set(metadata.source_ip),
        user_agent: Set(metadata.user_agent),
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

pub async fn insert_runtime_health(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    deployment_runtime_health::Entity::insert(deployment_runtime_health::ActiveModel {
        deployment_id: Set(deployment_id),
        status: Set(DeploymentRuntimeHealthStatus::NotObserved),
        summary: Set("No local runtime observation is available yet.".to_string()),
        observed_at: NotSet,
        generation: Set(1),
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

/// The deployment and its environment definition version are read first and the projection row is
/// written by key: the deployment row is this transaction's own insert, so nothing else can change
/// it in between.
pub async fn project_deployment_target(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    else {
        return Ok(());
    };
    let Some(environment_id) = deployment.environment_definition_version_id else {
        return Ok(());
    };
    let Some(environment) = environment_definition_versions::Entity::find_by_id(environment_id)
        .one(db)
        .await?
    else {
        return Ok(());
    };
    evaluation_target_projections::Entity::insert(evaluation_target_projections::ActiveModel {
        project_id: Set(deployment.project_id),
        target_kind: Set(EvaluationTargetKind::Deployment),
        target_id: Set(deployment.id),
        agent_version_id: Set(deployment.agent_version_id),
        environment_definition_version_id: Set(environment.id),
        logical_environment_class: Set(environment.logical_environment_class),
        display_name: Set(format!("Deployment {}", deployment.id)),
    })
    .on_conflict(
        OnConflict::columns([
            evaluation_target_projections::Column::TargetKind,
            evaluation_target_projections::Column::TargetId,
            evaluation_target_projections::Column::EnvironmentDefinitionVersionId,
        ])
        .update_columns([
            evaluation_target_projections::Column::ProjectId,
            evaluation_target_projections::Column::AgentVersionId,
            evaluation_target_projections::Column::LogicalEnvironmentClass,
            evaluation_target_projections::Column::DisplayName,
        ])
        .to_owned(),
    )
    .exec_without_returning(db)
    .await?;
    Ok(())
}

/// The canonical text of a JSON document the compiler produced, as the `jsonb` column holds it.
fn json_document(text: &str) -> Value {
    serde_json::from_str(text).expect("a compiled deployment document is always valid JSON")
}

pub async fn insert_deployment(
    db: &impl ConnectionTrait,
    id: Uuid,
    actor: Uuid,
    request: &CompiledRequest,
    key: &str,
    fingerprint: &str,
) -> Result<(), DbErr> {
    let lifecycle = if request.rule.approvers > 0 {
        EntityLifecycleStatus::AwaitingApproval
    } else {
        EntityLifecycleStatus::Requested
    };
    deployments::Entity::insert(deployments::ActiveModel {
        id: Set(id),
        organization_id: Set(request.version.organization_id),
        project_id: Set(request.version.project_id),
        agent_id: Set(request.version.agent_id),
        agent_version_id: Set(request.version.id),
        catalog_release_id: Set(request.version.catalog_release_id.clone()),
        catalog_release_digest: Set(request.version.catalog_release_digest.clone()),
        environment: Set(LogicalEnvironmentClass::try_from_value(
            &request.environment.logical_environment_class,
        )?),
        environment_definition_version_id: Set(Some(request.environment.id)),
        target_digest: Set(request.target_digest.clone()),
        strategy: Set(request.strategy.into()),
        lifecycle_status: Set(lifecycle),
        revision: Set(1),
        idempotency_key: Set(key.trim().to_string()),
        request_fingerprint: Set(Some(fingerprint.to_string())),
        requested_by: Set(actor),
        requested_at: NotSet,
        updated_at: NotSet,
        projection_revision: NotSet,
        project_lifecycle_revision: NotSet,
    })
    .exec_without_returning(db)
    .await?;
    project_deployment_target(db, id).await
}

pub async fn insert_plan(
    db: &impl ConnectionTrait,
    plan_id: Uuid,
    deployment_id: Uuid,
    actor: Uuid,
    request: &CompiledRequest,
) -> Result<(), DbErr> {
    deployment_plan_versions::Entity::insert(deployment_plan_versions::ActiveModel {
        id: Set(plan_id),
        deployment_id: Set(deployment_id),
        version_number: Set(1),
        agent_version_id: Set(request.version.id),
        catalog_release_id: Set(request.version.catalog_release_id.clone()),
        environment: Set(LogicalEnvironmentClass::try_from_value(
            &request.environment.logical_environment_class,
        )?),
        environment_definition_version_id: Set(Some(request.environment.id)),
        agent_content_digest: Set(Some(request.version.content_digest.clone())),
        catalog_release_digest: Set(Some(request.version.catalog_release_digest.clone())),
        target_digest: Set(Some(request.target_digest.clone())),
        compiler_version: Set(hive_application::deployment::compiler::COMPILER_VERSION.to_string()),
        canonical_plan: Set(json_document(&request.canonical_plan)),
        plan_digest: Set(request.plan_digest.clone()),
        package_digest: Set(request.package_digest.clone()),
        package_reference: Set(format!("local://packages/{}", request.package_digest)),
        created_by: Set(actor),
        created_at: NotSet,
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

pub async fn insert_plan_review(
    db: &impl ConnectionTrait,
    plan_id: Uuid,
    review: &hive_application::deployment::CompilerReview,
) -> Result<(), DbErr> {
    deployment_plan_review_facts::Entity::insert(deployment_plan_review_facts::ActiveModel {
        plan_id: Set(plan_id),
        active_agent_version_number: Set(review.active_agent_version_number),
        change_summary: Set(review.change_summary.clone()),
        requested_dependency_versions: Set(serde_json::json!(review.requested_dependency_versions)),
        added_dependency_versions: Set(serde_json::json!(review.added_dependency_versions)),
        removed_dependency_versions: Set(serde_json::json!(review.removed_dependency_versions)),
    })
    .on_conflict(
        OnConflict::column(deployment_plan_review_facts::Column::PlanId)
            .do_nothing()
            .to_owned(),
    )
    .try_insert()
    .exec_without_returning(db)
    .await?;
    Ok(())
}

pub async fn insert_policy_snapshot(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<(), DbErr> {
    // A deployment row that does not exist inserts no snapshot. The read runs in this
    // transaction, which created that row itself.
    if deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
        .is_none()
    {
        return Ok(());
    }
    deployment_policy_snapshots::Entity::insert(deployment_policy_snapshots::ActiveModel {
        deployment_id: Set(deployment_id),
        policy_id: Set(request.policy.id),
        policy_revision: Set(request.policy.revision),
        policy_digest: Set(request.policy.digest.clone()),
        policy_matrix: Set(json_document(&request.policy.matrix)),
        logical_environment_class: Set(LogicalEnvironmentClass::try_from_value(
            &request.environment.logical_environment_class,
        )?),
        risk: Set(request.risk.into()),
        required_evidence: Set(serde_json::json!(request.rule.evidence)),
        required_approvers: Set(request.rule.approvers),
        created_at: NotSet,
        agent_version_id: Set(Some(request.version.id)),
        environment_definition_version_id: Set(Some(request.environment.id)),
        target_digest: Set(Some(request.target_digest.clone())),
        plan_digest: Set(Some(request.plan_digest.clone())),
        package_digest: Set(Some(request.package_digest.clone())),
        binding_digest: Set(Some(request.binding_digest.clone())),
        risk_verification_digest: Set(Some(request.risk_verification_digest.clone())),
        evaluation_requirement_expires_at: Set(request
            .evaluation_requirement_expires_at
            .map(|value| value.fixed_offset())),
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

pub async fn insert_evidence(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<(), DbErr> {
    for kind in &request.rule.evidence {
        // M16 owns evaluation authoring. M14 freezes and validates only an evaluation fact that
        // that owner has already appended for this exact deployment target.
        if kind == "EVALUATION_PASSED" {
            continue;
        }
        deployment_evidence_snapshots::Entity::insert(deployment_evidence_snapshots::ActiveModel {
            id: Set(Uuid::new_v4()),
            deployment_id: Set(deployment_id),
            evidence_kind: Set(DeploymentEvidenceKind::try_from_value(kind)?),
            evidence_digest: Set(hive_application::deployment::compiler::digest(&format!(
                "{}|{}",
                request.binding_digest, kind
            ))),
            expires_at: Set(None),
            created_at: NotSet,
            agent_version_id: Set(Some(request.version.id)),
            environment_definition_version_id: Set(Some(request.environment.id)),
            target_digest: Set(Some(request.target_digest.clone())),
            plan_digest: Set(Some(request.plan_digest.clone())),
            package_digest: Set(Some(request.package_digest.clone())),
            binding_digest: Set(Some(request.binding_digest.clone())),
            source_evaluation_run_id: Set(None),
        })
        .exec_without_returning(db)
        .await?;
    }
    Ok(())
}

/// Inserts the one frozen approval requirement for this idempotent deployment cycle. The
/// deployment row supplies the organization, the project and the requested-at instant; the expiry
/// is that instant plus 24 hours.
pub async fn insert_approval_requirement(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<Uuid, DbErr> {
    if let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    {
        deployment_approval_requirements::Entity::insert(
            deployment_approval_requirements::ActiveModel {
                id: Set(Uuid::new_v4()),
                deployment_id: Set(deployment.id),
                revision: Set(1),
                organization_id: Set(deployment.organization_id),
                project_id: Set(deployment.project_id),
                requested_at: Set(deployment.requested_at),
                required_approvers: Set(request.rule.approvers),
                status: Set(crate::entity::enums::ApprovalRequirementStatus::Pending),
                expires_at: Set(deployment.requested_at + chrono::Duration::hours(24)),
                satisfied_at: Set(None),
                rejected_at: NotSet,
                invalidated_at: NotSet,
                invalidation_code: NotSet,
                satisfied_participants: NotSet,
                created_at: NotSet,
            },
        )
        .on_conflict(
            OnConflict::column(deployment_approval_requirements::Column::DeploymentId)
                .do_nothing()
                .to_owned(),
        )
        .try_insert()
        .exec_without_returning(db)
        .await?;
    }
    deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .select_only()
        .column(deployment_approval_requirements::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .ok_or_else(|| {
            DbErr::RecordNotFound(format!(
                "no deployment_approval_requirements row for deployment {deployment_id}"
            ))
        })
}

/// Terminalizes every still-`QUEUED`/`RUNNING` attempt for a deployment being force-terminated
/// (cancel, or the worker's dead-letter path), staging a matching timeline event for each.
pub async fn terminalize_running_attempts(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    status: &str,
    code: &str,
    summary: &str,
    stage_name: &str,
    stage_status: &str,
) -> Result<Vec<Uuid>, DbErr> {
    let terminal = DeploymentAttemptStatus::try_from_value(&status.to_string())?;
    let updated = deployment_attempts::Entity::update_many()
        .col_expr(
            deployment_attempts::Column::Status,
            Expr::val(terminal.to_value()),
        )
        .col_expr(
            deployment_attempts::Column::CompletedAt,
            Expr::current_timestamp(),
        )
        .col_expr(
            deployment_attempts::Column::Generation,
            Expr::col(deployment_attempts::Column::Generation).add(1),
        )
        .col_expr(deployment_attempts::Column::FailureCode, Expr::val(code))
        .col_expr(
            deployment_attempts::Column::FailureSummary,
            Expr::val(summary),
        )
        .filter(deployment_attempts::Column::DeploymentId.eq(deployment_id))
        .filter(deployment_attempts::Column::Status.is_in([
            DeploymentAttemptStatus::Queued,
            DeploymentAttemptStatus::Running,
        ]))
        .exec_with_returning(db)
        .await?;
    let ids: Vec<Uuid> = updated.into_iter().map(|attempt| attempt.id).collect();
    for attempt_id in &ids {
        stage(db, *attempt_id, stage_name, stage_status, summary).await?;
    }
    Ok(ids)
}

#[cfg(test)]
mod evidence_state_tests {
    use super::{by_id, evidence_state, required, review_text, sql_eq, string_list};
    use crate::entity::enums::{DeploymentEvidenceKind, EvidenceInvalidationKind};
    use crate::entity::enums::{DeploymentRisk, LogicalEnvironmentClass};
    use crate::entity::{
        deployment_evidence_invalidations, deployment_evidence_snapshots,
        deployment_policy_snapshots,
    };
    use hive_application::deployment::ApprovalEvidenceState;
    use sea_orm::prelude::DateTimeWithTimeZone;
    use sea_orm::DbErr;
    use serde_json::json;
    use uuid::{uuid, Uuid};

    const DEPLOYMENT: Uuid = uuid!("11111111-1111-1111-1111-111111111111");
    const AGENT_VERSION: Uuid = uuid!("22222222-2222-2222-2222-222222222222");
    const SNAPSHOT: Uuid = uuid!("44444444-4444-4444-4444-444444444444");

    fn instant(text: &str) -> DateTimeWithTimeZone {
        chrono::DateTime::parse_from_rfc3339(text).expect("an RFC 3339 instant")
    }

    fn now() -> DateTimeWithTimeZone {
        instant("2026-06-01T12:00:00Z")
    }

    fn policy() -> deployment_policy_snapshots::Model {
        deployment_policy_snapshots::Model {
            policy_id: Uuid::nil(),
            policy_revision: 1,
            policy_digest: "p".repeat(64),
            policy_matrix: json!({}),
            logical_environment_class: LogicalEnvironmentClass::Production,
            risk: DeploymentRisk::High,
            required_evidence: json!(["PLAN_VALIDATED"]),
            required_approvers: 2,
            created_at: instant("2026-01-01T00:00:00Z"),
            agent_version_id: Some(AGENT_VERSION),
            environment_definition_version_id: None,
            target_digest: Some("t".repeat(64)),
            plan_digest: Some("d".repeat(64)),
            package_digest: Some("k".repeat(64)),
            binding_digest: Some("b".repeat(64)),
            evaluation_requirement_expires_at: None,
            risk_verification_digest: None,
            deployment_id: DEPLOYMENT,
        }
    }

    /// A snapshot frozen against the same cycle as `policy`, except that its environment
    /// definition is absent on both sides — the `NULL`-is-never-equal case `sql_eq` exists for.
    fn matching_snapshot() -> deployment_evidence_snapshots::Model {
        let policy = policy();
        deployment_evidence_snapshots::Model {
            deployment_id: DEPLOYMENT,
            evidence_kind: DeploymentEvidenceKind::PlanValidated,
            evidence_digest: "e".repeat(64),
            expires_at: None,
            created_at: instant("2026-01-01T00:00:00Z"),
            agent_version_id: policy.agent_version_id,
            environment_definition_version_id: policy.environment_definition_version_id,
            target_digest: policy.target_digest,
            plan_digest: policy.plan_digest,
            package_digest: policy.package_digest,
            binding_digest: policy.binding_digest,
            source_evaluation_run_id: None,
            id: SNAPSHOT,
        }
    }

    fn invalidation(kind: EvidenceInvalidationKind) -> deployment_evidence_invalidations::Model {
        deployment_evidence_invalidations::Model {
            evidence_snapshot_id: SNAPSHOT,
            kind,
            occurred_at: instant("2026-05-01T00:00:00Z"),
            id: Uuid::nil(),
        }
    }

    /// Both environment definitions are absent, which SQL never calls equal, so the otherwise
    /// identical snapshot is a mismatch rather than valid.
    #[test]
    fn two_absent_digests_are_a_mismatch_not_a_match() {
        assert_eq!(
            evidence_state(&matching_snapshot(), &policy(), &[], now()),
            ApprovalEvidenceState::Mismatch
        );
    }

    #[test]
    fn a_snapshot_frozen_against_the_whole_cycle_is_valid() {
        let mut policy = policy();
        policy.environment_definition_version_id = Some(AGENT_VERSION);
        let mut snapshot = matching_snapshot();
        snapshot.environment_definition_version_id = Some(AGENT_VERSION);
        assert_eq!(
            evidence_state(&snapshot, &policy, &[], now()),
            ApprovalEvidenceState::Valid
        );
    }

    /// The four states are answered in one order, so an invalidated snapshot reports why it was
    /// invalidated rather than that it also expired, and `FAILED` outranks `REVOKED`.
    #[test]
    fn the_state_order_is_failed_then_revoked_then_expired_then_the_digests() {
        let mut snapshot = matching_snapshot();
        snapshot.expires_at = Some(instant("2026-01-02T00:00:00Z"));
        let revoked = [invalidation(EvidenceInvalidationKind::Revoked)];
        let failed = [invalidation(EvidenceInvalidationKind::Failed)];
        let both = [
            invalidation(EvidenceInvalidationKind::Revoked),
            invalidation(EvidenceInvalidationKind::Failed),
        ];
        assert_eq!(
            evidence_state(&snapshot, &policy(), &failed, now()),
            ApprovalEvidenceState::Failed
        );
        assert_eq!(
            evidence_state(&snapshot, &policy(), &both, now()),
            ApprovalEvidenceState::Failed
        );
        assert_eq!(
            evidence_state(&snapshot, &policy(), &revoked, now()),
            ApprovalEvidenceState::Revoked
        );
        assert_eq!(
            evidence_state(&snapshot, &policy(), &[], now()),
            ApprovalEvidenceState::Expired
        );
    }

    /// An invalidation of another snapshot is not this snapshot's.
    #[test]
    fn an_invalidation_of_another_snapshot_does_not_apply() {
        let mut other = invalidation(EvidenceInvalidationKind::Failed);
        other.evidence_snapshot_id = DEPLOYMENT;
        assert_eq!(
            evidence_state(&matching_snapshot(), &policy(), &[other], now()),
            ApprovalEvidenceState::Mismatch
        );
    }

    #[test]
    fn expiry_at_exactly_the_instant_is_expired() {
        let mut snapshot = matching_snapshot();
        snapshot.expires_at = Some(now());
        assert_eq!(
            evidence_state(&snapshot, &policy(), &[], now()),
            ApprovalEvidenceState::Expired
        );
        snapshot.expires_at = Some(instant("2026-06-01T12:00:01Z"));
        assert_eq!(
            evidence_state(&snapshot, &policy(), &[], now()),
            ApprovalEvidenceState::Mismatch
        );
    }

    /// Only the four review codes survive to the wire; free text a predecessor row may hold is
    /// dropped rather than shown.
    #[test]
    fn review_text_keeps_only_the_four_review_codes() {
        for code in [
            "REVIEWED_CHANGE_SCOPE",
            "AUTHORIZATION_GRANTED",
            "UNACCEPTABLE_CHANGE_SCOPE",
            "CHANGE_SCOPE_NOT_APPROVED",
        ] {
            assert_eq!(review_text(Some(code.to_string())), Some(code.to_string()));
            assert_eq!(
                review_text(Some(format!("  {code}  "))),
                Some(code.to_string()),
                "{code} is recognized after trimming"
            );
        }
        assert_eq!(review_text(None), None);
        assert_eq!(review_text(Some(String::new())), None);
        assert_eq!(review_text(Some("Looks fine to me.".to_string())), None);
        assert_eq!(review_text(Some("reviewed_change_scope".to_string())), None);
    }

    #[test]
    fn sql_eq_is_true_only_when_both_sides_are_present_and_equal() {
        assert!(sql_eq(&Some("a"), &Some("a")));
        assert!(!sql_eq(&Some("a"), &Some("b")));
        assert!(!sql_eq(&Some("a"), &None));
        assert!(!sql_eq::<&str>(&None, &None));
    }

    #[test]
    fn string_list_reads_a_jsonb_array_of_strings_and_nothing_else() {
        assert_eq!(string_list(&json!(["a", "b"])), vec!["a", "b"]);
        assert_eq!(string_list(&json!(["a", 1, null])), vec!["a"]);
        assert!(string_list(&json!({})).is_empty());
        assert!(string_list(&json!(null)).is_empty());
    }

    #[test]
    fn a_null_in_a_column_that_must_be_present_names_the_column() {
        assert!(matches!(required(Some(1), "plan_digest"), Ok(1)));
        let error = required::<i32>(None, "plan_digest").expect_err("a null is an error");
        assert!(
            matches!(&error, DbErr::Type(message) if message.contains("plan_digest")),
            "{error:?}"
        );
    }

    /// The index is built from rows already read, so a duplicate key is the caller's error to
    /// avoid; the last row wins and the map never grows a second entry.
    #[test]
    fn indexing_rows_by_key_keeps_the_last_of_a_duplicate() {
        let index = by_id(vec![(1, "first"), (2, "other"), (1, "last")], |row| row.0);
        assert_eq!(index.len(), 2);
        assert_eq!(index[&1].1, "last");
        assert!(by_id(Vec::<(i32, &str)>::new(), |row| row.0).is_empty());
    }
}
