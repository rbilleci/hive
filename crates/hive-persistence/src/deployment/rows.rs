//! Row-mapping helpers shared by `queries`/`mutations`/`worker`: the batched `Deployment` loader,
//! built on SeaORM entities, and the approval-requirement mappers the approval surface still
//! reads through its own statements.

use crate::entity::enums::{
    DeploymentLifecycleStatus as EntityLifecycleStatus, EvidenceInvalidationKind,
};
use crate::entity::{
    agent_versions, agents, deployment_approval_decisions, deployment_approval_requirements,
    deployment_attempts, deployment_evidence_invalidations, deployment_evidence_snapshots,
    deployment_plan_review_facts, deployment_plan_versions, deployment_policy_snapshots,
    deployment_runtime_health, deployments, environment_definition_versions,
};
use chrono::{DateTime, Utc};
use hive_application::deployment::{
    Deployment, DeploymentAttempt, DeploymentEnvironment, DeploymentEvidence, DeploymentPlan,
    DeploymentPlanReview, DeploymentPolicy, DeploymentRollbackTarget, DeploymentRuntimeHealth,
};
use hive_domain::deployment::{ApprovalRequirementStatus, DeploymentLifecycleStatus};
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::sea_query::{Expr, ExprTrait, IntoTableRef, LockType};
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, FromQueryResult,
    JoinType, QueryFilter, QueryOrder, QuerySelect, RelationTrait,
};
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
            status: requirement_status(row.status.to_value()),
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
) -> String {
    if invalidations.iter().any(|row| {
        row.evidence_snapshot_id == evidence.id && row.kind == EvidenceInvalidationKind::Failed
    }) {
        return "FAILED".to_string();
    }
    if invalidations.iter().any(|row| {
        row.evidence_snapshot_id == evidence.id && row.kind == EvidenceInvalidationKind::Revoked
    }) {
        return "REVOKED".to_string();
    }
    if evidence
        .expires_at
        .is_some_and(|expires_at| expires_at <= now)
    {
        return "EXPIRED".to_string();
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
        "VALID".to_string()
    } else {
        "MISMATCH".to_string()
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
            strategy: row.strategy.to_value(),
            lifecycle_status: lifecycle_status(row.lifecycle_status.to_value()),
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
                risk: policy.risk.to_value(),
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
                status: attempt.status.to_value(),
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

/// Parses `deployments.lifecycle_status` once at the row boundary. The column's CHECK constraint
/// admits only the values `DeploymentLifecycleStatus` names, so an unrecognized value is schema
/// drift and panics.
pub fn lifecycle_status(value: String) -> DeploymentLifecycleStatus {
    value.parse().unwrap_or_else(|error| panic!("{error}"))
}

/// Parses `deployment_approval_requirements.status` once at the row boundary; an unrecognized
/// value is schema drift against the column's CHECK constraint and panics.
pub fn requirement_status(value: String) -> ApprovalRequirementStatus {
    value.parse().unwrap_or_else(|error| panic!("{error}"))
}
