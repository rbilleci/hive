//! `cancel`: terminalizing a deployment in place. The expected revision is in the `WHERE` clause,
//! so a racing writer that already advanced it leaves the update with no rows and the command
//! refuses with a revision conflict.

use super::rows;
use super::{can_view, raced_revision};
use crate::capability::tx;
use crate::entity::enums::{
    DeploymentLifecycleStatus as EntityLifecycleStatus, DeploymentRuntimeHealthStatus,
};
use crate::entity::{deployment_runtime_health, deployments};
use crate::guard;
use hive_application::deployment::{DeploymentMutationResult, DeploymentProblem};
use hive_application::text::present;
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{
    ActiveEnum, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, TransactionTrait,
};
use uuid::Uuid;

fn safe_reason(value: &str) -> &'static str {
    if present(Some(value)).is_none() {
        "Cancellation was requested from the local deployment detail."
    } else {
        "Cancellation was requested by an authorized project principal."
    }
}

pub async fn cancel(
    db: &DatabaseConnection,
    principal_id: Uuid,
    deployment_id: Uuid,
    expected_revision: i64,
    reason: &str,
) -> Result<DeploymentMutationResult, DbErr> {
    let txn = db.begin().await?;
    let Some(current) = rows::deployments(&txn, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
    else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(&txn, principal_id, current.project_id, true).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    if !tx::has_deployment_capability(
        &txn,
        principal_id,
        tx::DEPLOYMENT_CANCEL,
        current.project_id,
        true,
    )
    .await?
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::forbidden(),
        ));
    }
    if current.revision != expected_revision {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::conflict(deployment_id, expected_revision, current.revision),
        ));
    }
    if !current.lifecycle_status.is_cancellable() {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::lifecycle(),
        ));
    }
    // The expected revision is in the `WHERE` clause, so a racing writer that already advanced it
    // leaves this update with no rows and the command refuses with a revision conflict.
    let claimed = guard::bump(
        &txn,
        deployments::Entity::update_many()
            .col_expr(
                deployments::Column::LifecycleStatus,
                Expr::val(EntityLifecycleStatus::Canceled.to_value()),
            )
            .col_expr(deployments::Column::UpdatedAt, Expr::current_timestamp())
            .filter(deployments::Column::Id.eq(deployment_id)),
        deployments::Column::Revision,
        expected_revision,
    )
    .await?;
    if !claimed {
        let raced = rows::deployments(&txn, &[deployment_id], true)
            .await?
            .into_iter()
            .next();
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::conflict(
                deployment_id,
                expected_revision,
                raced
                    .map(|value| value.revision)
                    .unwrap_or(expected_revision),
            ),
        ));
    }
    crate::deployment::approval::invalidate_pending_requirement(
        &txn,
        deployment_id,
        "TERMINAL_LIFECYCLE",
        Some(principal_id),
    )
    .await?;
    rows::terminalize_running_attempts(
        &txn,
        deployment_id,
        "CANCELED",
        "LOCAL_CANCELED",
        "The requester canceled this local deployment.",
        "CANCELED",
        "CANCELED",
    )
    .await?;
    deployment_runtime_health::Entity::update_many()
        .col_expr(
            deployment_runtime_health::Column::Status,
            Expr::val(DeploymentRuntimeHealthStatus::Canceled.to_value()),
        )
        .col_expr(
            deployment_runtime_health::Column::Summary,
            Expr::val("Execution was canceled before runtime health became available."),
        )
        .col_expr(
            deployment_runtime_health::Column::ObservedAt,
            Expr::current_timestamp(),
        )
        .col_expr(
            deployment_runtime_health::Column::Generation,
            Expr::col(deployment_runtime_health::Column::Generation).add(1),
        )
        .filter(deployment_runtime_health::Column::DeploymentId.eq(deployment_id))
        .exec(&txn)
        .await?;
    rows::audit(
        &txn,
        deployment_id,
        Some(principal_id),
        "CANCELED",
        serde_json::json!({"expectedRevision": expected_revision, "reason": safe_reason(reason)}),
    )
    .await?;
    let result = rows::deployments(&txn, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| DbErr::RecordNotFound(format!("no deployment with id {deployment_id}")))?;
    raced_revision(db, txn, deployment_id, expected_revision, result).await
}
