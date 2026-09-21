//! The five deployment lifecycle mutations: `deploy`, `cancel`, `retry` and `rollback` (which
//! share the `recovery` engine, one thin `RecoveryAction` wrapper each), and `promote` (which
//! does not).

use super::queries::{
    active_project_check, active_target, canonical_target_version, environment, policy,
    rollback_target_version, version_source,
};
use super::rows;
use super::writes;
use crate::capability::tx;
use crate::entity::enums::{
    DeploymentLifecycleStatus as EntityLifecycleStatus, DeploymentRecoveryAction,
    DeploymentRuntimeHealthStatus,
};
use crate::entity::{
    agent_versions, agents, deployment_project_quota_claims, deployment_promotion_facts,
    deployment_recovery_action_receipts, deployment_runtime_health, deployments,
};
use crate::retry::{is_serialization_failure_db, is_unique_violation_db};
use hive_application::deployment::compiler::digest;
use hive_application::deployment::{
    ActiveTarget, CompiledRequest, Deployment, DeploymentMutationResult, DeploymentProblem,
    EnvironmentDefinition, PolicySource, VersionSource,
};
use hive_application::text::present;
use hive_domain::deployment::DeploymentLifecycleStatus;
use sea_orm::sea_query::{Expr, ExprTrait, OnConflict, Query};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, JoinType,
    NotSet, PaginatorTrait, QueryFilter, QuerySelect, RelationTrait, Set, TransactionTrait,
};
use uuid::Uuid;

fn valid_key(value: &str) -> bool {
    let trimmed = value.trim();
    (8..=160).contains(&trimmed.len())
}

fn safe_reason(value: &str) -> &'static str {
    if present(Some(value)).is_none() {
        "Cancellation was requested from the local deployment detail."
    } else {
        "Cancellation was requested by an authorized project principal."
    }
}

fn safe_recovery_reason(value: Option<&str>) -> &'static str {
    if present(value).is_none() {
        "No caller-entered recovery reason was recorded."
    } else {
        "An authorized project principal supplied the required recovery reason."
    }
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

async fn project_for_version(
    db: &impl ConnectionTrait,
    version_id: Uuid,
) -> Result<Option<Uuid>, DbErr> {
    agent_versions::Entity::find_by_id(version_id)
        .join(JoinType::InnerJoin, agent_versions::Relation::Agents.def())
        .select_only()
        .column(agents::Column::ProjectId)
        .into_tuple::<Uuid>()
        .one(db)
        .await
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

struct IdempotencyHit {
    id: Uuid,
    fingerprint: String,
}

async fn idempotency(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    key: &str,
) -> Result<Option<IdempotencyHit>, DbErr> {
    Ok(deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project_id))
        .filter(deployments::Column::IdempotencyKey.eq(key.trim()))
        .one(db)
        .await?
        .map(|row| IdempotencyHit {
            id: row.id,
            fingerprint: row.request_fingerprint.unwrap_or_default(),
        }))
}

