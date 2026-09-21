//! The approval requirement row: creating it with `ensure_requirement`, moving it through its
//! statuses with `transition_requirement`, and the deployment-side writes that accompany a
//! terminal decision — the runtime-health cancel, the guarded lifecycle move and the handoff
//! release.
//!
//! Every statement here is a SeaORM entity read, an `update_many` with the guard in its `WHERE`
//! clause, or an `ActiveModel` insert.

use super::handoff::automatic_approval_handoff;
use super::reconciliation::{archive_boundary_condition, deployment_archive_boundary};
use super::{participants_json, wall_clock};
use crate::deployment::rows::raw_requirement_by_deployment;
use crate::deployment::rows::{audit, system_audit, touch_projection};
use crate::entity::enums::{
    ApprovalInvalidationCode, ApprovalRequirementStatus as EntityRequirementStatus,
    DeploymentLifecycleStatus as EntityLifecycleStatus, DeploymentRuntimeHealthStatus,
};
use crate::entity::{
    deployment_approval_handoff_releases, deployment_approval_project_archive_events,
    deployment_approval_requirements, deployment_policy_snapshots, deployment_runtime_health,
    deployments,
};
use hive_application::deployment::ApprovalRequirementStatus;
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::sea_query::{Expr, ExprTrait, OnConflict};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, NotSet, QueryFilter, QueryOrder,
    QuerySelect, Set, TryInsertResult,
};
use serde_json::json;
use uuid::Uuid;

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
pub(super) async fn cancel_runtime_health(
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
pub(super) async fn move_lifecycle(
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

pub(super) async fn delete_handoff_release(
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

pub(super) async fn requirement_of(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<deployment_approval_requirements::Model>, DbErr> {
    deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .one(db)
        .await
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
