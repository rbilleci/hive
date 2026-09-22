//! The five deployment lifecycle mutations, one module each: `deploy` admits a new cycle,
//! `cancel` terminalizes one in place, `recovery` backs both `retry` and `rollback` by creating a
//! new cycle from a failed source, and `promote` records a promotion fact. Each runs in one
//! transaction.
//!
//! Held here is what they share: the capability and key checks every command re-takes under the
//! row lock, the per-project quota anchor two admissions conflict on, the recovery action receipt
//! that makes a retried command idempotent, and `raced_revision`, which turns a lost commit into
//! the revision conflict the row lock would have produced.

mod cancel;
mod deploy;
mod promote;
mod recovery;

pub use cancel::cancel;
pub use deploy::deploy;
pub use promote::promote;
pub use recovery::{recovery, RecoveryAction};

use super::rows;
use crate::capability::tx;
use crate::entity::enums::{
    DeploymentLifecycleStatus as EntityLifecycleStatus, DeploymentRecoveryAction,
};
use crate::entity::{
    deployment_project_quota_claims, deployment_recovery_action_receipts, deployments,
};
use crate::retry;
use hive_application::deployment::compiler::digest;
use hive_application::deployment::{
    ActiveTarget, CompiledRequest, Deployment, DeploymentMutationResult, DeploymentProblem,
    EnvironmentDefinition, PolicySource, VersionSource,
};
use hive_application::text::present;
use sea_orm::sea_query::{Expr, OnConflict, Query};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr,
    EntityTrait, NotSet, PaginatorTrait, QueryFilter, QuerySelect, Set,
};
use uuid::Uuid;

fn valid_key(value: &str) -> bool {
    let trimmed = value.trim();
    (8..=160).contains(&trimmed.len())
}

async fn can_view(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    tx::has_deployment_capability(db, principal_id, tx::DEPLOYMENT_VIEW, project_id, lock).await
}

fn same_compilation_inputs(
    request: &CompiledRequest,
    version: &VersionSource,
    environment: Option<&EnvironmentDefinition>,
    policy: Option<&PolicySource>,
    current: Option<&ActiveTarget>,
) -> bool {
    &request.version == version
        && Some(&request.environment) == environment
        && Some(&request.policy) == policy
        && request.current_target.as_ref() == current
}

/// The per-project claim row two overlapping admissions both write, so Aurora DSQL detects their
/// conflict at commit. The column has no default and the instant is the database's own clock, so
/// it is written as `CURRENT_TIMESTAMP` rather than as a bound service-clock value.
async fn quota_anchor(db: &impl ConnectionTrait, project_id: Uuid) -> Result<(), DbErr> {
    let mut insert = Query::insert();
    insert
        .into_table(deployment_project_quota_claims::Entity)
        .columns([
            deployment_project_quota_claims::Column::ProjectId,
            deployment_project_quota_claims::Column::ClaimedAt,
        ])
        .values([Expr::val(project_id), Expr::current_timestamp()])
        .map_err(|error| DbErr::Custom(error.to_string()))?
        .on_conflict(
            OnConflict::column(deployment_project_quota_claims::Column::ProjectId)
                .update_column(deployment_project_quota_claims::Column::ClaimedAt)
                .to_owned(),
        );
    db.execute(&insert).await?;
    Ok(())
}

const MAX_PENDING_REQUESTS_PER_PRINCIPAL: u64 = 20;
const MAX_PENDING_REQUESTS_PER_PROJECT: u64 = 100;

/// The four lifecycle statuses a still-pending deployment request can hold.
const PENDING_LIFECYCLE: [EntityLifecycleStatus; 4] = [
    EntityLifecycleStatus::Requested,
    EntityLifecycleStatus::AwaitingApproval,
    EntityLifecycleStatus::Approved,
    EntityLifecycleStatus::InProgress,
];

async fn over_pending_quota(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    principal_id: Uuid,
) -> Result<bool, DbErr> {
    let per_principal = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project_id))
        .filter(deployments::Column::RequestedBy.eq(principal_id))
        .filter(deployments::Column::LifecycleStatus.is_in(PENDING_LIFECYCLE))
        .count(db)
        .await?;
    if per_principal >= MAX_PENDING_REQUESTS_PER_PRINCIPAL {
        return Ok(true);
    }
    let per_project = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project_id))
        .filter(deployments::Column::LifecycleStatus.is_in(PENDING_LIFECYCLE))
        .count(db)
        .await?;
    Ok(per_project >= MAX_PENDING_REQUESTS_PER_PROJECT)
}

