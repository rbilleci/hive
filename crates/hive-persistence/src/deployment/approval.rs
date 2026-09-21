//! The approval-requirement lifecycle machinery `deploy`, `recovery` and the outbox worker's
//! `execute` share: reconciliation, evidence-readiness, handoff eligibility, and blocking. The
//! approval inbox and decision surface lives in `queries.rs` instead, not here.
//!
//! `approval_evidence_issue`/`waiting_for_evaluation` are here because `reconcile_pending` calls
//! them, and every deploy and recovery path reaches `reconcile_pending` through
//! `automatic_approval_handoff`.
//!
//! The scheduled reconciliation entry points `reconcile_approval_expiry` and
//! `reconcile_approval_upgrade` are here too, with the maintenance-heartbeat helpers
//! (`reconcile_expired_approval_requirements`, `release_compatible_approval_handoffs`,
//! `approval_maintenance_failed`) that `worker.rs`'s `record_worker_heartbeat` composes into its
//! own opportunistic maintenance branch.
//!
//! Every statement here is a SeaORM entity read, an `update_many` with the guard in its `WHERE`
//! clause, or an `ActiveModel` insert. Three constructs are expressed in Rust rather than SQL,
//! each time inside the transaction that already holds the row locks:
//!
//! * The frozen-policy match (`policy_matrix -> (class || '_' || risk) -> ...` against
//!   `required_approvers`/`required_evidence`, and the six digest equalities against the plan) is
//!   the deployment, its policy snapshot and its frozen plan read as three rows and compared in
//!   Rust. The plan and the policy snapshot are insert-once rows of the deployment's own creating
//!   transaction, so nothing can change them under a reader.
//! * The evidence relational division (`NOT EXISTS (jsonb_array_elements_text(required_evidence)
//!   WHERE NOT EXISTS (valid snapshot))`) is one read of the deployment's evidence snapshots plus
//!   one read of their invalidations, divided in Rust. `evidence_ready` is exactly
//!   `approval_evidence_issue(..) == None`, so both now answer from the same three reads.
//! * `approval_execution_eligible`'s `count(DISTINCT ...)`/`jsonb_array_length` over
//!   `satisfied_participants` is the requirement row's own `jsonb` column counted in Rust, and its
//!   participants-are-all-approvers anti-join is one read of the requirement's `APPROVE` decisions.
//!
//! `clock_timestamp()` has no sea-query builder. Every one of its uses here is a comparison
//! against a stored instant (an expiry, an observation), never a stored value, so the same wall
//! clock is taken from the service and bound as a value. `CURRENT_TIMESTAMP` — the transaction's
//! own start, which every write here stores — stays `Expr::current_timestamp()`.

use crate::deployment::rows::raw_requirement_by_deployment;
use crate::deployment::writes::{audit, system_audit, touch_projection};
use crate::entity::enums::{
    ApprovalInvalidationCode, ApprovalRequirementStatus as EntityRequirementStatus,
    DeploymentLifecycleStatus as EntityLifecycleStatus, DeploymentOutboxEventType,
    DeploymentOutboxStatus, DeploymentRuntimeHealthStatus, LifecycleStatus, WorkerHeartbeatState,
};
use crate::entity::{
    deployment_approval_decisions, deployment_approval_handoff_releases,
    deployment_approval_project_archive_events, deployment_approval_requirements,
    deployment_evidence_invalidations, deployment_evidence_snapshots, deployment_outbox_events,
    deployment_plan_versions, deployment_policy_snapshots, deployment_runtime_health,
    deployment_worker_heartbeats, deployments, projects,
};
use hive_domain::deployment::{ApprovalRequirementStatus, DeploymentLifecycleStatus};
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::sea_query::{Expr, ExprTrait, Func, IntoTableRef, LockType, OnConflict, Query};
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    JoinType, NotSet, QueryFilter, QueryOrder, QuerySelect, RelationTrait, Set, TransactionTrait,
    TryInsertResult,
};
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

/// `clock_timestamp()` — see this module's doc comment.
fn wall_clock() -> DateTimeWithTimeZone {
    chrono::Utc::now().fixed_offset()
}

/// `CAST('<n> <unit>' AS interval)`, the spelling `evaluation::worker` established, because
/// sea-query has no interval `Value`.
fn interval(text: String) -> Expr {
    Expr::value(text).cast_as("interval")
}

/// `jsonb` array of principal ids, as `satisfied_participants` holds it.
fn participants_json(participants: &[Uuid]) -> serde_json::Value {
    serde_json::Value::Array(
        participants
            .iter()
            .map(|id| serde_json::Value::String(id.to_string()))
            .collect(),
    )
}

/// The 0-rows-affected branch below raises `RecordNotFound`, not a fabricated serialization
/// failure. Every call site already holds the row's lock from a `FOR UPDATE` read earlier in the
/// same transaction, so a genuine concurrent race surfaces as an authentic SQLSTATE 40001 from
/// Aurora DSQL's commit-time validation in `tx.commit()`; this branch is defense in depth only.
pub async fn transition_requirement(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
    status: ApprovalRequirementStatus,
    code: Option<&str>,
    participants: &[Uuid],
) -> Result<(), DbErr> {
    use deployment_approval_requirements::Column;
    let entity_status = EntityRequirementStatus::try_from_value(&status.as_str().to_string())?;
    let stamp = |when: bool| {
        if when {
            Expr::current_timestamp()
        } else {
            Expr::value(None::<DateTimeWithTimeZone>)
        }
    };
    let invalidation_code = if status == ApprovalRequirementStatus::Invalidated {
        code.map(|value| ApprovalInvalidationCode::try_from_value(&value.to_string()))
            .transpose()?
            .map(|value| value.to_value())
    } else {
        None
    };
    let mut update = deployment_approval_requirements::Entity::update_many()
        .col_expr(Column::Status, Expr::value(entity_status.to_value()))
        .col_expr(Column::Revision, Expr::col(Column::Revision).add(1))
        .col_expr(
            Column::SatisfiedAt,
            stamp(status == ApprovalRequirementStatus::Satisfied),
        )
        .col_expr(
            Column::RejectedAt,
            stamp(status == ApprovalRequirementStatus::Rejected),
        )
        .col_expr(
            Column::InvalidatedAt,
            stamp(status == ApprovalRequirementStatus::Invalidated),
        )
        .col_expr(Column::InvalidationCode, Expr::value(invalidation_code))
        .col_expr(
            Column::SatisfiedParticipants,
            Expr::value(participants_json(participants)),
        )
        .filter(Column::Id.eq(requirement_id))
        .filter(Column::Status.eq(EntityRequirementStatus::Pending));
    if matches!(
        status,
        ApprovalRequirementStatus::Satisfied | ApprovalRequirementStatus::Rejected
    ) {
        update = update.filter(Column::ExpiresAt.gt(wall_clock()));
    }
    let result = update.exec(db).await?;
    if result.rows_affected != 1 {
        return Err(DbErr::RecordNotFound(format!(
            "no pending approval requirement with id {requirement_id}"
        )));
    }
    Ok(())
}

/// `UPDATE deployment_runtime_health SET status = 'CANCELED', summary = <summary>, ...`, the shape
/// five terminalizing paths share.
async fn cancel_runtime_health(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    summary: &str,
) -> Result<(), DbErr> {
    use deployment_runtime_health::Column;
    deployment_runtime_health::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(DeploymentRuntimeHealthStatus::Canceled.to_value()),
        )
        .col_expr(Column::Summary, Expr::value(summary))
        .col_expr(Column::ObservedAt, Expr::current_timestamp())
        .col_expr(Column::Generation, Expr::col(Column::Generation).add(1))
        .filter(Column::DeploymentId.eq(deployment_id))
        .exec(db)
        .await?;
    Ok(())
}

/// `UPDATE deployments SET lifecycle_status = <status>, revision = revision + 1[, projection_revision
/// = projection_revision + 1], updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN
/// (<from>)`, the guarded lifecycle move every terminalizing path takes.
async fn move_lifecycle(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    to: EntityLifecycleStatus,
    from: [EntityLifecycleStatus; 2],
    touch: bool,
) -> Result<u64, DbErr> {
    use deployments::Column;
    let mut update = deployments::Entity::update_many()
        .col_expr(Column::LifecycleStatus, Expr::value(to.to_value()))
        .col_expr(Column::Revision, Expr::col(Column::Revision).add(1));
    if touch {
        update = update.col_expr(
            Column::ProjectionRevision,
            Expr::col(Column::ProjectionRevision).add(1),
        );
    }
    Ok(update
        .col_expr(Column::UpdatedAt, Expr::current_timestamp())
        .filter(Column::Id.eq(deployment_id))
        .filter(Column::LifecycleStatus.is_in(from))
        .exec(db)
        .await?
        .rows_affected)
}

