//! Computed fields of the generated deployment objects.
//! Each is derived from the row it is on, plus rows loaded through SeaORM. `hive-api` attaches
//! them to the generated objects.
//!
//! The nested structures of a deployment:
//!
//! - `Deployments.plan`: the frozen plan, the one `deployment_plan_versions` row with
//!   `version_number = 1`.
//! - `Deployments.currentAttempt`: the newest execution attempt.
//! - `Deployments.rollbackTarget`: the prior active target of the same agent and environment,
//!   answered as the deployment row itself, so its own relations and computed fields carry the
//!   version number, plan digest and runtime health.
//! - `Deployments.timeline`: the deployment's audit events and its attempts' stage events in one
//!   ordered list, with the audit action's fixed status/message/source vocabulary.
//! - `Deployments.terminal`, `canCancel`, `canRetry`, `canPromote`, `canRollback`: the lifecycle
//!   state machine and the recovery preconditions, taken with the requesting principal's
//!   capabilities at the project. A surface renders an action from these instead of restating the
//!   rules; each command reauthorizes and rechecks them under its own locks.
//! - `DeploymentPlanVersions.review`: the retained plan review facts, with placeholder values for
//!   a plan that has none.
//! - `DeploymentEvidenceSnapshots.state`: the evidence state, which depends on the deployment's
//!   frozen policy, on recorded invalidations and on the clock, so it is not a stored value.
//!
//! The approval surface adds these values next to the stored requirement row:
//!
//! - `DeploymentApprovalRequirements.status`: the stored status, with an elapsed `PENDING` expiry
//!   projected to `EXPIRED` before the maintenance tick writes it, taken on the service clock.
//! - `qualifyingApprovalCount`, `requester`, `satisfiedParticipants`, `eligible` and
//!   `decisionAvailable`: the per-requester and per-actor authority facts, each computed on
//!   `capability::deployment_approval_capabilities`.
//! - `approvalSnapshot`: the frozen policy, target and evidence facts, assembled from the
//!   deployment's own plan, policy snapshot, environment definition version and evidence rows.
//! - `DeploymentApprovalDecisions.comment` / `rejectionReason`: the four-code review-text
//!   normalization, over columns the generated API withholds.
//!
//! The parent row is already tenant-filtered by the `entity_filter` hook before any of these runs.

#![allow(non_snake_case)] // a computed field is named after its method

use super::loaders::{
    CurrentAttemptLoader, FrozenPlanLoader, RollbackTargetKey, RollbackTargetLoader,
};
use crate::capability::{
    self, deployment_approval_capabilities, Scope, DEPLOYMENT_APPROVAL_DECIDE,
};
use crate::console::requester;
use crate::entity::enums::{
    ApprovalDecision, ApprovalRequirementStatus, DeploymentAuditAction,
    DeploymentRuntimeHealthStatus as RuntimeHealthStatus, EvidenceInvalidationKind,
    LifecycleStatus,
};
use crate::entity::{
    deployment_approval_decisions, deployment_approval_requirements, deployment_attempts,
    deployment_audit_events, deployment_evidence_invalidations, deployment_evidence_snapshots,
    deployment_plan_review_facts, deployment_plan_versions, deployment_policy_snapshots,
    deployment_runtime_health, deployment_stage_events, deployments,
    environment_definition_versions, principals, projects,
};
use hive_domain::deployment::DeploymentLifecycleStatus;
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect,
};
use std::collections::HashMap;
use std::sync::Arc;
// `#[CustomFields]` and `CustomOutputType` expand to paths that start with `async_graphql::`.
use seaography::async_graphql::dataloader::DataLoader;
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

/// The sentence a plan with no retained facts reads.
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

impl deployments::Model {
    fn lifecycle(&self) -> DeploymentLifecycleStatus {
        self.lifecycle_status.into()
    }
}

/// Whether the requesting principal holds `code` at `project_id`.
async fn may(ctx: &Context<'_>, project_id: Uuid, code: &str) -> async_graphql::Result<bool> {
    let (principal_id, db) = requester(ctx)?;
    Ok(
        capability::has_capability(db, principal_id, code, Scope::Project(project_id), false)
            .await?,
    )
}