pub async fn deploy(
    db: &DatabaseConnection,
    principal_id: Uuid,
    request: &CompiledRequest,
    key: &str,
) -> Result<DeploymentMutationResult, DbErr> {
    let version_id = request.version.id;
    let environment_id = request.environment.id;
    let strategy = request.strategy.clone();

    let txn = db.begin().await?;
    let result = deploy_tx(&txn, principal_id, request, key).await;
    match result {
        Ok(value) => {
            txn.commit().await?;
            Ok(value)
        }
        Err(error) if is_unique_violation_db(&error) && valid_key(key) => {
            // A 23505 here is either the receipt's own unique constraint (idempotency replay) or the
            // deployments table's (project_id, idempotency_key) constraint (a genuinely new key
            // collision, not an idempotency replay). Try the replay path first; if it finds nothing,
            // this was a genuine collision and the original error is returned.
            let retry = db.begin().await?;
            if let Some(project_id) = project_for_version(&retry, version_id).await? {
                if let Some(existing) = idempotency(&retry, project_id, key).await? {
                    let fingerprint = digest(&format!("{version_id}|{environment_id}|{strategy}"));
                    let result = if existing.fingerprint == fingerprint {
                        match rows::deployments(&retry, &[existing.id], true)
                            .await?
                            .into_iter()
                            .next()
                        {
                            Some(deployment) => Ok(DeploymentMutationResult::success(deployment)),
                            None => Err(DbErr::RecordNotFound(format!(
                                "no deployment with id {}",
                                existing.id
                            ))),
                        }
                    } else {
                        Ok(DeploymentMutationResult::refused(
                            DeploymentProblem::idempotency(),
                        ))
                    };
                    retry.commit().await?;
                    return result;
                }
            }
            retry.rollback().await?;
            // Two overlapping deploy() calls for the same project both wrote quotaAnchor()'s claim
            // row; DSQL let both proceed and only detected the conflict here, at the loser's commit.
            if is_serialization_failure_db(&error) {
                return Ok(DeploymentMutationResult::refused(
                    DeploymentProblem::rate_limited(),
                ));
            }
            Err(error)
        }
        Err(error) => {
            if is_serialization_failure_db(&error) {
                return Ok(DeploymentMutationResult::refused(
                    DeploymentProblem::rate_limited(),
                ));
            }
            Err(error)
        }
    }
}

