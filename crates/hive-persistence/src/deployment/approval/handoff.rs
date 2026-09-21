//! Whether an approved deployment may execute, and the queueing that follows: the worker
//! compatibility and readiness reads, the handoff release row, the deferrals that return a claimed
//! outbox event to the queue, and `automatic_approval_handoff`, which every deploy and recovery
//! path reaches.
//!
//! `approval_execution_eligible`'s `count(DISTINCT ...)`/`jsonb_array_length` over
//! `satisfied_participants` is the requirement row's own `jsonb` column counted in Rust, and its
//! participants-are-all-approvers anti-join is one read of the requirement's `APPROVE` decisions.

use super::evidence::{evidence_ready, string_list, waiting_for_evaluation};
use super::reconciliation::{
    block_approval_execution, deployment_archive_boundary, reconcile_pending,
};
use super::requirements::{delete_handoff_release, ensure_requirement, requirement_of};
use super::{participants_json, wall_clock};
use crate::deployment::rows::{system_audit, touch_projection};
use crate::entity::enums::{
    ApprovalRequirementStatus as EntityRequirementStatus,
    DeploymentLifecycleStatus as EntityLifecycleStatus, DeploymentOutboxEventType,
    DeploymentOutboxStatus, LifecycleStatus, WorkerHeartbeatState,
};
use crate::entity::{
    deployment_approval_decisions, deployment_approval_handoff_releases,
    deployment_approval_project_archive_events, deployment_approval_requirements,
    deployment_outbox_events, deployment_worker_heartbeats, deployments, projects,
};
use hive_application::deployment::{ApprovalRequirementStatus, DeploymentLifecycleStatus};
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::sea_query::{Expr, ExprTrait, Func, OnConflict, Query};
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    JoinType, NotSet, QueryFilter, QueryOrder, QuerySelect, RelationTrait, Set, TransactionTrait,
};
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

/// `CAST('<n> <unit>' AS interval)`, the spelling `evaluation::worker` established, because
/// sea-query has no interval `Value`.
fn interval(text: String) -> Expr {
    Expr::value(text).cast_as("interval")
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
                crate::deployment::rows::enqueue(
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
            tracing::warn!(code = %crate::deployment::worker::db_failure_code(&error), "approval handoff maintenance failed");
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
