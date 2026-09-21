//! Computed fields (`docs/idiomatic-seaography-plan.md`, A4) of the generated deployment objects.
//! Each is derived from the row it is on, plus rows loaded through SeaORM. `hive-api` attaches
//! them to the generated objects.
//!
//! These replace the nested structures the deleted `Deployment` wire type carried, which the
//! deleted `DEPLOYMENT_COLUMNS`/`DEPLOYMENT_FROM` statement produced with a seven-table join, two
//! `LEFT JOIN LATERAL ... LIMIT 1` subqueries and a correlated `jsonb_agg(... ORDER BY ...)`:
//!
//! - `Deployments.plan`: the frozen plan, the one `deployment_plan_versions` row with
//!   `version_number = 1` the join pinned.
//! - `Deployments.currentAttempt`: the newest execution attempt (the first lateral).
//! - `Deployments.rollbackTarget`: the prior active target of the same agent and environment (the
//!   second lateral), answered as the deployment row itself, so its own relations and computed
//!   fields carry the version number, plan digest and runtime health the lateral projected.
//! - `Deployments.timeline`: the deployment's audit events and its attempts' stage events in one
//!   ordered list, with the audit action's fixed status/message/source vocabulary.
//! - `DeploymentPlanVersions.review`: the retained plan review facts, with the placeholder values
//!   the deleted statement's `COALESCE`s supplied for a plan that has none.
//! - `DeploymentEvidenceSnapshots.state`: the evidence state, which depends on the deployment's
//!   frozen policy, on recorded invalidations and on the clock, so it is not a stored value.
//!
//! The approval surface adds the values the deleted `ApprovalInboxItem`/`ApprovalRequirement` wire
//! types carried next to the stored requirement row:
//!
//! - `DeploymentApprovalRequirements.status`: the stored status, with an elapsed `PENDING` expiry
//!   projected to `EXPIRED` before the maintenance tick writes it — the projection the deleted
//!   inbox applied with a `CASE` over `clock_timestamp()`, taken on the service clock.
//! - `qualifyingApprovalCount`, `requester`, `satisfiedParticipants`, `eligible` and
//!   `decisionAvailable`: the per-requester and per-actor authority facts the deleted item
//!   computed, each on `capability::deployment_approval_capabilities`.
//! - `approvalSnapshot`: the frozen policy, target and evidence facts, assembled from the
//!   deployment's own plan, policy snapshot, environment definition version and evidence rows.
//! - `DeploymentApprovalDecisions.comment` / `rejectionReason`: the four-code review-text
//!   normalization, over columns the generated API withholds.
//!
//! The parent row is already tenant-filtered by the `entity_filter` hook before any of these runs.

#![allow(non_snake_case)] // a computed field is named after its method

use crate::capability::{deployment_approval_capabilities, DEPLOYMENT_APPROVAL_DECIDE};
use crate::console::requester;
use crate::entity::enums::{
    ApprovalDecision, ApprovalRequirementStatus, DeploymentAuditAction, EvidenceInvalidationKind,
    LifecycleStatus,
};
use crate::entity::{
    deployment_approval_decisions, deployment_approval_requirements, deployment_attempts,
    deployment_audit_events, deployment_evidence_invalidations, deployment_evidence_snapshots,
    deployment_plan_review_facts, deployment_plan_versions, deployment_policy_snapshots,
    deployment_stage_events, deployments, environment_definition_versions, principals, projects,
};
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, JoinType, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait,
};
use std::collections::HashMap;
// `#[CustomFields]` and `CustomOutputType` expand to paths that start with `async_graphql::`.
use seaography::async_graphql::{self, Context};
use seaography::{CustomFields, CustomOutputType};
use uuid::Uuid;

/// The largest timeline page a single request may ask for.
const TIMELINE_LIMIT: i32 = 200;

/// One entry of a deployment's ordered history: an audit event of the deployment, or a stage event
/// of one of its attempts.
#[derive(CustomOutputType, Clone)]
pub struct DeploymentTimelineEvent {
    pub id: Uuid,
    pub attemptId: Option<Uuid>,
    pub attemptNumber: i64,
    pub sequence: i64,
    pub stage: String,
    pub status: String,
    pub message: String,
    pub source: String,
    pub occurredAt: DateTimeWithTimeZone,
}