async fn deploy_tx(
    txn: &impl ConnectionTrait,
    principal_id: Uuid,
    request: &CompiledRequest,
    key: &str,
) -> Result<DeploymentMutationResult, DbErr> {
    let version_id = request.version.id;
    let environment_id = request.environment.id;

    let Some(preauthorized) = version_source(txn, version_id, false).await? else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(txn, principal_id, preauthorized.project_id, false).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    if !tx::has_deployment_capability(
        txn,
        principal_id,
        tx::DEPLOYMENT_REQUEST,
        preauthorized.project_id,
        false,
    )
    .await?
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::forbidden(),
        ));
    }
    quota_anchor(txn, preauthorized.project_id).await?;
    let Some(version) = version_source(txn, version_id, true).await? else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(txn, principal_id, version.project_id, true).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    if !tx::has_deployment_capability(
        txn,
        principal_id,
        tx::DEPLOYMENT_REQUEST,
        version.project_id,
        true,
    )
    .await?
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::forbidden(),
        ));
    }
    if !active_project_check(txn, version.project_id, true).await? || !valid_key(key) {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::invalid(),
        ));
    }
    let environment_value = environment(txn, environment_id, &version.catalog_release_id).await?;
    let policy_value = policy(txn, version.project_id, true).await?;
    let current = active_target(
        txn,
        version.project_id,
        version.agent_id,
        environment_id,
        true,
    )
    .await?;
    if !same_compilation_inputs(
        request,
        &version,
        environment_value.as_ref(),
        policy_value.as_ref(),
        current.as_ref(),
    ) {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::conflict(version_id, 0, 1),
        ));
    }
    let fingerprint = digest(&format!(
        "{version_id}|{environment_id}|{}",
        request.strategy
    ));
    if let Some(existing) = idempotency(txn, version.project_id, key).await? {
        return if existing.fingerprint == fingerprint {
            match rows::deployments(txn, &[existing.id], true)
                .await?
                .into_iter()
                .next()
            {
                Some(deployment) => Ok(DeploymentMutationResult::success(deployment)),
                None => Ok(DeploymentMutationResult::refused(
                    DeploymentProblem::not_found(),
                )),
            }
        } else {
            Ok(DeploymentMutationResult::refused(
                DeploymentProblem::idempotency(),
            ))
        };
    }
    if over_pending_quota(txn, version.project_id, principal_id).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::rate_limited(),
        ));
    }
    let deployment_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    writes::insert_deployment(txn, deployment_id, principal_id, request, key, &fingerprint).await?;
    writes::insert_plan(txn, plan_id, deployment_id, principal_id, request).await?;
    writes::insert_plan_review(txn, plan_id, &request.review).await?;
    writes::insert_policy_snapshot(txn, deployment_id, request).await?;
    writes::insert_approval_requirement(txn, deployment_id, request).await?;
    writes::insert_evidence(txn, deployment_id, request).await?;
    writes::insert_runtime_health(txn, deployment_id).await?;
    writes::audit(
        txn,
        deployment_id,
        Some(principal_id),
        "REQUESTED",
        serde_json::json!({"bindingDigest": request.binding_digest, "environmentDefinitionVersionId": environment_id.to_string(), "planDigest": request.plan_digest}),
    )
    .await?;
    crate::deployment::approval::automatic_approval_handoff(txn, deployment_id).await?;
    let result = rows::deployments(txn, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| DbErr::RecordNotFound(format!("no deployment with id {deployment_id}")))?;
    Ok(DeploymentMutationResult::success(result))
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
    let claimed = deployments::Entity::update_many()
        .col_expr(
            deployments::Column::LifecycleStatus,
            Expr::val(EntityLifecycleStatus::Canceled.to_value()),
        )
        .col_expr(
            deployments::Column::Revision,
            Expr::col(deployments::Column::Revision).add(1),
        )
        .col_expr(deployments::Column::UpdatedAt, Expr::current_timestamp())
        .filter(deployments::Column::Id.eq(deployment_id))
        .filter(deployments::Column::Revision.eq(expected_revision))
        .exec(&txn)
        .await?;
    if claimed.rows_affected == 0 {
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
    writes::terminalize_running_attempts(
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
    writes::audit(
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
    match txn.commit().await {
        Ok(()) => Ok(DeploymentMutationResult::success(result)),
        Err(error) if is_serialization_failure_db(&error) => {
            let retry = db.begin().await?;
            let raced = rows::deployments(&retry, &[deployment_id], true)
                .await?
                .into_iter()
                .next();
            retry.commit().await?;
            Ok(DeploymentMutationResult::refused(
                DeploymentProblem::conflict(
                    deployment_id,
                    expected_revision,
                    raced
                        .map(|value| value.revision)
                        .unwrap_or(expected_revision),
                ),
            ))
        }
        Err(error) => Err(error),
    }
}

pub async fn promote(
    db: &DatabaseConnection,
    principal_id: Uuid,
    deployment_id: Uuid,
    expected_revision: i64,
    key: &str,
) -> Result<DeploymentMutationResult, DbErr> {
    let txn = db.begin().await?;
    let Some(preauthorized_project) = deployment_project(&txn, deployment_id).await? else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(&txn, principal_id, preauthorized_project, false).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    if !tx::has_deployment_capability(
        &txn,
        principal_id,
        tx::DEPLOYMENT_PROMOTE,
        preauthorized_project,
        false,
    )
    .await?
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::forbidden(),
        ));
    }
    let Some(current) = deployment_locked(&txn, deployment_id).await? else {
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
        tx::DEPLOYMENT_PROMOTE,
        current.project_id,
        true,
    )
    .await?
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::forbidden(),
        ));
    }
    let fingerprint = action_fingerprint(
        "PROMOTE",
        deployment_id,
        expected_revision,
        None,
        None,
        None,
    );
    if let Some(replay) =
        action_receipt(&txn, deployment_id, principal_id, "PROMOTE", Some(key)).await?
    {
        return if replay.fingerprint == fingerprint {
            match rows::deployments(&txn, &[replay.result_deployment_id], true)
                .await?
                .into_iter()
                .next()
            {
                Some(deployment) => Ok(DeploymentMutationResult::replayed(deployment)),
                None => Ok(DeploymentMutationResult::refused(
                    DeploymentProblem::not_found(),
                )),
            }
        } else {
            Ok(DeploymentMutationResult::refused(
                DeploymentProblem::idempotency(),
            ))
        };
    }
    if !valid_key(key) {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::invalid(),
        ));
    }
    if current.revision != expected_revision {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::conflict(deployment_id, expected_revision, current.revision),
        ));
    }
    if current.lifecycle_status != DeploymentLifecycleStatus::Active
        || current.runtime_health.status != "HEALTHY"
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::lifecycle(),
        ));
    }
    let receipt = record_action_receipt(
        &txn,
        deployment_id,
        principal_id,
        "PROMOTE",
        key,
        &fingerprint,
        deployment_id,
    )
    .await?;
    record_promotion(&txn, receipt, &current).await?;
    writes::audit(&txn, deployment_id, Some(principal_id), "PROMOTION_RECORDED", serde_json::json!({"receiptId": receipt.to_string(), "targetDigest": current.plan.target_digest})).await?;
    let result = rows::deployments(&txn, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| DbErr::RecordNotFound(format!("no deployment with id {deployment_id}")))?;
    match txn.commit().await {
        Ok(()) => Ok(DeploymentMutationResult::success(result)),
        Err(error) if is_serialization_failure_db(&error) => {
            let retry = db.begin().await?;
            let raced = rows::deployments(&retry, &[deployment_id], true)
                .await?
                .into_iter()
                .next();
            retry.commit().await?;
            Ok(DeploymentMutationResult::refused(
                DeploymentProblem::conflict(
                    deployment_id,
                    expected_revision,
                    raced
                        .map(|value| value.revision)
                        .unwrap_or(expected_revision),
                ),
            ))
        }
        Err(error) => Err(error),
    }
}