async fn delete_handoff_release(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    deployment_approval_handoff_releases::Entity::delete_many()
        .filter(deployment_approval_handoff_releases::Column::DeploymentId.eq(deployment_id))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn cancel_rejected_deployment(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    cancel_runtime_health(
        db,
        deployment_id,
        "Approval rejection terminalized this local deployment.",
    )
    .await?;
    let updated = move_lifecycle(
        db,
        deployment_id,
        EntityLifecycleStatus::Canceled,
        [
            EntityLifecycleStatus::AwaitingApproval,
            EntityLifecycleStatus::Requested,
        ],
        false,
    )
    .await?;
    if updated != 1 {
        return Err(DbErr::RecordNotFound(format!(
            "no cancelable deployment with id {deployment_id}"
        )));
    }
    Ok(())
}

pub async fn approve_deployment_for_execution(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    let updated = move_lifecycle(
        db,
        deployment_id,
        EntityLifecycleStatus::Approved,
        [
            EntityLifecycleStatus::AwaitingApproval,
            EntityLifecycleStatus::Requested,
        ],
        false,
    )
    .await?;
    if updated != 1 {
        return Err(DbErr::RecordNotFound(format!(
            "no approvable deployment with id {deployment_id}"
        )));
    }
    Box::pin(automatic_approval_handoff(db, deployment_id)).await?;
    Ok(())
}

pub async fn invalidate_pending_requirement(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    code: &str,
    actor: Option<Uuid>,
) -> Result<(), DbErr> {
    let Some(raw) = raw_requirement_by_deployment(db, deployment_id, true).await? else {
        return Ok(());
    };
    if raw.status != ApprovalRequirementStatus::Pending {
        return Ok(());
    }
    transition_requirement(
        db,
        raw.id,
        ApprovalRequirementStatus::Invalidated,
        Some(code),
        &[],
    )
    .await?;
    audit(
        db,
        deployment_id,
        actor,
        "APPROVAL_INVALIDATED",
        json!({"requirementId": raw.id.to_string(), "code": code}),
    )
    .await
}

pub async fn requirement_expired(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
) -> Result<bool, DbErr> {
    Ok(
        deployment_approval_requirements::Entity::find_by_id(requirement_id)
            .filter(deployment_approval_requirements::Column::ExpiresAt.lte(wall_clock()))
            .select_only()
            .column(deployment_approval_requirements::Column::Id)
            .into_tuple::<Uuid>()
            .one(db)
            .await?
            .is_some(),
    )
}

/// The five-term archive-boundary `OR` — the deployment's project revision against the event's, or
/// its requested-at against the event's archived-at when either revision is unknown — as a
/// condition over the archive events of the deployment's own project. The deployment's three
/// values are read first, so the disjunction collapses to the branch its own row selects.
fn archive_boundary_condition(deployment: &deployments::Model) -> Condition {
    use deployment_approval_project_archive_events::Column;
    match deployment.project_lifecycle_revision {
        Some(revision) => Condition::any()
            .add(
                Condition::all()
                    .add(Column::ArchivedProjectRevision.is_not_null())
                    .add(Column::ArchivedProjectRevision.gte(revision)),
            )
            .add(
                Condition::all()
                    .add(Column::ArchivedProjectRevision.is_null())
                    .add(Column::ArchivedAt.gte(deployment.requested_at)),
            ),
        None => Condition::all().add(Column::ArchivedAt.gte(deployment.requested_at)),
    }
}

pub async fn deployment_archive_boundary(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    Ok(deployment_approval_project_archive_events::Entity::find()
        .filter(
            deployment_approval_project_archive_events::Column::ProjectId.eq(deployment.project_id),
        )
        .filter(archive_boundary_condition(&deployment))
        .select_only()
        .column(deployment_approval_project_archive_events::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .is_some())
}

/// A worker heartbeat is fresh for 15 seconds after it was observed.
fn fresh_heartbeat_after() -> DateTimeWithTimeZone {
    wall_clock() - chrono::Duration::seconds(15)
}

pub async fn worker_ready(db: &impl ConnectionTrait) -> Result<bool, DbErr> {
    use deployment_worker_heartbeats::Column;
    Ok(deployment_worker_heartbeats::Entity::find()
        .filter(Column::ApprovalExecutionCompatible.eq(true))
        .filter(Column::State.eq(WorkerHeartbeatState::Ready))
        .filter(Column::ObservedAt.gt(fresh_heartbeat_after()))
        .select_only()
        .column(Column::WorkerId)
        .into_tuple::<String>()
        .one(db)
        .await?
        .is_some())
}

pub async fn compatible_approval_worker(
    db: &impl ConnectionTrait,
    worker: &str,
) -> Result<bool, DbErr> {
    use deployment_worker_heartbeats::Column;
    Ok(deployment_worker_heartbeats::Entity::find()
        .filter(Column::WorkerId.eq(worker))
        .filter(Column::ApprovalExecutionCompatible.eq(true))
        .filter(Column::State.eq(WorkerHeartbeatState::Ready))
        .filter(Column::ObservedAt.gt(fresh_heartbeat_after()))
        .select_only()
        .column(Column::WorkerId)
        .into_tuple::<String>()
        .one(db)
        .await?
        .is_some())
}

pub async fn approved_approval_handoff(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let Some(requirement) = requirement_of(db, deployment_id).await? else {
        return Ok(false);
    };
    if requirement.status != EntityRequirementStatus::Satisfied {
        return Ok(false);
    }
    Ok(deployments::Entity::find_by_id(deployment_id)
        .filter(deployments::Column::LifecycleStatus.eq(EntityLifecycleStatus::Approved))
        .select_only()
        .column(deployments::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .is_some())
}

async fn requirement_of(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<deployment_approval_requirements::Model>, DbErr> {
    deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .one(db)
        .await
}

/// Returns a claimed event to the queue after `delay_millis`, spread by the first byte of its own
/// identifier (`get_byte(uuid_send(id), 0)`, taken here from the identifier this process already
/// holds), without consuming retry capacity.
async fn defer_handoff(
    db: &impl ConnectionTrait,
    event_id: Uuid,
    delay_millis: i64,
    detail: &str,
) -> Result<(), DbErr> {
    use deployment_outbox_events::Column;
    let jitter = i64::from(event_id.as_bytes()[0]);
    deployment_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(DeploymentOutboxStatus::Pending.to_value()),
        )
        .col_expr(
            Column::AvailableAt,
            Expr::current_timestamp()
                .add(interval(format!("{} milliseconds", delay_millis + jitter))),
        )
        .col_expr(Column::ClaimedAt, Expr::value(None::<DateTimeWithTimeZone>))
        .col_expr(Column::ClaimedBy, Expr::value(None::<String>))
        .col_expr(
            Column::AttemptCount,
            Expr::from(Func::greatest([
                Expr::col(Column::AttemptCount).sub(1),
                Expr::val(0),
            ])),
        )
        .col_expr(Column::LastError, Expr::value(detail))
        .filter(Column::Id.eq(event_id))
        .filter(Column::Status.eq(DeploymentOutboxStatus::Processing))
        .exec(db)
        .await?;
    Ok(())
}

/// Returns a raced M13 claim to the compatible-worker gate without consuming retry capacity.
pub async fn defer_incompatible_approval_handoff(
    db: &impl ConnectionTrait,
    event_id: Uuid,
) -> Result<(), DbErr> {
    defer_handoff(
        db,
        event_id,
        5_000,
        "An incompatible worker cannot execute an approved handoff.",
    )
    .await
}

/// Returns a retained event to the queue until maintenance establishes its frozen handoff.
pub async fn defer_pending_approval_handoff(
    db: &impl ConnectionTrait,
    event_id: Uuid,
) -> Result<(), DbErr> {
    defer_handoff(
        db,
        event_id,
        1_000,
        "The approval handoff is not ready for execution.",
    )
    .await
}

/// A `jsonb` array of strings as the column holds it.
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

/// SQL equality, where a comparison with `NULL` is never true.
fn sql_eq<T: PartialEq>(left: &Option<T>, right: &Option<T>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left == right)
}

struct PolicyRow {
    binding_digest: Option<String>,
    agent_version_id: Option<Uuid>,
    environment_definition_version_id: Option<Uuid>,
    target_digest: Option<String>,
    plan_digest: Option<String>,
    package_digest: Option<String>,
    required_evidence: Vec<String>,
}

impl From<&deployment_policy_snapshots::Model> for PolicyRow {
    fn from(policy: &deployment_policy_snapshots::Model) -> Self {
        PolicyRow {
            binding_digest: policy.binding_digest.clone(),
            agent_version_id: policy.agent_version_id,
            environment_definition_version_id: policy.environment_definition_version_id,
            target_digest: policy.target_digest.clone(),
            plan_digest: policy.plan_digest.clone(),
            package_digest: policy.package_digest.clone(),
            required_evidence: string_list(&policy.required_evidence),
        }
    }
}

/// The join conditions between the deployment, its frozen policy snapshot and its version-1 plan,
/// decided in Rust over the three rows. Every comparison keeps SQL's own
/// `NULL`-is-never-equal semantics, and a `->` that finds no key is `NULL`, so a policy matrix with
/// no cell for this environment class and risk never matches.
fn policy_matches(
    deployment: &deployments::Model,
    policy: &deployment_policy_snapshots::Model,
    plan: &deployment_plan_versions::Model,
) -> bool {
    if !sql_eq(&policy.agent_version_id, &Some(deployment.agent_version_id))
        || !sql_eq(
            &policy.environment_definition_version_id,
            &deployment.environment_definition_version_id,
        )
        || !sql_eq(&policy.target_digest, &plan.target_digest)
        || !sql_eq(&policy.plan_digest, &Some(plan.plan_digest.clone()))
        || !sql_eq(&policy.package_digest, &Some(plan.package_digest.clone()))
        || policy.logical_environment_class != deployment.environment
    {
        return false;
    }
    let cell = format!(
        "{}_{}",
        policy.logical_environment_class.to_value(),
        policy.risk.to_value()
    );
    let Some(rule) = policy.policy_matrix.get(&cell) else {
        return false;
    };
    rule.get("requiredApprovers") == Some(&json!(policy.required_approvers))
        && rule.get("requiredEvidence") == Some(&policy.required_evidence)
}

/// The deployment's evidence snapshots and the identifiers of every snapshot an invalidation
/// covers, read once for the whole evidence decision.
struct EvidenceFacts {
    snapshots: Vec<deployment_evidence_snapshots::Model>,
    invalidated: HashSet<Uuid>,
}

async fn evidence_facts(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<EvidenceFacts, DbErr> {
    let snapshots = deployment_evidence_snapshots::Entity::find()
        .filter(deployment_evidence_snapshots::Column::DeploymentId.eq(deployment_id))
        .all(db)
        .await?;
    let ids: Vec<Uuid> = snapshots.iter().map(|row| row.id).collect();
    let invalidated = if ids.is_empty() {
        HashSet::new()
    } else {
        deployment_evidence_invalidations::Entity::find()
            .filter(deployment_evidence_invalidations::Column::EvidenceSnapshotId.is_in(ids))
            .select_only()
            .column(deployment_evidence_invalidations::Column::EvidenceSnapshotId)
            .into_tuple::<Uuid>()
            .all(db)
            .await?
            .into_iter()
            .collect()
    };
    Ok(EvidenceFacts {
        snapshots,
        invalidated,
    })
}

impl EvidenceFacts {
    fn digests_match(snapshot: &deployment_evidence_snapshots::Model, policy: &PolicyRow) -> bool {
        sql_eq(&snapshot.binding_digest, &policy.binding_digest)
            && sql_eq(&snapshot.agent_version_id, &policy.agent_version_id)
            && sql_eq(
                &snapshot.environment_definition_version_id,
                &policy.environment_definition_version_id,
            )
            && sql_eq(&snapshot.target_digest, &policy.target_digest)
            && sql_eq(&snapshot.plan_digest, &policy.plan_digest)
            && sql_eq(&snapshot.package_digest, &policy.package_digest)
    }

    fn valid(&self, policy: &PolicyRow, kind: &str, now: DateTimeWithTimeZone) -> bool {
        self.snapshots.iter().any(|snapshot| {
            snapshot.evidence_kind.to_value() == kind
                && Self::digests_match(snapshot, policy)
                && snapshot
                    .expires_at
                    .is_none_or(|expires_at| expires_at > now)
                && !self.invalidated.contains(&snapshot.id)
        })
    }

    fn observed(&self, kind: &str) -> bool {
        self.snapshots
            .iter()
            .any(|snapshot| snapshot.evidence_kind.to_value() == kind)
    }

    fn expired(&self, policy: &PolicyRow, kind: &str, now: DateTimeWithTimeZone) -> bool {
        self.snapshots.iter().any(|snapshot| {
            snapshot.evidence_kind.to_value() == kind
                && Self::digests_match(snapshot, policy)
                && snapshot
                    .expires_at
                    .is_some_and(|expires_at| expires_at <= now)
                && !self.invalidated.contains(&snapshot.id)
        })
    }
}

/// The deployment, its frozen policy snapshot and its version-1 plan, when all three exist.
async fn frozen_cycle(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<
    Option<(
        deployments::Model,
        deployment_policy_snapshots::Model,
        deployment_plan_versions::Model,
    )>,
    DbErr,
> {
    let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let Some(policy) = deployment_policy_snapshots::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let Some(plan) = deployment_plan_versions::Entity::find()
        .filter(deployment_plan_versions::Column::DeploymentId.eq(deployment_id))
        .filter(deployment_plan_versions::Column::VersionNumber.eq(1_i64))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some((deployment, policy, plan)))
}

/// The frozen policy the evidence decision runs against, or `None` when the cycle no longer matches
/// its own plan and policy.
async fn evidence_issue_policy_row(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<PolicyRow>, DbErr> {
    let Some((deployment, policy, plan)) = frozen_cycle(db, deployment_id).await? else {
        return Ok(None);
    };
    if !policy_matches(&deployment, &policy, &plan) {
        return Ok(None);
    }
    Ok(Some(PolicyRow::from(&policy)))
}

/// The policy snapshot of a deployment that still exists, with no frozen-cycle test.
async fn waiting_policy_row(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<PolicyRow>, DbErr> {
    if deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    Ok(
        deployment_policy_snapshots::Entity::find_by_id(deployment_id)
            .one(db)
            .await?
            .as_ref()
            .map(PolicyRow::from),
    )
}

/// Returns `APPROVAL_EVIDENCE_MISMATCH`/`APPROVAL_EVIDENCE_MISSING`/`APPROVAL_EVIDENCE_EXPIRED`, or
/// `None` when every required evidence kind is valid.
pub async fn approval_evidence_issue(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<String>, DbErr> {
    let Some(policy) = evidence_issue_policy_row(db, deployment_id).await? else {
        return Ok(Some("APPROVAL_EVIDENCE_MISMATCH".to_string()));
    };
    let facts = evidence_facts(db, deployment_id).await?;
    Ok(evidence_issue_of(&policy, &facts))
}

fn evidence_issue_of(policy: &PolicyRow, facts: &EvidenceFacts) -> Option<String> {
    let now = wall_clock();
    for kind in &policy.required_evidence {
        if facts.valid(policy, kind, now) {
            continue;
        }
        if !facts.observed(kind) {
            return Some("APPROVAL_EVIDENCE_MISSING".to_string());
        }
        if facts.expired(policy, kind, now) {
            return Some("APPROVAL_EVIDENCE_EXPIRED".to_string());
        }
        return Some("APPROVAL_EVIDENCE_MISMATCH".to_string());
    }
    None
}

pub async fn waiting_for_evaluation(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let Some(waiting_policy) = waiting_policy_row(db, deployment_id).await? else {
        return Ok(false);
    };
    if !waiting_policy
        .required_evidence
        .iter()
        .any(|kind| kind == "EVALUATION_PASSED")
    {
        return Ok(false);
    }
    let Some(policy) = evidence_issue_policy_row(db, deployment_id).await? else {
        return Ok(false);
    };
    let facts = evidence_facts(db, deployment_id).await?;
    if evidence_issue_of(&policy, &facts).as_deref() != Some("APPROVAL_EVIDENCE_MISSING") {
        return Ok(false);
    }
    if facts.observed("EVALUATION_PASSED") {
        return Ok(false);
    }
    let now = wall_clock();
    for kind in &waiting_policy.required_evidence {
        if kind == "EVALUATION_PASSED" {
            continue;
        }
        if !facts.valid(&policy, kind, now) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The frozen-cycle test plus "no required evidence kind lacks a valid snapshot", which together
/// are exactly `approval_evidence_issue(..) == None` over the same three reads.
pub async fn evidence_ready(db: &impl ConnectionTrait, deployment_id: Uuid) -> Result<bool, DbErr> {
    Ok(approval_evidence_issue(db, deployment_id).await?.is_none())
}

/// Terminalizes a pending requirement the frozen facts can no longer satisfy: a deployment that
/// has started execution invalidates it, an expired requirement expires it, and an evidence issue
/// invalidates it unless the deployment is still waiting for an evaluation to finish. With no
/// pending requirement, an already-satisfied one past its expiry blocks execution instead.
/// Returns whether it changed anything.
pub async fn reconcile_pending(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let Some(lifecycle_row) = deployments::Entity::find_by_id(deployment_id)
        .lock_exclusive()
        .select_only()
        .column(deployments::Column::LifecycleStatus)
        .into_tuple::<EntityLifecycleStatus>()
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    let lifecycle = DeploymentLifecycleStatus::from(lifecycle_row);

    let pending = deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .filter(
            deployment_approval_requirements::Column::Status.eq(EntityRequirementStatus::Pending),
        )
        .lock_exclusive()
        .select_only()
        .column(deployment_approval_requirements::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?;
    let Some(requirement_id) = pending else {
        let satisfied_expired = deployment_approval_requirements::Entity::find()
            .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
            .filter(
                deployment_approval_requirements::Column::Status
                    .eq(EntityRequirementStatus::Satisfied),
            )
            .filter(deployment_approval_requirements::Column::ExpiresAt.lte(wall_clock()))
            .select_only()
            .column(deployment_approval_requirements::Column::Id)
            .into_tuple::<Uuid>()
            .one(db)
            .await?
            .is_some();
        if satisfied_expired {
            block_approval_execution(db, deployment_id, None).await?;
            return Ok(true);
        }
        return Ok(false);
    };

    let (action, issue): (&str, Option<String>) = if lifecycle.has_started_execution() {
        (
            "APPROVAL_INVALIDATED",
            Some("TERMINAL_LIFECYCLE".to_string()),
        )
    } else if requirement_expired(db, requirement_id).await? {
        (
            "APPROVAL_EXPIRED",
            Some("APPROVAL_REQUIREMENT_EXPIRED".to_string()),
        )
    } else {
        let issue = approval_evidence_issue(db, deployment_id).await?;
        if issue.is_none()
            || (issue.as_deref() == Some("APPROVAL_EVIDENCE_MISSING")
                && waiting_for_evaluation(db, deployment_id).await?)
        {
            return Ok(false);
        }
        ("APPROVAL_INVALIDATED", issue)
    };
    let status = if action == "APPROVAL_EXPIRED" {
        ApprovalRequirementStatus::Expired
    } else {
        ApprovalRequirementStatus::Invalidated
    };
    transition_requirement(db, requirement_id, status, issue.as_deref(), &[]).await?;
    system_audit(
        db,
        deployment_id,
        None,
        action,
        json!({"requirementId": requirement_id.to_string(), "code": issue}),
    )
    .await?;
    touch_projection(db, deployment_id).await?;
    if matches!(
        lifecycle,
        DeploymentLifecycleStatus::AwaitingApproval | DeploymentLifecycleStatus::Requested
    ) {
        cancel_runtime_health(
            db,
            deployment_id,
            "Approval could not complete with the frozen requirement facts.",
        )
        .await?;
        move_lifecycle(
            db,
            deployment_id,
            EntityLifecycleStatus::Canceled,
            [
                EntityLifecycleStatus::AwaitingApproval,
                EntityLifecycleStatus::Requested,
            ],
            true,
        )
        .await?;
    }
    Ok(true)
}

/// Blocks execution for a satisfied requirement the deployment can no longer honour. `actor` is
/// `None` for a system-attributed block; only `reconcile_project_archives` passes one, the
/// archive event's own actor.
pub async fn block_approval_execution(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    actor: Option<Uuid>,
) -> Result<bool, DbErr> {
    let satisfied_and_pending = match requirement_of(db, deployment_id).await? {
        Some(requirement) if requirement.status == EntityRequirementStatus::Satisfied => {
            deployments::Entity::find_by_id(deployment_id)
                .filter(deployments::Column::LifecycleStatus.is_in([
                    EntityLifecycleStatus::Requested,
                    EntityLifecycleStatus::Approved,
                ]))
                .select_only()
                .column(deployments::Column::Id)
                .into_tuple::<Uuid>()
                .one(db)
                .await?
                .is_some()
        }
        _ => false,
    };
    if !satisfied_and_pending {
        return Ok(false);
    }
    let archive_boundary = deployment_archive_boundary(db, deployment_id).await?;
    let project_active = deployments::Entity::find_by_id(deployment_id)
        .find_also_related(projects::Entity)
        .one(db)
        .await?
        .and_then(|(_, project)| project)
        .is_some_and(|project| project.lifecycle_status == LifecycleStatus::Active);
    let any_expired = deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .filter(deployment_approval_requirements::Column::ExpiresAt.lte(wall_clock()))
        .select_only()
        .column(deployment_approval_requirements::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .is_some();
    if project_active
        && !archive_boundary
        && evidence_ready(db, deployment_id).await?
        && !any_expired
    {
        return Ok(false);
    }
    let evidence_issue = approval_evidence_issue(db, deployment_id).await?;
    let block_code = if archive_boundary {
        "PROJECT_ARCHIVED".to_string()
    } else if project_active {
        evidence_issue.unwrap_or_else(|| "APPROVAL_EVIDENCE_NO_LONGER_VALID".to_string())
    } else {
        "PROJECT_NOT_ACTIVE".to_string()
    };
    cancel_runtime_health(
        db,
        deployment_id,
        "Execution stopped because frozen approval requirements were no longer executable.",
    )
    .await?;
    let updated = move_lifecycle(
        db,
        deployment_id,
        EntityLifecycleStatus::Canceled,
        [
            EntityLifecycleStatus::Requested,
            EntityLifecycleStatus::Approved,
        ],
        true,
    )
    .await?;
    if updated == 0 {
        return Ok(false);
    }
    delete_handoff_release(db, deployment_id).await?;
    system_audit(
        db,
        deployment_id,
        actor,
        "APPROVAL_EXECUTION_BLOCKED",
        json!({"code": block_code}),
    )
    .await?;
    touch_projection(db, deployment_id).await?;
    Ok(true)
}

/// Whether an approved deployment may execute. The participant count comes from the requirement
/// row's own `jsonb` column, checked in Rust against one read of its `APPROVE` decisions: the
/// requirement is `SATISFIED` by this point, so its participant list is frozen and the decisions
/// it names are immutable rows.
pub async fn approval_execution_eligible(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let Some(requirement) = deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .filter(
            deployment_approval_requirements::Column::Status.eq(EntityRequirementStatus::Satisfied),
        )
        .filter(deployment_approval_requirements::Column::ExpiresAt.gt(wall_clock()))
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    let eligible_deployment = deployments::Entity::find_by_id(deployment_id)
        .filter(deployments::Column::LifecycleStatus.is_in([
            EntityLifecycleStatus::Approved,
            EntityLifecycleStatus::Requested,
        ]))
        .find_also_related(projects::Entity)
        .one(db)
        .await?
        .and_then(|(_, project)| project)
        .is_some_and(|project| project.lifecycle_status == LifecycleStatus::Active);
    if !eligible_deployment {
        return Ok(false);
    }
    let participants = string_list(&requirement.satisfied_participants);
    let required = requirement.required_approvers;
    let distinct: HashSet<&String> = participants.iter().collect();
    if participants.len() as i32 != required || distinct.len() as i32 != required {
        return Ok(false);
    }
    let approvers: HashSet<String> = deployment_approval_decisions::Entity::find()
        .filter(deployment_approval_decisions::Column::ApprovalRequirementId.eq(requirement.id))
        .filter(
            deployment_approval_decisions::Column::Decision
                .eq(crate::entity::enums::ApprovalDecision::Approve),
        )
        .select_only()
        .column(deployment_approval_decisions::Column::ActorPrincipalId)
        .into_tuple::<Uuid>()
        .all(db)
        .await?
        .into_iter()
        .map(|id| id.to_string())
        .collect();
    if !participants
        .iter()
        .all(|participant| approvers.contains(participant))
    {
        return Ok(false);
    }
    if deployment_archive_boundary(db, deployment_id).await? {
        return Ok(false);
    }
    evidence_ready(db, deployment_id).await
}

/// Creates the deployment's approval requirement if it has none, reading the deployment and its
/// policy snapshot first and inserting by key. The status, the expiry and any invalidation code
/// follow from the deployment's own lifecycle and requested-at instant.
pub async fn ensure_requirement(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let archived = deployment_archive_boundary(db, deployment_id).await?;
    let source = deployments::Entity::find_by_id(deployment_id)
        .find_also_related(deployment_policy_snapshots::Entity)
        .one(db)
        .await?;
    if let Some((deployment, Some(policy))) = source {
        let terminal = archived
            || matches!(
                deployment.lifecycle_status,
                EntityLifecycleStatus::InProgress
                    | EntityLifecycleStatus::Active
                    | EntityLifecycleStatus::Failed
                    | EntityLifecycleStatus::Canceled
                    | EntityLifecycleStatus::RolledBack
            );
        let invalidation_code = if archived {
            Some(ApprovalInvalidationCode::ProjectArchived)
        } else if terminal {
            Some(ApprovalInvalidationCode::TerminalLifecycle)
        } else {
            None
        };
        let requirement_id = Uuid::new_v4();
        let inserted = deployment_approval_requirements::Entity::insert(
            deployment_approval_requirements::ActiveModel {
                id: Set(requirement_id),
                deployment_id: Set(deployment.id),
                revision: Set(1),
                organization_id: Set(deployment.organization_id),
                project_id: Set(deployment.project_id),
                requested_at: Set(deployment.requested_at),
                required_approvers: Set(policy.required_approvers),
                status: Set(if terminal {
                    EntityRequirementStatus::Invalidated
                } else {
                    EntityRequirementStatus::Pending
                }),
                expires_at: Set(deployment.requested_at + chrono::Duration::hours(24)),
                satisfied_at: NotSet,
                rejected_at: NotSet,
                invalidated_at: Set(terminal.then(wall_clock)),
                invalidation_code: Set(invalidation_code),
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
        // Zero rows affected is the `ON CONFLICT (deployment_id) DO NOTHING` branch: the
        // requirement already exists.
        if matches!(inserted, TryInsertResult::Inserted(rows) if rows > 0) && terminal {
            if archived {
                invalidate_archived_approval_requirement(db, deployment_id, requirement_id).await?;
            } else {
                record_terminal_invalidation(db, deployment_id, requirement_id).await?;
            }
        }
    }
    Ok(requirement_of(db, deployment_id).await?.is_some())
}

/// `ensure_requirement`'s archived-at-creation branch: a requirement created under an archived
/// project is invalidated immediately, attributed to the archive event's actor.
async fn invalidate_archived_approval_requirement(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    requirement_id: Uuid,
) -> Result<(), DbErr> {
    use deployment_approval_project_archive_events::Column;
    let archive_actor = match deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    {
        Some(deployment) => deployment_approval_project_archive_events::Entity::find()
            .filter(Column::ProjectId.eq(deployment.project_id))
            .filter(archive_boundary_condition(&deployment))
            .order_by_desc(Column::ArchivedProjectRevision)
            .order_by_desc(Column::ArchivedAt)
            .order_by_desc(Column::Id)
            .one(db)
            .await?
            .and_then(|event| event.actor_principal_id),
        None => None,
    };
    system_audit(
        db,
        deployment_id,
        archive_actor,
        "APPROVAL_INVALIDATED",
        json!({"requirementId": requirement_id.to_string(), "code": "PROJECT_ARCHIVED"}),
    )
    .await?;
    cancel_runtime_health(
        db,
        deployment_id,
        "Project archive terminalized this delayed approval cycle.",
    )
    .await?;
    move_lifecycle(
        db,
        deployment_id,
        EntityLifecycleStatus::Canceled,
        [
            EntityLifecycleStatus::AwaitingApproval,
            EntityLifecycleStatus::Requested,
        ],
        true,
    )
    .await?;
    delete_handoff_release(db, deployment_id).await?;
    touch_projection(db, deployment_id).await
}

/// `ensure_requirement`'s non-archived invalidation branch.
async fn record_terminal_invalidation(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    requirement_id: Uuid,
) -> Result<(), DbErr> {
    system_audit(
        db,
        deployment_id,
        None,
        "APPROVAL_INVALIDATED",
        json!({"requirementId": requirement_id.to_string(), "code": "TERMINAL_LIFECYCLE"}),
    )
    .await?;
    touch_projection(db, deployment_id).await
}

/// Whether an `EXECUTE_DEPLOYMENT` event is already queued or in flight for this deployment.
async fn pending_execute_event(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    use deployment_outbox_events::Column;
    Ok(deployment_outbox_events::Entity::find()
        .filter(Column::DeploymentId.eq(deployment_id))
        .filter(Column::EventType.eq(DeploymentOutboxEventType::ExecuteDeployment))
        .filter(Column::Status.is_in([
            DeploymentOutboxStatus::Pending,
            DeploymentOutboxStatus::Processing,
        ]))
        .select_only()
        .column(Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .is_some())
}

/// Atomically validates evidence, satisfies a zero-approver rule, and queues local execution.
/// Every caller wants the execution enqueued, so there is no option to skip it.
pub async fn automatic_approval_handoff(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    if !ensure_requirement(db, deployment_id).await? {
        return Ok(false);
    }
    if deployment_archive_boundary(db, deployment_id).await? {
        return Ok(false);
    }
    reconcile_pending(db, deployment_id).await?;

    // The requirement row and then the deployment row are locked `FOR UPDATE`, two locked reads in
    // the same transaction, in that order.
    let Some(requirement) = deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .lock_exclusive()
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .lock_exclusive()
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    let requirement_id = requirement.id;
    let mut requirement_status = ApprovalRequirementStatus::from(requirement.status);
    let required_approvers = requirement.required_approvers;
    let lifecycle = DeploymentLifecycleStatus::from(deployment.lifecycle_status);
    let requirement_expiry = requirement.expires_at;

    let requested_or_approved = lifecycle.awaits_execution();
    if requirement_status == ApprovalRequirementStatus::Satisfied
        && requested_or_approved
        && requirement_expiry <= wall_clock()
    {
        block_approval_execution(db, deployment_id, None).await?;
        return Ok(false);
    }
    if requirement_status == ApprovalRequirementStatus::Pending
        && required_approvers == 0
        && !lifecycle.has_started_execution()
        && requirement_expiry > wall_clock()
        && evidence_ready(db, deployment_id).await?
    {
        deployment_approval_requirements::Entity::update_many()
            .col_expr(
                deployment_approval_requirements::Column::Status,
                Expr::value(EntityRequirementStatus::Satisfied.to_value()),
            )
            .col_expr(
                deployment_approval_requirements::Column::Revision,
                Expr::col(deployment_approval_requirements::Column::Revision).add(1),
            )
            .col_expr(
                deployment_approval_requirements::Column::SatisfiedAt,
                Expr::current_timestamp(),
            )
            .col_expr(
                deployment_approval_requirements::Column::SatisfiedParticipants,
                Expr::value(participants_json(&[])),
            )
            .filter(deployment_approval_requirements::Column::Id.eq(requirement_id))
            .filter(
                deployment_approval_requirements::Column::Status
                    .eq(EntityRequirementStatus::Pending),
            )
            .exec(db)
            .await?;
        system_audit(
            db,
            deployment_id,
            None,
            "APPROVAL_SATISFIED",
            json!({"requirementId": requirement_id.to_string(), "participantIds": []}),
        )
        .await?;
        touch_projection(db, deployment_id).await?;
        requirement_status = ApprovalRequirementStatus::Satisfied;
    }
    if requirement_status == ApprovalRequirementStatus::Satisfied
        && requested_or_approved
        && !evidence_ready(db, deployment_id).await?
        && !waiting_for_evaluation(db, deployment_id).await?
    {
        block_approval_execution(db, deployment_id, None).await?;
        return Ok(false);
    }
    if requirement_status == ApprovalRequirementStatus::Satisfied && requested_or_approved {
        if !waiting_for_evaluation(db, deployment_id).await? {
            deployment_approval_handoff_releases::Entity::insert(
                deployment_approval_handoff_releases::ActiveModel {
                    deployment_id: Set(deployment_id),
                    created_at: NotSet,
                },
            )
            .on_conflict(
                OnConflict::column(deployment_approval_handoff_releases::Column::DeploymentId)
                    .do_nothing()
                    .to_owned(),
            )
            .try_insert()
            .exec_without_returning(db)
            .await?;
            let pending_execute = pending_execute_event(db, deployment_id).await?;
            if worker_ready(db).await? && !pending_execute {
                crate::deployment::writes::enqueue(
                    db,
                    deployment_id,
                    "EXECUTE_DEPLOYMENT",
                    "SUCCESS",
                )
                .await?;
            }
            if pending_execute_event(db, deployment_id).await? {
                delete_handoff_release(db, deployment_id).await?;
            }
        }
        return Ok(true);
    }
    Ok(requirement_status == ApprovalRequirementStatus::Satisfied
        && evidence_ready(db, deployment_id).await?)
}

async fn expired_approval_requirement_deployments(
    db: &impl ConnectionTrait,
) -> Result<Vec<Uuid>, DbErr> {
    use deployment_approval_requirements::Column;
    deployment_approval_requirements::Entity::find()
        .filter(Column::ExpiresAt.lte(wall_clock()))
        .filter(Column::Status.is_in([
            EntityRequirementStatus::Pending,
            EntityRequirementStatus::Satisfied,
        ]))
        .order_by_asc(Column::ExpiresAt)
        .order_by_asc(Column::Id)
        .limit(50)
        .select_only()
        .column(Column::DeploymentId)
        .into_tuple::<Uuid>()
        .all(db)
        .await
}

/// Fetches one page of expired requirements and reconciles each in its own transaction, so one bad
/// row cannot abort the batch. Every call site already relies on `reconcile_pending`'s own
/// conditional-UPDATE race safety, so no advisory lock is taken here — matching the
/// DSQL-compatibility reasoning `reconcile_pending` itself documents.
pub(crate) async fn reconcile_expired_approval_requirements(
    db: &DatabaseConnection,
) -> Result<crate::ApprovalMaintenanceHealth, DbErr> {
    let deployments = expired_approval_requirement_deployments(db).await?;
    let mut attempted = 0i64;
    let mut reconciled = 0i64;
    let mut failed = 0i64;
    for deployment_id in deployments {
        attempted += 1;
        let txn = db.begin().await?;
        match reconcile_pending(&txn, deployment_id).await {
            Ok(_) => {
                txn.commit().await?;
                reconciled += 1;
            }
            Err(_) => {
                let _ = txn.rollback().await;
                failed += 1;
            }
        }
    }
    Ok(if failed > 0 {
        crate::ApprovalMaintenanceHealth {
            healthy: false,
            failure_code: Some("PARTIAL_RECONCILIATION".to_string()),
            attempted,
            reconciled,
            failed,
        }
    } else {
        crate::ApprovalMaintenanceHealth {
            healthy: true,
            failure_code: None,
            attempted,
            reconciled,
            failed: 0,
        }
    })
}

/// The scheduled maintenance task's expiry entry point, publishing its outcome directly into the
/// shared `/health` state.
pub async fn reconcile_approval_expiry(
    db: &DatabaseConnection,
    state: &crate::ApprovalMaintenanceState,
) {
    state.set_maintenance(crate::ApprovalMaintenanceHealth::in_progress());
    let health = match reconcile_expired_approval_requirements(db).await {
        Ok(health) => health,
        Err(error) => crate::ApprovalMaintenanceHealth {
            healthy: false,
            failure_code: Some(super::worker::db_failure_code(&error)),
            attempted: 0,
            reconciled: 0,
            failed: 0,
        },
    };
    state.set_maintenance(health);
}

async fn approval_project_archive_pending(db: &impl ConnectionTrait) -> Result<bool, DbErr> {
    Ok(deployment_approval_project_archive_events::Entity::find()
        .filter(deployment_approval_project_archive_events::Column::ProcessedAt.is_null())
        .select_only()
        .column(deployment_approval_project_archive_events::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .is_some())
}

/// The candidate deployments one archive event's boundary covers: the same five-term `OR`, now
/// non-correlated because the event's own revision and instant are read first.
fn archived_candidates(event: &deployment_approval_project_archive_events::Model) -> Condition {
    match event.archived_project_revision {
        Some(revision) => Condition::any()
            .add(
                Condition::all()
                    .add(deployments::Column::ProjectLifecycleRevision.is_not_null())
                    .add(deployments::Column::ProjectLifecycleRevision.lte(revision)),
            )
            .add(
                Condition::all()
                    .add(deployments::Column::ProjectLifecycleRevision.is_null())
                    .add(deployments::Column::RequestedAt.lte(event.archived_at)),
            ),
        None => Condition::all().add(deployments::Column::RequestedAt.lte(event.archived_at)),
    }
}

/// The lifecycle/requirement-status pairs archive reconciliation terminalizes.
fn archived_pending_condition() -> Condition {
    Condition::any()
        .add(
            Condition::all()
                .add(deployments::Column::LifecycleStatus.is_in([
                    EntityLifecycleStatus::AwaitingApproval,
                    EntityLifecycleStatus::Requested,
                ]))
                .add(
                    deployment_approval_requirements::Column::Status
                        .eq(EntityRequirementStatus::Pending),
                ),
        )
        .add(
            Condition::all()
                .add(deployments::Column::LifecycleStatus.is_in([
                    EntityLifecycleStatus::Approved,
                    EntityLifecycleStatus::Requested,
                ]))
                .add(
                    deployment_approval_requirements::Column::Status
                        .eq(EntityRequirementStatus::Satisfied),
                ),
        )
}

#[derive(sea_orm::FromQueryResult)]
struct ArchiveCandidate {
    id: Uuid,
    deployment_id: Uuid,
    status: EntityRequirementStatus,
}

/// Terminalizes every pending/satisfied approval cycle an archived project's boundary now covers.
/// One transaction per archive event (not per candidate row, unlike
/// `reconcile_expired_approval_requirements`), because the `FOR UPDATE` locks on the deployment and
/// requirement rows must span the whole candidate page and every write that follows. A partial
/// failure leaves `processed_at` NULL, so the whole event retries on the next tick; that is safe
/// because every write here is a conditional UPDATE already idempotent against a re-run.
async fn reconcile_project_archives(db: &DatabaseConnection) -> Result<(), DbErr> {
    use deployment_approval_project_archive_events::Column;
    let events = deployment_approval_project_archive_events::Entity::find()
        .filter(Column::ProcessedAt.is_null())
        .order_by_asc(Column::ArchivedAt)
        .order_by_asc(Column::Id)
        .limit(10)
        .all(db)
        .await?;

    for event in events {
        let txn = db.begin().await?;
        let candidate_select = || {
            deployment_approval_requirements::Entity::find()
                .join(
                    JoinType::InnerJoin,
                    deployment_approval_requirements::Relation::Deployments.def(),
                )
                .filter(deployments::Column::ProjectId.eq(event.project_id))
                .filter(archived_candidates(&event))
                .filter(archived_pending_condition())
        };
        let mut candidate_query = candidate_select()
            .order_by_asc(deployments::Column::Id)
            .limit(50)
            .select_only()
            .column(deployment_approval_requirements::Column::Id)
            .column_as(deployments::Column::Id, "deployment_id")
            .column(deployment_approval_requirements::Column::Status);
        QuerySelect::query(&mut candidate_query).lock_with_tables(
            LockType::Update,
            [
                deployments::Entity.into_table_ref(),
                deployment_approval_requirements::Entity.into_table_ref(),
            ],
        );
        let candidates = candidate_query
            .into_model::<ArchiveCandidate>()
            .all(&txn)
            .await?;

        for candidate in candidates {
            if candidate.status == EntityRequirementStatus::Satisfied {
                block_approval_execution(&txn, candidate.deployment_id, event.actor_principal_id)
                    .await?;
                continue;
            }
            let updated = deployment_approval_requirements::Entity::update_many()
                .col_expr(
                    deployment_approval_requirements::Column::Status,
                    Expr::value(EntityRequirementStatus::Invalidated.to_value()),
                )
                .col_expr(
                    deployment_approval_requirements::Column::Revision,
                    Expr::col(deployment_approval_requirements::Column::Revision).add(1),
                )
                .col_expr(
                    deployment_approval_requirements::Column::InvalidatedAt,
                    Expr::current_timestamp(),
                )
                .col_expr(
                    deployment_approval_requirements::Column::InvalidationCode,
                    Expr::value(ApprovalInvalidationCode::ProjectArchived.to_value()),
                )
                .filter(deployment_approval_requirements::Column::Id.eq(candidate.id))
                .filter(
                    deployment_approval_requirements::Column::Status
                        .eq(EntityRequirementStatus::Pending),
                )
                .exec(&txn)
                .await?;
            if updated.rows_affected != 1 {
                continue;
            }
            audit(
                &txn,
                candidate.deployment_id,
                event.actor_principal_id,
                "APPROVAL_INVALIDATED",
                json!({"requirementId": candidate.id.to_string(), "code": "PROJECT_ARCHIVED"}),
            )
            .await?;
            cancel_runtime_health(
                &txn,
                candidate.deployment_id,
                "Project archive terminalized this pending approval cycle.",
            )
            .await?;
            move_lifecycle(
                &txn,
                candidate.deployment_id,
                EntityLifecycleStatus::Canceled,
                [
                    EntityLifecycleStatus::AwaitingApproval,
                    EntityLifecycleStatus::Requested,
                ],
                true,
            )
            .await?;
            touch_projection(&txn, candidate.deployment_id).await?;
        }

        let still_pending = candidate_select()
            .select_only()
            .column(deployment_approval_requirements::Column::Id)
            .into_tuple::<Uuid>()
            .one(&txn)
            .await?
            .is_some();
        if !still_pending {
            deployment_approval_project_archive_events::Entity::update_many()
                .col_expr(Column::ProcessedAt, Expr::current_timestamp())
                .filter(Column::Id.eq(event.id))
                .exec(&txn)
                .await?;
        }
        txn.commit().await?;
    }
    Ok(())
}

/// The scheduled maintenance task's archive entry point: it reconciles project archives and
/// reports whether any archive event is still unprocessed.
pub async fn reconcile_approval_upgrade(
    db: &DatabaseConnection,
    state: &crate::ApprovalMaintenanceState,
) {
    let outcome = async {
        reconcile_project_archives(db).await?;
        approval_project_archive_pending(db).await
    }
    .await;
    let health = match outcome {
        Ok(archive_pending) => crate::ApprovalMaintenanceHealth {
            healthy: !archive_pending,
            failure_code: if archive_pending {
                Some("ARCHIVE_RECONCILIATION_PENDING".to_string())
            } else {
                None
            },
            attempted: 0,
            reconciled: 0,
            failed: 0,
        },
        Err(error) => crate::ApprovalMaintenanceHealth {
            healthy: false,
            failure_code: Some(format!(
                "ARCHIVE_RECONCILIATION_{}",
                super::worker::db_failure_code(&error)
            )),
            attempted: 0,
            reconciled: 0,
            failed: 0,
        },
    };
    state.set_upgrade(health);
}

/// `worker_ready` is read once by the caller (not re-evaluated per row): every row this selects
/// already re-checks worker readiness inside `automatic_approval_handoff` for the
/// `required_approvers > 0` case, but that later re-check alone would under-filter the candidate
/// set the `required_approvers = 0 OR $1` clause narrows. The two `NOT EXISTS` anti-joins stay
/// anti-joins: the archive one as a correlated `Expr::exists(..).not()` over the deployment's own
/// project, the outbox one as a `not_in_subquery`.
async fn compatible_approval_handoff_deployments(
    db: &impl ConnectionTrait,
    worker_ready: bool,
) -> Result<Vec<Uuid>, DbErr> {
    use deployment_approval_handoff_releases::Column as ReleaseColumn;
    let eligible_state = Condition::any()
        .add(
            Condition::all()
                .add(deployments::Column::LifecycleStatus.eq(EntityLifecycleStatus::Approved))
                .add(
                    deployment_approval_requirements::Column::Status
                        .eq(EntityRequirementStatus::Satisfied),
                ),
        )
        .add(
            Condition::all()
                .add(deployments::Column::LifecycleStatus.eq(EntityLifecycleStatus::Requested))
                .add(deployment_approval_requirements::Column::RequiredApprovers.eq(0))
                .add(deployment_approval_requirements::Column::Status.is_in([
                    EntityRequirementStatus::Pending,
                    EntityRequirementStatus::Satisfied,
                ])),
        );
    // `(requirement.required_approvers = 0 OR $1)`: with a ready worker the clause is vacuous.
    let approver_gate = if worker_ready {
        Condition::all()
    } else {
        Condition::all().add(deployment_approval_requirements::Column::RequiredApprovers.eq(0))
    };
    // The correlated archive-boundary anti-join, with the deployment's own columns referenced from
    // the outer query.
    let archive_event = deployment_approval_project_archive_events::Entity;
    let archive_column = |column: deployment_approval_project_archive_events::Column| {
        Expr::col((archive_event, column))
    };
    let deployment_column = |column: deployments::Column| Expr::col((deployments::Entity, column));
    let boundary = Condition::any()
        .add(
            Condition::all()
                .add(deployment_column(deployments::Column::ProjectLifecycleRevision).is_not_null())
                .add(
                    archive_column(
                        deployment_approval_project_archive_events::Column::ArchivedProjectRevision,
                    )
                    .is_not_null(),
                )
                .add(
                    deployment_column(deployments::Column::ProjectLifecycleRevision).lte(
                        archive_column(
                            deployment_approval_project_archive_events::Column::ArchivedProjectRevision,
                        ),
                    ),
                ),
        )
        .add(
            Condition::all()
                .add(
                    Condition::any()
                        .add(
                            deployment_column(deployments::Column::ProjectLifecycleRevision)
                                .is_null(),
                        )
                        .add(
                            archive_column(
                                deployment_approval_project_archive_events::Column::ArchivedProjectRevision,
                            )
                            .is_null(),
                        ),
                )
                .add(
                    deployment_column(deployments::Column::RequestedAt).lte(archive_column(
                        deployment_approval_project_archive_events::Column::ArchivedAt,
                    )),
                ),
        );
    let archived = Query::select()
        .expr(Expr::val(1))
        .from(archive_event)
        .cond_where(
            Condition::all()
                .add(
                    archive_column(deployment_approval_project_archive_events::Column::ProjectId)
                        .eq(deployment_column(deployments::Column::ProjectId)),
                )
                .add(boundary),
        )
        .to_owned();
    let queued = Query::select()
        .column(deployment_outbox_events::Column::DeploymentId)
        .from(deployment_outbox_events::Entity)
        .cond_where(
            Condition::all()
                .add(
                    Expr::col(deployment_outbox_events::Column::EventType)
                        .eq(DeploymentOutboxEventType::ExecuteDeployment.to_value()),
                )
                .add(Expr::col(deployment_outbox_events::Column::Status).is_in([
                    DeploymentOutboxStatus::Pending.to_value(),
                    DeploymentOutboxStatus::Processing.to_value(),
                ])),
        )
        .to_owned();
    deployment_approval_handoff_releases::Entity::find()
        .join(
            JoinType::InnerJoin,
            deployment_approval_handoff_releases::Relation::Deployments.def(),
        )
        .join(
            JoinType::InnerJoin,
            deployments::Relation::DeploymentApprovalRequirements.def(),
        )
        .filter(eligible_state)
        .filter(approver_gate)
        .filter(Expr::exists(archived).not())
        .filter(deployment_approval_requirements::Column::ExpiresAt.gt(wall_clock()))
        .filter(deployments::Column::Id.not_in_subquery(queued))
        .order_by_asc(ReleaseColumn::CreatedAt)
        .order_by_asc(ReleaseColumn::DeploymentId)
        .limit(50)
        .select_only()
        .column(ReleaseColumn::DeploymentId)
        .into_tuple::<Uuid>()
        .all(db)
        .await
}

async fn release_compatible_approval_handoffs_inner(
    db: &DatabaseConnection,
) -> Result<bool, DbErr> {
    let ready = worker_ready(db).await?;
    let deployments = compatible_approval_handoff_deployments(db, ready).await?;
    let mut all_succeeded = true;
    for deployment_id in deployments {
        let txn = db.begin().await?;
        match automatic_approval_handoff(&txn, deployment_id).await {
            Ok(_) => txn.commit().await?,
            Err(_) => {
                let _ = txn.rollback().await;
                all_succeeded = false;
            }
        }
    }
    Ok(all_succeeded)
}

/// `true` only if the candidate page was read and every row's handoff attempt succeeded. Every
/// failure mode folds into the same `APPROVAL_MAINTENANCE_FAILED` heartbeat report.
pub(crate) async fn release_compatible_approval_handoffs(db: &DatabaseConnection) -> bool {
    match release_compatible_approval_handoffs_inner(db).await {
        Ok(all_succeeded) => all_succeeded,
        Err(error) => {
            tracing::warn!(code = %super::worker::db_failure_code(&error), "approval handoff maintenance failed");
            false
        }
    }
}

/// Whether this worker's own currently-stored heartbeat row already reports a sticky, unrecovered
/// maintenance failure.
pub(crate) async fn approval_maintenance_failed(
    db: &impl ConnectionTrait,
    worker: &str,
) -> Result<bool, DbErr> {
    Ok(
        deployment_worker_heartbeats::Entity::find_by_id(worker.trim().to_string())
            .one(db)
            .await?
            .is_some_and(|heartbeat| {
                heartbeat.state == WorkerHeartbeatState::Degraded
                    && heartbeat.failure_code.as_deref() == Some("APPROVAL_MAINTENANCE_FAILED")
            }),
    )
}

#[cfg(test)]
mod frozen_cycle_tests {
    use super::{
        archive_boundary_condition, archived_candidates, archived_pending_condition,
        evidence_issue_of, policy_matches, sql_eq, string_list, EvidenceFacts, PolicyRow,
    };
    use crate::entity::enums::{
        DeploymentEvidenceKind, DeploymentLifecycleStatus, DeploymentRisk, DeploymentStrategy,
        LogicalEnvironmentClass,
    };
    use crate::entity::{
        deployment_approval_project_archive_events, deployment_evidence_snapshots,
        deployment_plan_versions, deployment_policy_snapshots, deployments,
    };
    use sea_orm::prelude::DateTimeWithTimeZone;
    use sea_orm::sea_query::{PostgresQueryBuilder, Query};
    use sea_orm::Condition;
    use serde_json::json;
    use std::collections::HashSet;
    use uuid::{uuid, Uuid};

    const DEPLOYMENT: Uuid = uuid!("11111111-1111-1111-1111-111111111111");
    const AGENT_VERSION: Uuid = uuid!("22222222-2222-2222-2222-222222222222");
    const ENVIRONMENT_VERSION: Uuid = uuid!("33333333-3333-3333-3333-333333333333");
    const SNAPSHOT: Uuid = uuid!("44444444-4444-4444-4444-444444444444");

    fn instant(text: &str) -> DateTimeWithTimeZone {
        chrono::DateTime::parse_from_rfc3339(text).expect("an RFC 3339 instant")
    }

    /// Far enough ahead of any `wall_clock()` a test run observes that `evidence_issue_of`, which
    /// takes its own clock, sees these snapshots as unexpired.
    fn distant_future() -> DateTimeWithTimeZone {
        instant("2999-01-01T00:00:00Z")
    }

    fn distant_past() -> DateTimeWithTimeZone {
        instant("2000-01-01T00:00:00Z")
    }

    /// A deployment, its frozen policy snapshot and its plan that `policy_matches` accepts. Each
    /// test changes one cell and asserts the refusal that cell alone causes.
    fn frozen_cycle() -> (
        deployments::Model,
        deployment_policy_snapshots::Model,
        deployment_plan_versions::Model,
    ) {
        let deployment = deployments::Model {
            organization_id: Uuid::nil(),
            project_id: Uuid::nil(),
            agent_id: Uuid::nil(),
            agent_version_id: AGENT_VERSION,
            catalog_release_id: "release".to_string(),
            catalog_release_digest: "c".repeat(64),
            environment: LogicalEnvironmentClass::Production,
            target_digest: "t".repeat(64),
            strategy: DeploymentStrategy::Rolling,
            lifecycle_status: DeploymentLifecycleStatus::AwaitingApproval,
            revision: 1,
            idempotency_key: "idempotency-key".to_string(),
            requested_by: Uuid::nil(),
            requested_at: instant("2026-01-01T00:00:00Z"),
            updated_at: instant("2026-01-01T00:00:00Z"),
            environment_definition_version_id: Some(ENVIRONMENT_VERSION),
            request_fingerprint: None,
            projection_revision: None,
            project_lifecycle_revision: Some(7),
            id: DEPLOYMENT,
        };
        let policy = deployment_policy_snapshots::Model {
            policy_id: Uuid::nil(),
            policy_revision: 1,
            policy_digest: "p".repeat(64),
            policy_matrix: json!({ "PRODUCTION_HIGH": {
                "requiredApprovers": 2,
                "requiredEvidence": ["PLAN_VALIDATED"],
            } }),
            logical_environment_class: LogicalEnvironmentClass::Production,
            risk: DeploymentRisk::High,
            required_evidence: json!(["PLAN_VALIDATED"]),
            required_approvers: 2,
            created_at: instant("2026-01-01T00:00:00Z"),
            agent_version_id: Some(AGENT_VERSION),
            environment_definition_version_id: Some(ENVIRONMENT_VERSION),
            target_digest: Some("t".repeat(64)),
            plan_digest: Some("d".repeat(64)),
            package_digest: Some("k".repeat(64)),
            binding_digest: Some("b".repeat(64)),
            evaluation_requirement_expires_at: None,
            risk_verification_digest: None,
            deployment_id: DEPLOYMENT,
        };
        let plan = deployment_plan_versions::Model {
            deployment_id: DEPLOYMENT,
            version_number: 1,
            agent_version_id: AGENT_VERSION,
            catalog_release_id: "release".to_string(),
            environment: LogicalEnvironmentClass::Production,
            compiler_version: "1".to_string(),
            canonical_plan: json!({}),
            plan_digest: "d".repeat(64),
            package_digest: "k".repeat(64),
            package_reference: "reference".to_string(),
            created_by: Uuid::nil(),
            created_at: instant("2026-01-01T00:00:00Z"),
            environment_definition_version_id: Some(ENVIRONMENT_VERSION),
            agent_content_digest: None,
            catalog_release_digest: None,
            target_digest: Some("t".repeat(64)),
            id: Uuid::nil(),
        };
        (deployment, policy, plan)
    }

    fn evidence(
        kind: DeploymentEvidenceKind,
        expires_at: Option<DateTimeWithTimeZone>,
    ) -> deployment_evidence_snapshots::Model {
        let (_, policy, _) = frozen_cycle();
        deployment_evidence_snapshots::Model {
            deployment_id: DEPLOYMENT,
            evidence_kind: kind,
            evidence_digest: "e".repeat(64),
            expires_at,
            created_at: instant("2026-01-01T00:00:00Z"),
            agent_version_id: policy.agent_version_id,
            environment_definition_version_id: policy.environment_definition_version_id,
            target_digest: policy.target_digest.clone(),
            plan_digest: policy.plan_digest.clone(),
            package_digest: policy.package_digest.clone(),
            binding_digest: policy.binding_digest.clone(),
            source_evaluation_run_id: None,
            id: SNAPSHOT,
        }
    }

    fn facts(snapshots: Vec<deployment_evidence_snapshots::Model>) -> EvidenceFacts {
        EvidenceFacts {
            snapshots,
            invalidated: HashSet::new(),
        }
    }

    fn policy_row() -> PolicyRow {
        PolicyRow::from(&frozen_cycle().1)
    }

    /// The condition as the SQL the `WHERE` clause applies, so each rule is asserted on the rows
    /// it admits rather than on the builder calls that produced it.
    fn rendered(condition: Condition) -> String {
        Query::select()
            .expr(sea_orm::sea_query::Expr::val(1))
            .cond_where(condition)
            .to_string(PostgresQueryBuilder)
    }

    #[test]
    fn sql_eq_is_true_only_when_both_sides_are_present_and_equal() {
        assert!(sql_eq(&Some(1), &Some(1)));
        assert!(!sql_eq(&Some(1), &Some(2)));
        assert!(!sql_eq(&Some(1), &None));
        assert!(!sql_eq(&None, &Some(1)));
        // The reason this function exists: Rust's `==` answers true here and SQL's does not.
        assert!(!sql_eq::<i32>(&None, &None));
    }

    #[test]
    fn string_list_reads_a_jsonb_array_of_strings_and_nothing_else() {
        assert_eq!(
            string_list(&json!(["PLAN_VALIDATED", "EVALUATION_PASSED"])),
            vec![
                "PLAN_VALIDATED".to_string(),
                "EVALUATION_PASSED".to_string()
            ]
        );
        assert!(string_list(&json!([])).is_empty());
        assert!(string_list(&json!(null)).is_empty());
        assert!(string_list(&json!({ "a": "b" })).is_empty());
        assert!(string_list(&json!("PLAN_VALIDATED")).is_empty());
        assert_eq!(
            string_list(&json!(["PLAN_VALIDATED", 3, null])),
            vec!["PLAN_VALIDATED".to_string()]
        );
    }

    #[test]
    fn policy_matches_the_cycle_it_was_frozen_against() {
        let (deployment, policy, plan) = frozen_cycle();
        assert!(policy_matches(&deployment, &policy, &plan));
    }

    /// A matrix with no cell for the class and risk the snapshot froze is not a weaker rule; it is
    /// no rule, and `->` returning `NULL` must refuse rather than default.
    #[test]
    fn policy_does_not_match_when_the_matrix_has_no_cell_for_its_class_and_risk() {
        let (deployment, mut policy, plan) = frozen_cycle();
        policy.policy_matrix = json!({ "STAGING_HIGH": {
            "requiredApprovers": 2,
            "requiredEvidence": ["PLAN_VALIDATED"],
        } });
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    /// A cell that is present but empty is a different refusal from a cell that is absent, and
    /// both must refuse: the rule's two keys are read, not its presence.
    #[test]
    fn policy_does_not_match_a_cell_that_is_present_but_empty() {
        let (deployment, mut policy, plan) = frozen_cycle();
        policy.policy_matrix = json!({ "PRODUCTION_HIGH": {} });
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    #[test]
    fn policy_does_not_match_a_cell_whose_approver_count_or_evidence_differs() {
        let (deployment, policy, plan) = frozen_cycle();
        let mut fewer_approvers = policy.clone();
        fewer_approvers.policy_matrix = json!({ "PRODUCTION_HIGH": {
            "requiredApprovers": 1,
            "requiredEvidence": ["PLAN_VALIDATED"],
        } });
        assert!(!policy_matches(&deployment, &fewer_approvers, &plan));

        let mut other_evidence = policy;
        other_evidence.policy_matrix = json!({ "PRODUCTION_HIGH": {
            "requiredApprovers": 2,
            "requiredEvidence": ["EVALUATION_PASSED"],
        } });
        assert!(!policy_matches(&deployment, &other_evidence, &plan));
    }

    /// Both sides absent is the case SQL refuses and Rust's `==` would admit. A deployment with no
    /// environment definition must not match a snapshot that froze none either.
    #[test]
    fn policy_does_not_match_when_both_environment_definition_versions_are_absent() {
        let (mut deployment, mut policy, plan) = frozen_cycle();
        deployment.environment_definition_version_id = None;
        policy.environment_definition_version_id = None;
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    #[test]
    fn policy_does_not_match_when_the_plan_carries_no_target_digest() {
        let (deployment, policy, mut plan) = frozen_cycle();
        plan.target_digest = None;
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    #[test]
    fn policy_does_not_match_a_deployment_in_another_environment_class() {
        let (mut deployment, policy, plan) = frozen_cycle();
        deployment.environment = LogicalEnvironmentClass::Staging;
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    #[test]
    fn policy_does_not_match_a_plan_with_another_package_digest() {
        let (deployment, policy, mut plan) = frozen_cycle();
        plan.package_digest = "0".repeat(64);
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    /// The expiry comparison is strict on one side and inclusive on the other, so a snapshot that
    /// expires at exactly the instant under test is expired, never valid, and the two answers
    /// never both hold.
    #[test]
    fn evidence_expiring_at_the_instant_is_expired_and_not_valid() {
        let now = instant("2026-06-01T12:00:00Z");
        let policy = policy_row();
        let kind = "PLAN_VALIDATED";

        let at = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(now),
        )]);
        assert!(!at.valid(&policy, kind, now));
        assert!(at.expired(&policy, kind, now));

        let after = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(instant("2026-06-01T12:00:01Z")),
        )]);
        assert!(after.valid(&policy, kind, now));
        assert!(!after.expired(&policy, kind, now));

        let before = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(instant("2026-06-01T11:59:59Z")),
        )]);
        assert!(!before.valid(&policy, kind, now));
        assert!(before.expired(&policy, kind, now));
    }

    #[test]
    fn evidence_with_no_expiry_never_expires() {
        let now = instant("2026-06-01T12:00:00Z");
        let policy = policy_row();
        let facts = facts(vec![evidence(DeploymentEvidenceKind::PlanValidated, None)]);
        assert!(facts.valid(&policy, "PLAN_VALIDATED", now));
        assert!(!facts.expired(&policy, "PLAN_VALIDATED", now));
    }

    /// An invalidated snapshot is neither valid nor expired, but it was still observed — which is
    /// what turns the refusal from `MISSING` into `MISMATCH`.
    #[test]
    fn an_invalidated_snapshot_is_neither_valid_nor_expired_but_stays_observed() {
        let now = instant("2026-06-01T12:00:00Z");
        let policy = policy_row();
        let facts = EvidenceFacts {
            snapshots: vec![evidence(
                DeploymentEvidenceKind::PlanValidated,
                Some(instant("2026-06-01T11:00:00Z")),
            )],
            invalidated: HashSet::from([SNAPSHOT]),
        };
        assert!(!facts.valid(&policy, "PLAN_VALIDATED", now));
        assert!(!facts.expired(&policy, "PLAN_VALIDATED", now));
        assert!(facts.observed("PLAN_VALIDATED"));
    }

    #[test]
    fn a_snapshot_frozen_against_another_cycle_is_observed_but_not_valid() {
        let now = instant("2026-06-01T12:00:00Z");
        let policy = policy_row();
        let mut snapshot = evidence(DeploymentEvidenceKind::PlanValidated, None);
        snapshot.plan_digest = Some("0".repeat(64));
        let facts = facts(vec![snapshot]);
        assert!(!facts.valid(&policy, "PLAN_VALIDATED", now));
        assert!(facts.observed("PLAN_VALIDATED"));
    }

    #[test]
    fn a_cycle_that_requires_no_evidence_has_no_issue() {
        let mut policy = policy_row();
        policy.required_evidence = Vec::new();
        assert_eq!(evidence_issue_of(&policy, &facts(Vec::new())), None);
    }

    #[test]
    fn evidence_never_observed_is_missing() {
        assert_eq!(
            evidence_issue_of(&policy_row(), &facts(Vec::new())),
            Some("APPROVAL_EVIDENCE_MISSING".to_string())
        );
    }

    /// Observed, matching and past its expiry: `EXPIRED`, not `MISSING` and not `MISMATCH`.
    #[test]
    fn evidence_observed_and_past_its_expiry_is_expired() {
        let facts = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(distant_past()),
        )]);
        assert_eq!(
            evidence_issue_of(&policy_row(), &facts),
            Some("APPROVAL_EVIDENCE_EXPIRED".to_string())
        );
    }

    /// Observed and unexpired, but frozen against another plan: `MISMATCH`. This is the refusal a
    /// re-planned deployment must get instead of silently reusing old evidence.
    #[test]
    fn evidence_observed_against_another_cycle_is_a_mismatch() {
        let mut snapshot = evidence(DeploymentEvidenceKind::PlanValidated, None);
        snapshot.binding_digest = Some("0".repeat(64));
        assert_eq!(
            evidence_issue_of(&policy_row(), &facts(vec![snapshot])),
            Some("APPROVAL_EVIDENCE_MISMATCH".to_string())
        );
    }

    #[test]
    fn a_valid_snapshot_for_every_required_kind_has_no_issue() {
        let facts = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(distant_future()),
        )]);
        assert_eq!(evidence_issue_of(&policy_row(), &facts), None);
    }

    /// The loop returns on the first required kind that fails, so a satisfied kind listed first
    /// does not mask a missing one listed second.
    #[test]
    fn the_first_unsatisfied_required_kind_decides_the_issue() {
        let mut policy = policy_row();
        policy.required_evidence = vec![
            "PLAN_VALIDATED".to_string(),
            "EVALUATION_PASSED".to_string(),
        ];
        let facts = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(distant_future()),
        )]);
        assert_eq!(
            evidence_issue_of(&policy, &facts),
            Some("APPROVAL_EVIDENCE_MISSING".to_string())
        );
    }

    /// A deployment whose project revision is known compares revisions, and falls back to its
    /// requested-at only for the events that recorded no revision.
    #[test]
    fn the_archive_boundary_compares_revisions_when_the_deployment_has_one() {
        let (deployment, _, _) = frozen_cycle();
        assert_eq!(
            rendered(archive_boundary_condition(&deployment)),
            "SELECT 1 WHERE \
             (\"deployment_approval_project_archive_events\".\"archived_project_revision\" IS NOT NULL \
             AND \"deployment_approval_project_archive_events\".\"archived_project_revision\" >= 7) \
             OR (\"deployment_approval_project_archive_events\".\"archived_project_revision\" IS NULL \
             AND \"deployment_approval_project_archive_events\".\"archived_at\" \
             >= '2026-01-01 00:00:00.000000 +00:00')"
        );
    }

    #[test]
    fn the_archive_boundary_compares_instants_when_the_deployment_has_no_revision() {
        let (mut deployment, _, _) = frozen_cycle();
        deployment.project_lifecycle_revision = None;
        assert_eq!(
            rendered(archive_boundary_condition(&deployment)),
            "SELECT 1 WHERE \"deployment_approval_project_archive_events\".\"archived_at\" \
             >= '2026-01-01 00:00:00.000000 +00:00'"
        );
    }

    fn archive_event(
        archived_project_revision: Option<i64>,
    ) -> deployment_approval_project_archive_events::Model {
        deployment_approval_project_archive_events::Model {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            actor_principal_id: None,
            archived_at: instant("2026-01-01T00:00:00Z"),
            processed_at: None,
            archived_project_revision,
        }
    }

    /// The mirror of `archive_boundary_condition`, with every comparison reversed: the event now
    /// selects the deployments it covers rather than the deployment selecting its events.
    #[test]
    fn archived_candidates_reverse_the_boundary_comparisons() {
        assert_eq!(
            rendered(archived_candidates(&archive_event(Some(7)))),
            "SELECT 1 WHERE (\"deployments\".\"project_lifecycle_revision\" IS NOT NULL \
             AND \"deployments\".\"project_lifecycle_revision\" <= 7) \
             OR (\"deployments\".\"project_lifecycle_revision\" IS NULL \
             AND \"deployments\".\"requested_at\" <= '2026-01-01 00:00:00.000000 +00:00')"
        );
        assert_eq!(
            rendered(archived_candidates(&archive_event(None))),
            "SELECT 1 WHERE \"deployments\".\"requested_at\" \
             <= '2026-01-01 00:00:00.000000 +00:00'"
        );
    }

    /// Archive reconciliation terminalizes two lifecycle/status pairings and no others; a
    /// deployment already executing or finished is outside the boundary whatever its requirement
    /// says.
    #[test]
    fn archived_pending_matches_only_the_two_open_cycles() {
        assert_eq!(
            rendered(archived_pending_condition()),
            "SELECT 1 WHERE (\"deployments\".\"lifecycle_status\" IN \
             ('AWAITING_APPROVAL', 'REQUESTED') \
             AND \"deployment_approval_requirements\".\"status\" = 'PENDING') \
             OR (\"deployments\".\"lifecycle_status\" IN ('APPROVED', 'REQUESTED') \
             AND \"deployment_approval_requirements\".\"status\" = 'SATISFIED')"
        );
    }
}
