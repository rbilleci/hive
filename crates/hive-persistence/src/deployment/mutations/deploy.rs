//! `deploy`: admitting a new deployment cycle. The compilation inputs the caller planned against
//! are re-read under the row locks and compared, so a version, environment, policy or active
//! target that moved between planning and admission refuses rather than deploys.
//!
//! The idempotency key is answered twice: once inside the transaction against the stored
//! fingerprint, and once from a 23505 at commit, which is either that same replay seen by a
//! concurrent caller or a genuine key collision.

use super::rows;
use super::{can_view, over_pending_quota, quota_anchor, same_compilation_inputs, valid_key};
use crate::capability::tx;
use crate::deployment::queries::{
    active_project_check, active_target, environment, policy, version_source,
};
use crate::entity::{agent_versions, agents, deployments};
use crate::retry;
use hive_application::deployment::compiler::digest;
use hive_application::deployment::{CompiledRequest, DeploymentMutationResult, DeploymentProblem};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, JoinType, QueryFilter,
    QuerySelect, RelationTrait, TransactionTrait,
};
use uuid::Uuid;

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
    let strategy = request.strategy;

    let txn = db.begin().await?;
    let result = deploy_tx(&txn, principal_id, request, key).await;
    match result {
        Ok(value) => {
            txn.commit().await?;
            Ok(value)
        }
        Err(error) if retry::is_unique_violation_db(&error) && valid_key(key) => {
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
            if retry::is_serialization_failure_db(&error) {
                return Ok(DeploymentMutationResult::refused(
                    DeploymentProblem::rate_limited(),
                ));
            }
            Err(error)
        }
        Err(error) => {
            if retry::is_serialization_failure_db(&error) {
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
    rows::insert_deployment(txn, deployment_id, principal_id, request, key, &fingerprint).await?;
    rows::insert_plan(txn, plan_id, deployment_id, principal_id, request).await?;
    rows::insert_plan_review(txn, plan_id, &request.review).await?;
    rows::insert_policy_snapshot(txn, deployment_id, request).await?;
    rows::insert_approval_requirement(txn, deployment_id, request).await?;
    rows::insert_evidence(txn, deployment_id, request).await?;
    rows::insert_runtime_health(txn, deployment_id).await?;
    rows::audit(
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