pub enum RecoveryAction {
    Retry,
    Rollback,
}

impl RecoveryAction {
    fn name(&self) -> &'static str {
        match self {
            RecoveryAction::Retry => "RETRY",
            RecoveryAction::Rollback => "ROLLBACK",
        }
    }
}

/// Creates a new immutable local deployment cycle while preserving the failed source's facts and
/// attempts. Backs both `retry` and `rollback`.
#[allow(clippy::too_many_arguments)]
pub async fn recovery(
    db: &DatabaseConnection,
    principal_id: Uuid,
    deployment_id: Uuid,
    expected_revision: i64,
    key: &str,
    action: RecoveryAction,
    requested_target_version: Option<&str>,
    reason: Option<&str>,
    production_confirmation: Option<&str>,
    request: Option<&CompiledRequest>,
) -> Result<DeploymentMutationResult, DbErr> {
    let txn = db.begin().await?;
    let result = recovery_tx(
        &txn,
        principal_id,
        deployment_id,
        expected_revision,
        key,
        &action,
        requested_target_version,
        reason,
        production_confirmation,
        request,
    )
    .await;
    match result {
        Ok(value) => {
            txn.commit().await?;
            Ok(value)
        }
        Err(error) => {
            // Same quotaAnchor() commit-time race deploy() catches.
            if is_serialization_failure_db(&error) {
                return Ok(DeploymentMutationResult::refused(
                    DeploymentProblem::rate_limited(),
                ));
            }
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn recovery_tx(
    txn: &impl ConnectionTrait,
    principal_id: Uuid,
    deployment_id: Uuid,
    expected_revision: i64,
    key: &str,
    action: &RecoveryAction,
    requested_target_version: Option<&str>,
    reason: Option<&str>,
    production_confirmation: Option<&str>,
    request: Option<&CompiledRequest>,
) -> Result<DeploymentMutationResult, DbErr> {
    let Some(preauthorized_project) = deployment_project(txn, deployment_id).await? else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(txn, principal_id, preauthorized_project, false).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    let capability = if matches!(action, RecoveryAction::Retry) {
        tx::DEPLOYMENT_RETRY
    } else {
        tx::DEPLOYMENT_ROLLBACK
    };
    if !tx::has_deployment_capability(txn, principal_id, capability, preauthorized_project, false)
        .await?
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::forbidden(),
        ));
    }
    quota_anchor(txn, preauthorized_project).await?;
    let Some(source) = deployment_locked(txn, deployment_id).await? else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(txn, principal_id, source.project_id, true).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    if !tx::has_deployment_capability(txn, principal_id, capability, source.project_id, true)
        .await?
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::forbidden(),
        ));
    }
    let canonical_target = canonical_target_version(requested_target_version);
    let fingerprint = action_fingerprint(
        action.name(),
        deployment_id,
        expected_revision,
        canonical_target.as_deref(),
        reason,
        production_confirmation,
    );
    if let Some(replay) =
        action_receipt(txn, deployment_id, principal_id, action.name(), Some(key)).await?
    {
        return if replay.fingerprint == fingerprint {
            match rows::deployments(txn, &[replay.result_deployment_id], true)
                .await?
                .into_iter()
                .next()
            {
                Some(deployment) => Ok(DeploymentMutationResult::replayed(deployment)),
                None => Ok(DeploymentMutationResult::refused(
                    DeploymentProblem::not_found(),
                )),
            }
        } else {
            Ok(DeploymentMutationResult::refused(
                DeploymentProblem::idempotency(),
            ))
        };
    }
    if !valid_key(key) {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::invalid(),
        ));
    }
    if matches!(action, RecoveryAction::Rollback)
        && !valid_optional_uuid(canonical_target.as_deref())
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::invalid(),
        ));
    }
    if source.revision != expected_revision {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::conflict(deployment_id, expected_revision, source.revision),
        ));
    }
    if matches!(action, RecoveryAction::Rollback) {
        if present(reason).is_none() {
            return Ok(DeploymentMutationResult::refused(
                DeploymentProblem::reason_required(),
            ));
        }
        if source.environment.logical_environment_class == "PRODUCTION" {
            let Some(production_confirmation) = production_confirmation else {
                return Ok(DeploymentMutationResult::refused(
                    DeploymentProblem::confirmation_required(),
                ));
            };
            if source.environment.stable_definition_id != production_confirmation.trim() {
                return Ok(DeploymentMutationResult::refused(
                    DeploymentProblem::confirmation_mismatch(),
                ));
            }
        }
    }
    if !active_project_check(txn, source.project_id, true).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::lifecycle(),
        ));
    }
    if matches!(action, RecoveryAction::Retry)
        && source.lifecycle_status != DeploymentLifecycleStatus::Failed
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::lifecycle(),
        ));
    }
    if matches!(action, RecoveryAction::Rollback)
        && !matches!(
            source.lifecycle_status,
            DeploymentLifecycleStatus::Failed | DeploymentLifecycleStatus::Active
        )
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::lifecycle(),
        ));
    }
    let version_id = if matches!(action, RecoveryAction::Retry) {
        Some(source.agent_version_id)
    } else {
        rollback_target_version(txn, &source, canonical_target.as_deref()).await?
    };
    let Some(version_id) = version_id else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::lifecycle(),
        ));
    };
    let Some(request) = request else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::invalid(),
        ));
    };
    if !same_recovery_compilation_inputs(txn, request, &source, version_id).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::conflict(deployment_id, expected_revision, source.revision),
        ));
    }
    if over_pending_quota(txn, source.project_id, principal_id).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::rate_limited(),
        ));
    }
    let child_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    let child_key = format!(
        "m15-{}",
        digest(&format!(
            "{}|{deployment_id}|{principal_id}|{key}",
            action.name()
        ))
    );
    writes::insert_deployment(
        txn,
        child_id,
        principal_id,
        request,
        &child_key,
        &digest(&format!(
            "{version_id}|{}|{}",
            request.environment.id, request.strategy
        )),
    )
    .await?;
    writes::insert_plan(txn, plan_id, child_id, principal_id, request).await?;
    writes::insert_plan_review(txn, plan_id, &request.review).await?;
    writes::insert_policy_snapshot(txn, child_id, request).await?;
    writes::insert_approval_requirement(txn, child_id, request).await?;
    writes::insert_evidence(txn, child_id, request).await?;
    writes::insert_runtime_health(txn, child_id).await?;
    writes::audit(
        txn,
        child_id,
        Some(principal_id),
        "REQUESTED",
        serde_json::json!({
            "bindingDigest": request.binding_digest, "environmentDefinitionVersionId": request.environment.id.to_string(),
            "planDigest": request.plan_digest, "recoveryAction": action.name(), "recoverySourceDeploymentId": deployment_id.to_string(),
        }),
    )
    .await?;
    let receipt = record_action_receipt(
        txn,
        deployment_id,
        principal_id,
        action.name(),
        key,
        &fingerprint,
        child_id,
    )
    .await?;
    writes::audit(
        txn,
        deployment_id,
        Some(principal_id),
        &format!("{}_RECORDED", action.name()),
        serde_json::json!({"receiptId": receipt.to_string(), "resultDeploymentId": child_id.to_string(), "reason": safe_recovery_reason(reason)}),
    )
    .await?;
    crate::deployment::approval::automatic_approval_handoff(txn, child_id).await?;
    let result = rows::deployments(txn, &[child_id], true)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| DbErr::RecordNotFound(format!("no deployment with id {child_id}")))?;
    Ok(DeploymentMutationResult::success(result))
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