/// A loader's failure, which arrives shared because every key of the batch is told about it.
fn loaded(error: Arc<DbErr>) -> async_graphql::Error {
    async_graphql::Error::new(error.to_string())
}

/// The prior active deployment a rollback of `deployment` would return to, batched across the
/// page by `loaders::RollbackTargetLoader`.
async fn rollback_target(
    ctx: &Context<'_>,
    deployment: &deployments::Model,
) -> async_graphql::Result<Option<deployments::Model>> {
    let Some(key) = RollbackTargetKey::of(deployment) else {
        return Ok(None);
    };
    ctx.data::<DataLoader<RollbackTargetLoader>>()?
        .load_one(key)
        .await
        .map_err(loaded)
}

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

/// Every `APPROVAL_*` action is the service's own, an action with no actor is the worker's, and
/// everything else is a user's.
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
        ctx.data::<DataLoader<FrozenPlanLoader>>()?
            .load_one(self.id)
            .await
            .map_err(loaded)
    }

    /// The newest execution attempt, or `null` before the first one.
    pub async fn currentAttempt(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<deployment_attempts::Model>> {
        ctx.data::<DataLoader<CurrentAttemptLoader>>()?
            .load_one(self.id)
            .await
            .map_err(loaded)
    }

    /// The prior active deployment of the same agent and environment that a rollback would return
    /// to: the newest one requested before this one. A candidate without a published version, a
    /// frozen plan or observed runtime health is passed over.
    pub async fn rollbackTarget(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<deployments::Model>> {
        rollback_target(ctx, self).await
    }

    /// Whether the lifecycle has reached a state no further transition leaves. A surface that
    /// polls the deployment stops when this is true.
    pub async fn terminal(&self, _ctx: &Context<'_>) -> async_graphql::Result<bool> {
        Ok(self.lifecycle().is_terminal())
    }

    /// Whether the requesting principal may cancel this deployment now. A rendering hint: the
    /// command reauthorizes and rechecks the lifecycle under its own locks.
    pub async fn canCancel(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        Ok(self.lifecycle().is_cancellable()
            && may(ctx, self.project_id, capability::DEPLOYMENT_CANCEL).await?)
    }

    /// Whether the requesting principal may start a recovery cycle from this failed deployment.
    pub async fn canRetry(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        Ok(self.lifecycle() == DeploymentLifecycleStatus::Failed
            && may(ctx, self.project_id, capability::DEPLOYMENT_RETRY).await?)
    }

    /// Whether the requesting principal may record a promotion: an active deployment whose
    /// observed runtime health is `HEALTHY`.
    pub async fn canPromote(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        if self.lifecycle() != DeploymentLifecycleStatus::Active
            || !may(ctx, self.project_id, capability::DEPLOYMENT_PROMOTE).await?
        {
            return Ok(false);
        }
        let (_, db) = requester(ctx)?;
        Ok(deployment_runtime_health::Entity::find_by_id(self.id)
            .one(db)
            .await?
            .is_some_and(|health| health.status == RuntimeHealthStatus::Healthy))
    }

    /// Whether the requesting principal may start a rollback cycle: a failed or active deployment
    /// with a prior active target to return to.
    pub async fn canRollback(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        if !matches!(
            self.lifecycle(),
            DeploymentLifecycleStatus::Failed | DeploymentLifecycleStatus::Active
        ) || !may(ctx, self.project_id, capability::DEPLOYMENT_ROLLBACK).await?
        {
            return Ok(false);
        }
        Ok(rollback_target(ctx, self).await?.is_some())
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
    /// The retained review facts of this plan. A plan that has none answers placeholder values.
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
    /// the wall clock; the value is a read, not a guard on a write.
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

/// `DEPLOYMENT_APPROVAL.DECIDE` at the project, and an active project.
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
/// each with the state of the snapshot that satisfies it.
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
    /// stored column is only ever rewritten by a command or by the maintenance tick; this
    /// projection is taken on the clock, and reading stores nothing.
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
        let plan = ctx
            .data::<DataLoader<FrozenPlanLoader>>()?
            .load_one(self.deployment_id)
            .await
            .map_err(loaded)?;
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
    /// vocabulary is withheld.
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

// --- the deployment preview --------------------------------------------------------------------

/// The active target a preview reports next to the version it would replace. Not the generated
/// `Deployments` object: `aliasName` is derived from the environment's stable definition id, and
/// the three digests are the *current* target's, not the previewed one's.
#[derive(CustomOutputType, Clone)]
pub struct DeploymentPreviewTarget {
    pub aliasName: Option<String>,
    pub deploymentId: Uuid,
    pub agentVersionId: Uuid,
    pub agentVersionNumber: i64,
    pub targetDigest: String,
    pub requestedAt: DateTimeWithTimeZone,
}

/// What deploying this agent version into this environment definition version with this strategy
/// would freeze. Nothing here is stored: the compiler derives it from the version, the environment
/// definition version, the project's approval policy and the current target, all read through
/// SeaORM. Every text enum is a `String`, as every other generated text enum is.
#[derive(CustomOutputType, Clone)]
pub struct DeploymentPreview {
    /// The generated entity row, so the console reads one fragment for it.
    pub environmentDefinitionVersion: environment_definition_versions::Model,
    pub strategy: String,
    pub risk: String,
    pub policyDigest: String,
    pub policyRevision: i64,
    pub requiredEvidence: Vec<String>,
    pub requiredApprovers: i32,
    pub planDigest: String,
    pub packageDigest: String,
    pub catalogReleaseId: String,
    pub catalogReleaseDigest: String,
    pub agentContentDigest: String,
    pub targetDigest: String,
    pub bindingDigest: String,
    pub currentTarget: Option<DeploymentPreviewTarget>,
    pub requirementExpiresAt: Option<DateTimeWithTimeZone>,
    pub warnings: Vec<String>,
    pub compatibility: String,
}

/// Answers `AgentVersions.deploymentPreview`. The preview is gated on `DEPLOYMENT.VIEW` at the
/// *agent version's* project (`queries::compilation_context`), which is why the field sits on
/// `agent_versions::Model` and not on `environment_definition_versions`: an environment definition
/// version is a catalog row with no owner, visible to an active member of any organization, which
/// is wider than who may request a preview. The service applies that capability test itself, so
/// the field answers `null` for a principal who may read the version row but may not deploy from
/// it.
pub(crate) async fn agent_version_preview(
    ctx: &Context<'_>,
    agent_version_id: Uuid,
    environment_definition_version_id: &str,
    strategy: &str,
) -> async_graphql::Result<Option<DeploymentPreview>> {
    let (principal_id, db) = requester(ctx)?;
    let service = hive_application::deployment::DeploymentService::new(
        super::PgDeploymentRepository::new(db.clone()),
    );
    let Some(preview) = service
        .preview(
            principal_id,
            &agent_version_id.to_string(),
            environment_definition_version_id,
            strategy,
        )
        .await
        .map_err(|error| async_graphql::Error::new(error.to_string()))?
    else {
        return Ok(None);
    };
    // The compiled environment is the stored row; hand the row itself back so the console reads the
    // generated `EnvironmentDefinitionVersions` object with its own relations.
    let Some(environment) =
        environment_definition_versions::Entity::find_by_id(preview.environment.id)
            .one(db)
            .await?
    else {
        return Ok(None);
    };
    Ok(Some(DeploymentPreview {
        environmentDefinitionVersion: environment,
        strategy: preview.strategy,
        risk: preview.risk,
        policyDigest: preview.policy_digest,
        policyRevision: preview.policy_revision,
        requiredEvidence: preview.required_evidence,
        requiredApprovers: preview.required_approvers,
        planDigest: preview.plan_digest,
        packageDigest: preview.package_digest,
        catalogReleaseId: preview.catalog_release_id,
        catalogReleaseDigest: preview.catalog_release_digest,
        agentContentDigest: preview.agent_content_digest,
        targetDigest: preview.target_digest,
        bindingDigest: preview.binding_digest,
        currentTarget: preview
            .current_target
            .map(|target| DeploymentPreviewTarget {
                aliasName: Some(target.alias_name),
                deploymentId: target.deployment_id,
                agentVersionId: target.agent_version_id,
                agentVersionNumber: target.agent_version_number,
                targetDigest: target.target_digest,
                requestedAt: target.requested_at.fixed_offset(),
            }),
        requirementExpiresAt: Some(preview.requirement_expires_at.fixed_offset()),
        warnings: preview.warnings,
        compatibility: preview.compatibility,
    }))
}