/// The retained review facts of a frozen plan.
#[derive(CustomOutputType, Clone)]
pub struct DeploymentPlanReview {
    pub activeAgentVersionNumber: Option<i64>,
    pub changeSummary: String,
    pub addedDependencyVersions: Vec<String>,
    pub removedDependencyVersions: Vec<String>,
}

/// The sentence the deleted statement's `COALESCE` supplied for a plan with no retained facts.
const REVIEW_UNAVAILABLE: &str = "Retained plan review facts are unavailable.";

fn string_list(value: &serde_json::Value) -> Vec<String> {
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

/// SQL equality, where a comparison with `NULL` is never true. The evidence state test compares
/// six nullable digests and identifiers, and Rust's own `==` would call two absent values equal.
fn sql_eq<T: PartialEq>(left: &Option<T>, right: &Option<T>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left == right)
}

/// The frozen plan of `deployment_id`: the join pinned `version_number = 1`.
async fn frozen_plan(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<deployment_plan_versions::Model>, DbErr> {
    deployment_plan_versions::Entity::find()
        .filter(deployment_plan_versions::Column::DeploymentId.eq(deployment_id))
        .filter(deployment_plan_versions::Column::VersionNumber.eq(1_i64))
        .one(db)
        .await
}

/// Ports the audit action's status vocabulary (the deleted `CASE WHEN audit.action IN (...)`).
fn audit_status(action: DeploymentAuditAction) -> &'static str {
    use DeploymentAuditAction::*;
    match action {
        Canceled | ApprovalRejected | ApprovalExecutionBlocked => "CANCELED",
        ExecutionFailed | OutboxDeadLettered | ApprovalExpired | ApprovalInvalidated => "FAILED",
        ApprovalReplayed
        | OutboxDeliveryRetried
        | RetryRecorded
        | PromotionRecorded
        | RollbackRecorded => "RECORDED",
        _ => "SUCCEEDED",
    }
}

/// Ports the audit action's message vocabulary (the deleted `CASE audit.action WHEN ...`).
fn audit_message(action: DeploymentAuditAction) -> &'static str {
    use DeploymentAuditAction::*;
    match action {
        Requested => "A user recorded this deployment request.",
        Canceled => "A user canceled this deployment.",
        OutboxDeadLettered => "The local worker isolated an event.",
        OutboxLeaseReclaimed => "The local worker reclaimed an expired lease.",
        OutboxDeliveryRetried => "The local worker recorded a bounded database-delivery retry.",
        ExecutionStarted => "The local worker started execution.",
        ExecutionFailed => "The local worker recorded a sanitized failure.",
        ApprovalRecorded => "An eligible approver recorded an immutable decision.",
        ApprovalReplayed => "The service returned the actor's immutable decision for this request.",
        ApprovalSatisfied => "The frozen approval requirement was satisfied.",
        ApprovalRejected => "An eligible approver rejected this deployment request.",
        ApprovalExpired => "The approval requirement expired before satisfaction.",
        ApprovalInvalidated => "The approval requirement no longer matched its frozen evidence.",
        ApprovalExecutionBlocked => "The local worker stopped execution because frozen approval requirements were no longer executable.",
        RetryRecorded => "An authorized operator recorded a new recovery deployment cycle.",
        PromotionRecorded => "An authorized operator recorded a promotion for the observed healthy target.",
        RollbackRecorded => "An authorized operator recorded a rollback deployment cycle.",
        _ => "The local worker recorded successful execution.",
    }
}

/// Ports the audit event's source vocabulary: every `APPROVAL_*` action is the service's own, an
/// action with no actor is the worker's, and everything else is a user's.
fn audit_source(action: DeploymentAuditAction, actor: Option<Uuid>) -> &'static str {
    if action.to_value().starts_with("APPROVAL_") {
        "SERVICE"
    } else if actor.is_none() {
        "WORKER"
    } else {
        "USER"
    }
}