/// Commits a command that holds the deployment row, answering `result` on success and the
/// revision conflict the row lock would have produced when the commit lost the race instead.
async fn raced_revision(
    db: &DatabaseConnection,
    txn: DatabaseTransaction,
    deployment_id: Uuid,
    expected_revision: i64,
    result: Deployment,
) -> Result<DeploymentMutationResult, DbErr> {
    let raced = retry::committed(db, txn, async |retry| {
        Ok(rows::deployments(retry, &[deployment_id], true)
            .await?
            .into_iter()
            .next())
    })
    .await?;
    match raced {
        None => Ok(DeploymentMutationResult::success(result)),
        Some(raced) => Ok(DeploymentMutationResult::refused(
            DeploymentProblem::conflict(
                deployment_id,
                expected_revision,
                raced
                    .map(|value| value.revision)
                    .unwrap_or(expected_revision),
            ),
        )),
    }
}

/// The deployment, with its row locked `FOR UPDATE` first.
async fn deployment_locked(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Option<Deployment>, DbErr> {
    let locked = deployments::Entity::find_by_id(id)
        .lock_exclusive()
        .select_only()
        .column(deployments::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?;
    if locked.is_none() {
        return Ok(None);
    }
    Ok(rows::deployments(db, &[id], true).await?.into_iter().next())
}

async fn deployment_project(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<Uuid>, DbErr> {
    deployments::Entity::find_by_id(deployment_id)
        .select_only()
        .column(deployments::Column::ProjectId)
        .into_tuple::<Uuid>()
        .one(db)
        .await
}

struct ActionReceipt {
    fingerprint: String,
    result_deployment_id: Uuid,
}

async fn action_receipt(
    db: &impl ConnectionTrait,
    source: Uuid,
    actor: Uuid,
    action: &str,
    key: Option<&str>,
) -> Result<Option<ActionReceipt>, DbErr> {
    let Some(key) = key else {
        return Ok(None);
    };
    Ok(deployment_recovery_action_receipts::Entity::find()
        .filter(deployment_recovery_action_receipts::Column::SourceDeploymentId.eq(source))
        .filter(deployment_recovery_action_receipts::Column::ActorPrincipalId.eq(actor))
        .filter(deployment_recovery_action_receipts::Column::Action.eq(
            DeploymentRecoveryAction::try_from_value(&action.to_string())?,
        ))
        .filter(deployment_recovery_action_receipts::Column::IdempotencyKey.eq(key.trim()))
        .one(db)
        .await?
        .map(|row| ActionReceipt {
            fingerprint: row.request_fingerprint,
            result_deployment_id: row.result_deployment_id,
        }))
}

#[allow(clippy::too_many_arguments)]
async fn record_action_receipt(
    db: &impl ConnectionTrait,
    source: Uuid,
    actor: Uuid,
    action: &str,
    key: &str,
    fingerprint: &str,
    result_deployment: Uuid,
) -> Result<Uuid, DbErr> {
    let id = Uuid::new_v4();
    deployment_recovery_action_receipts::Entity::insert(
        deployment_recovery_action_receipts::ActiveModel {
            id: Set(id),
            source_deployment_id: Set(source),
            actor_principal_id: Set(actor),
            action: Set(DeploymentRecoveryAction::try_from_value(
                &action.to_string(),
            )?),
            idempotency_key: Set(key.trim().to_string()),
            request_fingerprint: Set(fingerprint.to_string()),
            result_deployment_id: Set(result_deployment),
            occurred_at: NotSet,
        },
    )
    .exec_without_returning(db)
    .await?;
    Ok(id)
}

fn action_fingerprint(
    action: &str,
    deployment_id: Uuid,
    revision: i64,
    target_version: Option<&str>,
    reason: Option<&str>,
    confirmation: Option<&str>,
) -> String {
    let reason_digest = present(reason).map(digest).unwrap_or_default();
    let confirmation_digest = present(confirmation).map(digest).unwrap_or_default();
    digest(&format!(
        "{action}|{deployment_id}|{revision}|{}|{reason_digest}|{confirmation_digest}",
        target_version.unwrap_or("")
    ))
}
