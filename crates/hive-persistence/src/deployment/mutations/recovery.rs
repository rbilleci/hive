//! `recovery`: the engine behind `retry` and `rollback`. Both create a new immutable deployment
//! cycle while preserving the failed source's facts and attempts, so the source keeps its own
//! history and the child is a first-class deployment with its own approval requirement.
//!
//! The two differ only in the capability they demand, the lifecycle statuses they accept, the
//! version they target, and rollback's reason and production-confirmation requirements.

use super::rows;
use super::{
    action_fingerprint, action_receipt, can_view, deployment_locked, deployment_project,
    over_pending_quota, quota_anchor, record_action_receipt, same_compilation_inputs, valid_key,
};
use crate::capability::tx;
use crate::deployment::queries::{
    active_project_check, active_target, canonical_target_version, environment, policy,
    rollback_target_version, version_source,
};
use crate::retry;
use hive_application::deployment::compiler::digest;
use hive_application::deployment::DeploymentLifecycleStatus;
use hive_application::deployment::{
    ActiveTarget, CompiledRequest, Deployment, DeploymentMutationResult, DeploymentProblem,
    EnvironmentDefinition, PolicySource, VersionSource,
};
use hive_application::text::present;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, TransactionTrait};
use uuid::Uuid;

fn safe_recovery_reason(value: Option<&str>) -> &'static str {
    if present(value).is_none() {
        "No caller-entered recovery reason was recorded."
    } else {
        "An authorized project principal supplied the required recovery reason."
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
            if retry::is_serialization_failure_db(&error) {
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
    rows::insert_deployment(
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
    rows::insert_plan(txn, plan_id, child_id, principal_id, request).await?;
    rows::insert_plan_review(txn, plan_id, &request.review).await?;
    rows::insert_policy_snapshot(txn, child_id, request).await?;
    rows::insert_approval_requirement(txn, child_id, request).await?;
    rows::insert_evidence(txn, child_id, request).await?;
    rows::insert_runtime_health(txn, child_id).await?;
    rows::audit(
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
    rows::audit(
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