fn valid_optional_uuid(value: Option<&str>) -> bool {
    match value {
        None => true,
        Some(value) => Uuid::parse_str(value).is_ok(),
    }
}

async fn recovery_compilation_inputs(
    db: &impl ConnectionTrait,
    source: &Deployment,
    version_id: Uuid,
    lock: bool,
) -> Result<
    Option<(
        VersionSource,
        EnvironmentDefinition,
        PolicySource,
        Option<ActiveTarget>,
    )>,
    DbErr,
> {
    let Some(version) = version_source(db, version_id, lock).await? else {
        return Ok(None);
    };
    if version.project_id != source.project_id || version.agent_id != source.agent_id {
        return Ok(None);
    }
    let environment_value =
        environment(db, source.environment.id, &version.catalog_release_id).await?;
    let policy_value = policy(db, source.project_id, lock).await?;
    let current_target = active_target(
        db,
        source.project_id,
        source.agent_id,
        source.environment.id,
        lock,
    )
    .await?;
    match (environment_value, policy_value) {
        (Some(environment_value), Some(policy_value)) => Ok(Some((
            version,
            environment_value,
            policy_value,
            current_target,
        ))),
        _ => Ok(None),
    }
}

async fn same_recovery_compilation_inputs(
    db: &impl ConnectionTrait,
    request: &CompiledRequest,
    source: &Deployment,
    version_id: Uuid,
) -> Result<bool, DbErr> {
    let Some((version, environment_value, policy_value, current_target)) =
        recovery_compilation_inputs(db, source, version_id, true).await?
    else {
        return Ok(false);
    };
    Ok(same_compilation_inputs(
        request,
        &version,
        Some(&environment_value),
        Some(&policy_value),
        current_target.as_ref(),
    ))
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

async fn record_promotion(
    db: &impl ConnectionTrait,
    receipt: Uuid,
    deployment: &Deployment,
) -> Result<(), DbErr> {
    deployment_promotion_facts::Entity::insert(deployment_promotion_facts::ActiveModel {
        id: Set(Uuid::new_v4()),
        deployment_id: Set(deployment.id),
        action_receipt_id: Set(receipt),
        agent_version_id: Set(deployment.agent_version_id),
        target_digest: Set(deployment.plan.target_digest.clone()),
        runtime_health_generation: Set(deployment.runtime_health.generation),
        occurred_at: NotSet,
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
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