#[CustomFields]
impl deployments::Model {
    /// The frozen plan of this deployment.
    pub async fn plan(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<deployment_plan_versions::Model>> {
        let (_, db) = requester(ctx)?;
        Ok(frozen_plan(db, self.id).await?)
    }

    /// The newest execution attempt, or `null` before the first one.
    pub async fn currentAttempt(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<deployment_attempts::Model>> {
        let (_, db) = requester(ctx)?;
        Ok(deployment_attempts::Entity::find()
            .filter(deployment_attempts::Column::DeploymentId.eq(self.id))
            .order_by_desc(deployment_attempts::Column::AttemptNumber)
            .one(db)
            .await?)
    }

    /// The prior active deployment of the same agent and environment that a rollback would return
    /// to: the newest one requested before this one. It keeps the deleted lateral's three inner
    /// joins, so a candidate without a published version, a frozen plan or observed runtime health
    /// is passed over exactly as it was.
    pub async fn rollbackTarget(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<deployments::Model>> {
        let (_, db) = requester(ctx)?;
        let Some(environment_id) = self.environment_definition_version_id else {
            return Ok(None);
        };
        // `(candidate.requested_at, candidate.id) < (deployment.requested_at, deployment.id)`.
        let before = Condition::any()
            .add(deployments::Column::RequestedAt.lt(self.requested_at))
            .add(
                Condition::all()
                    .add(deployments::Column::RequestedAt.eq(self.requested_at))
                    .add(deployments::Column::Id.lt(self.id)),
            );
        Ok(deployments::Entity::find()
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
            .filter(deployments::Column::ProjectId.eq(self.project_id))
            .filter(deployments::Column::AgentId.eq(self.agent_id))
            .filter(deployments::Column::EnvironmentDefinitionVersionId.eq(environment_id))
            .filter(
                deployments::Column::LifecycleStatus
                    .eq(crate::entity::enums::DeploymentLifecycleStatus::Active),
            )
            .filter(deployments::Column::Id.ne(self.id))
            .filter(before)
            .order_by_desc(deployments::Column::RequestedAt)
            .order_by_desc(deployments::Column::Id)
            .one(db)
            .await?)
    }

    /// This deployment's history: its own audit events and its attempts' stage events, in one
    /// list ordered by attempt, then by timeline sequence, then by identifier. `first` bounds the
    /// list; there is no cursor page, as there is none for the evaluation candidate targets.
    pub async fn timeline(
        &self,
        ctx: &Context<'_>,
        first: i64,
    ) -> async_graphql::Result<Vec<DeploymentTimelineEvent>> {
        let (_, db) = requester(ctx)?;
        let limit = first.clamp(0, i64::from(TIMELINE_LIMIT)) as usize;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut events: Vec<DeploymentTimelineEvent> = Vec::new();
        for audit in deployment_audit_events::Entity::find()
            .filter(deployment_audit_events::Column::DeploymentId.eq(self.id))
            .all(db)
            .await?
        {
            events.push(DeploymentTimelineEvent {
                id: audit.id,
                attemptId: audit.deployment_attempt_id,
                attemptNumber: audit.attempt_number.unwrap_or_default(),
                sequence: audit.timeline_sequence.unwrap_or_default(),
                stage: audit.action.to_value(),
                status: audit_status(audit.action).to_string(),
                message: audit_message(audit.action).to_string(),
                source: audit_source(audit.action, audit.actor_principal_id).to_string(),
                occurredAt: audit.occurred_at,
            });
        }
        let attempts: Vec<deployment_attempts::Model> = deployment_attempts::Entity::find()
            .filter(deployment_attempts::Column::DeploymentId.eq(self.id))
            .all(db)
            .await?;
        for attempt in &attempts {
            for stage in deployment_stage_events::Entity::find()
                .filter(deployment_stage_events::Column::DeploymentAttemptId.eq(attempt.id))
                .all(db)
                .await?
            {
                events.push(DeploymentTimelineEvent {
                    id: stage.id,
                    attemptId: Some(attempt.id),
                    attemptNumber: attempt.attempt_number,
                    sequence: stage.timeline_sequence.unwrap_or_default(),
                    stage: stage.stage.to_value(),
                    status: stage.status.to_value(),
                    message: stage.message,
                    source: "WORKER".to_string(),
                    occurredAt: stage.occurred_at,
                });
            }
        }
        events.sort_by(|left, right| {
            (left.attemptNumber, left.sequence, left.id).cmp(&(
                right.attemptNumber,
                right.sequence,
                right.id,
            ))
        });
        events.truncate(limit);
        Ok(events)
    }
}

#[CustomFields]
impl deployment_plan_versions::Model {
    /// The retained review facts of this plan. A plan that has none answers the placeholder values
    /// the deleted statement's `COALESCE`s supplied.
    pub async fn review(&self, ctx: &Context<'_>) -> async_graphql::Result<DeploymentPlanReview> {
        let (_, db) = requester(ctx)?;
        let facts = deployment_plan_review_facts::Entity::find_by_id(self.id)
            .one(db)
            .await?;
        Ok(match facts {
            Some(facts) => DeploymentPlanReview {
                activeAgentVersionNumber: facts.active_agent_version_number,
                changeSummary: facts.change_summary,
                addedDependencyVersions: string_list(&facts.added_dependency_versions),
                removedDependencyVersions: string_list(&facts.removed_dependency_versions),
            },
            None => DeploymentPlanReview {
                activeAgentVersionNumber: None,
                changeSummary: REVIEW_UNAVAILABLE.to_string(),
                addedDependencyVersions: Vec::new(),
                removedDependencyVersions: Vec::new(),
            },
        })
    }
}

#[CustomFields]
impl deployment_evidence_snapshots::Model {
    /// This evidence's state against the deployment's frozen policy: `FAILED` or `REVOKED` when an
    /// invalidation was recorded, `EXPIRED` once its expiry has passed, `VALID` while every frozen
    /// binding still matches, and `MISMATCH` otherwise. The expiry is decided on the service clock,
    /// where the deleted statement used `clock_timestamp()`; both are the wall clock, and the value
    /// is a read, not a guard on a write.
    pub async fn state(&self, ctx: &Context<'_>) -> async_graphql::Result<String> {
        let (_, db) = requester(ctx)?;
        let invalidations = deployment_evidence_invalidations::Entity::find()
            .filter(deployment_evidence_invalidations::Column::EvidenceSnapshotId.eq(self.id))
            .all(db)
            .await?;
        if invalidations
            .iter()
            .any(|row| row.kind == EvidenceInvalidationKind::Failed)
        {
            return Ok("FAILED".to_string());
        }
        if invalidations
            .iter()
            .any(|row| row.kind == EvidenceInvalidationKind::Revoked)
        {
            return Ok("REVOKED".to_string());
        }
        let now = chrono::Utc::now().fixed_offset();
        if self.expires_at.is_some_and(|expires_at| expires_at <= now) {
            return Ok("EXPIRED".to_string());
        }
        let Some(policy) = deployment_policy_snapshots::Entity::find_by_id(self.deployment_id)
            .one(db)
            .await?
        else {
            return Ok("MISMATCH".to_string());
        };
        let matches = sql_eq(&self.binding_digest, &policy.binding_digest)
            && sql_eq(&self.agent_version_id, &policy.agent_version_id)
            && sql_eq(
                &self.environment_definition_version_id,
                &policy.environment_definition_version_id,
            )
            && sql_eq(&self.target_digest, &policy.target_digest)
            && sql_eq(&self.plan_digest, &policy.plan_digest)
            && sql_eq(&self.package_digest, &policy.package_digest);
        Ok(match matches {
            true => "VALID".to_string(),
            false => "MISMATCH".to_string(),
        })
    }
}

// --- the approval surface ----------------------------------------------------------------------

/// The frozen rule of an approval requirement's policy cell.
#[derive(CustomOutputType, Clone)]
pub struct ProjectApprovalPolicyRule {
    pub requiredEvidence: Vec<String>,
    pub requiredDistinctApproverCount: i32,
}

/// The frozen target an approval requirement was recorded against.
#[derive(CustomOutputType, Clone)]
pub struct ApprovalTargetSnapshot {
    pub agentVersionId: Uuid,
    pub agentVersionDigest: String,
    pub environmentDefinitionVersionId: Option<Uuid>,
    pub environmentDefinitionDigest: String,
    pub targetDigest: String,
    pub deploymentPlanDigest: String,
    pub artifactDigest: String,
}

/// One required evidence kind and the state of the snapshot that satisfies it. This is not the
/// generated `DeploymentEvidenceSnapshots` object: it lists the kinds the frozen policy *requires*,
/// so a kind with no stored snapshot is listed as `MISSING`.
#[derive(CustomOutputType, Clone)]
pub struct DeploymentEvidenceSnapshot {
    pub kind: String,
    pub digest: Option<String>,
    pub bindingDigest: Option<String>,
    pub expiresAt: Option<DateTimeWithTimeZone>,
    pub state: String,
}

/// The frozen policy, rule, target and evidence facts of one approval requirement.
#[derive(CustomOutputType, Clone)]
pub struct DeploymentApprovalSnapshot {
    pub policyDigest: String,
    pub policyRevision: i64,
    pub environmentClass: String,
    pub risk: String,
    pub riskLevel: String,
    pub rule: ProjectApprovalPolicyRule,
    pub target: ApprovalTargetSnapshot,
    pub evidence: Vec<DeploymentEvidenceSnapshot>,
    pub expiresAt: DateTimeWithTimeZone,
}

/// The requirement's own deployment row.
async fn requirement_deployment(
    db: &impl ConnectionTrait,
    requirement: &deployment_approval_requirements::Model,
) -> Result<Option<deployments::Model>, DbErr> {
    deployments::Entity::find_by_id(requirement.deployment_id)
        .one(db)
        .await
}

/// Whether the requirement's project is active, the second half of "an eligible approver" and the
/// `JOIN projects ... lifecycle_status = 'ACTIVE'` the qualifying-approval count applied.
async fn project_active(db: &impl ConnectionTrait, project_id: Uuid) -> Result<bool, DbErr> {
    Ok(projects::Entity::find_by_id(project_id)
        .filter(projects::Column::LifecycleStatus.eq(LifecycleStatus::Active))
        .select_only()
        .column(projects::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .is_some())
}

/// Ports `eligibleApprover`: `DEPLOYMENT_APPROVAL.DECIDE` at the project, and an active project.
async fn eligible_approver(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<bool, DbErr> {
    Ok(
        deployment_approval_capabilities(db, principal_id, project_id, false)
            .await?
            .contains(DEPLOYMENT_APPROVAL_DECIDE)
            && project_active(db, project_id).await?,
    )
}

/// The stored identifiers of `satisfied_participants`, in the order the column holds them.
fn participant_ids(value: &serde_json::Value) -> Vec<Uuid> {
    string_list(value)
        .into_iter()
        .filter_map(|value| Uuid::parse_str(&value).ok())
        .collect()
}

/// The evidence list of one deployment: every kind its frozen policy requires, in kind order,
/// each with the state of the snapshot that satisfies it. Ports `approvalEvidenceFor`.
async fn approval_evidence(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Vec<DeploymentEvidenceSnapshot>, DbErr> {
    let Some(policy) = deployment_policy_snapshots::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    else {
        return Ok(Vec::new());
    };
    let snapshots = deployment_evidence_snapshots::Entity::find()
        .filter(deployment_evidence_snapshots::Column::DeploymentId.eq(deployment_id))
        .order_by_asc(deployment_evidence_snapshots::Column::Id)
        .all(db)
        .await?;
    let invalidations = if snapshots.is_empty() {
        Vec::new()
    } else {
        deployment_evidence_invalidations::Entity::find()
            .filter(
                deployment_evidence_invalidations::Column::EvidenceSnapshotId
                    .is_in(snapshots.iter().map(|row| row.id).collect::<Vec<_>>()),
            )
            .all(db)
            .await?
    };
    let now: DateTimeWithTimeZone = chrono::Utc::now().fixed_offset();
    let mut kinds = string_list(&policy.required_evidence);
    kinds.sort();
    let mut listed = Vec::new();
    for kind in kinds {
        let matching: Vec<&deployment_evidence_snapshots::Model> = snapshots
            .iter()
            .filter(|snapshot| snapshot.evidence_kind.to_value() == kind)
            .collect();
        if matching.is_empty() {
            listed.push(DeploymentEvidenceSnapshot {
                kind,
                digest: None,
                bindingDigest: None,
                expiresAt: None,
                state: "MISSING".to_string(),
            });
            continue;
        }
        for snapshot in matching {
            listed.push(DeploymentEvidenceSnapshot {
                kind: kind.clone(),
                digest: Some(snapshot.evidence_digest.clone()),
                bindingDigest: snapshot.binding_digest.clone(),
                expiresAt: snapshot.expires_at,
                state: super::rows::evidence_state(snapshot, &policy, &invalidations, now),
            });
        }
    }
    Ok(listed)
}

#[CustomFields]
impl deployment_approval_requirements::Model {
    /// This requirement's status, with an elapsed `PENDING` expiry projected to `EXPIRED`. The
    /// stored column is only ever rewritten by a command or by the maintenance tick; the deleted
    /// inbox projected the same value on the clock, and so does this. Reading stores nothing.
    pub async fn status(&self, _ctx: &Context<'_>) -> async_graphql::Result<String> {
        Ok(self.projected_status().to_value())
    }

    /// The distinct principals whose `APPROVE` decision counts towards this requirement: an
    /// approver other than the requester who still holds `DEPLOYMENT_APPROVAL.DECIDE` on an active
    /// project. A satisfied requirement's participant list is frozen, so it answers its own length.
    pub async fn qualifyingApprovalCount(&self, ctx: &Context<'_>) -> async_graphql::Result<i32> {
        let (_, db) = requester(ctx)?;
        if self.projected_status() == ApprovalRequirementStatus::Satisfied {
            return Ok(participant_ids(&self.satisfied_participants).len() as i32);
        }
        if !project_active(db, self.project_id).await? {
            return Ok(0);
        }
        let Some(deployment) = requirement_deployment(db, self).await? else {
            return Ok(0);
        };
        let mut qualified = 0;
        let mut cache: HashMap<Uuid, bool> = HashMap::new();
        for actor in approving_actors(db, self.id).await? {
            if actor == deployment.requested_by {
                continue;
            }
            let held = match cache.get(&actor) {
                Some(value) => *value,
                None => {
                    let value = deployment_approval_capabilities(db, actor, self.project_id, false)
                        .await?
                        .contains(DEPLOYMENT_APPROVAL_DECIDE);
                    cache.insert(actor, value);
                    value
                }
            };
            if held {
                qualified += 1;
            }
        }
        Ok(qualified)
    }

    /// The principal that requested the deployment this requirement froze.
    pub async fn requester(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<principals::Model>> {
        let (_, db) = requester(ctx)?;
        let Some(deployment) = requirement_deployment(db, self).await? else {
            return Ok(None);
        };
        Ok(principals::Entity::find_by_id(deployment.requested_by)
            .one(db)
            .await?)
    }

    /// The principals whose decisions satisfied this requirement, as the frozen list records them.
    pub async fn satisfiedParticipants(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<principals::Model>> {
        let (_, db) = requester(ctx)?;
        let ids = participant_ids(&self.satisfied_participants);
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let found = principals::Entity::find()
            .filter(principals::Column::Id.is_in(ids.clone()))
            .all(db)
            .await?;
        let by_id: HashMap<Uuid, principals::Model> =
            found.into_iter().map(|row| (row.id, row)).collect();
        Ok(ids
            .into_iter()
            .filter_map(|id| by_id.get(&id).cloned())
            .collect())
    }

    /// Whether the requesting principal may record a decision on this project at all. A rendering
    /// hint: the command reauthorizes under its own locks.
    pub async fn eligible(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        let (principal_id, db) = requester(ctx)?;
        Ok(eligible_approver(db, principal_id, self.project_id).await?)
    }

    /// Whether the requesting principal has a decision left to record on *this* requirement.
    pub async fn decisionAvailable(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        let (principal_id, db) = requester(ctx)?;
        if self.required_approvers <= 0
            || self.projected_status() != ApprovalRequirementStatus::Pending
        {
            return Ok(false);
        }
        let Some(deployment) = requirement_deployment(db, self).await? else {
            return Ok(false);
        };
        if deployment.lifecycle_status
            != crate::entity::enums::DeploymentLifecycleStatus::AwaitingApproval
            || principal_id == deployment.requested_by
        {
            return Ok(false);
        }
        if !eligible_approver(db, principal_id, self.project_id).await? {
            return Ok(false);
        }
        let prior = deployment_approval_decisions::Entity::find()
            .filter(deployment_approval_decisions::Column::ApprovalRequirementId.eq(self.id))
            .filter(deployment_approval_decisions::Column::ActorPrincipalId.eq(principal_id))
            .select_only()
            .column(deployment_approval_decisions::Column::Id)
            .into_tuple::<Uuid>()
            .one(db)
            .await?;
        Ok(prior.is_none())
    }

    /// The frozen policy, rule, target and evidence facts this requirement was recorded against.
    pub async fn approvalSnapshot(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<DeploymentApprovalSnapshot> {
        let (_, db) = requester(ctx)?;
        let deployment = requirement_deployment(db, self).await?;
        let policy = deployment_policy_snapshots::Entity::find_by_id(self.deployment_id)
            .one(db)
            .await?;
        let plan = frozen_plan(db, self.deployment_id).await?;
        let environment_id = deployment
            .as_ref()
            .and_then(|row| row.environment_definition_version_id);
        let environment = match environment_id {
            Some(id) => {
                environment_definition_versions::Entity::find_by_id(id)
                    .one(db)
                    .await?
            }
            None => None,
        };
        let risk = policy
            .as_ref()
            .map(|policy| policy.risk.to_value())
            .unwrap_or_default();
        Ok(DeploymentApprovalSnapshot {
            policyDigest: policy
                .as_ref()
                .map(|policy| policy.policy_digest.clone())
                .unwrap_or_default(),
            policyRevision: policy.as_ref().map_or(0, |policy| policy.policy_revision),
            environmentClass: policy
                .as_ref()
                .map(|policy| policy.logical_environment_class.to_value())
                .unwrap_or_default(),
            risk: risk.clone(),
            riskLevel: risk,
            rule: ProjectApprovalPolicyRule {
                requiredEvidence: policy
                    .as_ref()
                    .map(|policy| string_list(&policy.required_evidence))
                    .unwrap_or_default(),
                requiredDistinctApproverCount: self.required_approvers,
            },
            target: ApprovalTargetSnapshot {
                agentVersionId: deployment
                    .as_ref()
                    .map(|row| row.agent_version_id)
                    .unwrap_or_default(),
                agentVersionDigest: plan
                    .as_ref()
                    .and_then(|plan| plan.agent_content_digest.clone())
                    .unwrap_or_default(),
                environmentDefinitionVersionId: environment_id,
                environmentDefinitionDigest: environment
                    .map(|row| row.content_digest)
                    .unwrap_or_default(),
                targetDigest: plan
                    .as_ref()
                    .and_then(|plan| plan.target_digest.clone())
                    .unwrap_or_default(),
                deploymentPlanDigest: plan
                    .as_ref()
                    .map(|plan| plan.plan_digest.clone())
                    .unwrap_or_default(),
                artifactDigest: plan
                    .as_ref()
                    .map(|plan| plan.package_digest.clone())
                    .unwrap_or_default(),
            },
            evidence: approval_evidence(db, self.deployment_id).await?,
            expiresAt: self.expires_at,
        })
    }
}

impl deployment_approval_requirements::Model {
    /// The status the surface reports: the stored value, with an elapsed `PENDING` expiry taken as
    /// `EXPIRED` on the service clock.
    fn projected_status(&self) -> ApprovalRequirementStatus {
        if self.status == ApprovalRequirementStatus::Pending
            && self.expires_at <= chrono::Utc::now().fixed_offset()
        {
            return ApprovalRequirementStatus::Expired;
        }
        self.status
    }
}

/// Every principal that recorded an `APPROVE` decision on a requirement.
async fn approving_actors(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
) -> Result<Vec<Uuid>, DbErr> {
    deployment_approval_decisions::Entity::find()
        .filter(deployment_approval_decisions::Column::ApprovalRequirementId.eq(requirement_id))
        .filter(deployment_approval_decisions::Column::Decision.eq(ApprovalDecision::Approve))
        .order_by_asc(deployment_approval_decisions::Column::ActorPrincipalId)
        .select_only()
        .column(deployment_approval_decisions::Column::ActorPrincipalId)
        .distinct()
        .into_tuple::<Uuid>()
        .all(db)
        .await
}

#[CustomFields]
impl deployment_approval_decisions::Model {
    /// The approval comment, normalized to M14's four review codes: a stored value outside that
    /// vocabulary is withheld, exactly as the deleted wire type withheld it.
    pub async fn comment(&self, _ctx: &Context<'_>) -> async_graphql::Result<Option<String>> {
        Ok(super::rows::review_text(self.comment.clone()))
    }

    /// The rejection reason, normalized the same way as `comment`.
    pub async fn rejectionReason(
        &self,
        _ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<String>> {
        Ok(super::rows::review_text(self.rejection_reason.clone()))
    }
}
