//! `promote`: recording that an active, healthy deployment's target was promoted. The promotion
//! fact carries the runtime-health generation it was taken against, so a later observation cannot
//! be mistaken for the one that justified it.

use super::rows;
use super::{
    action_fingerprint, action_receipt, can_view, deployment_locked, deployment_project,
    raced_revision, record_action_receipt, valid_key,
};
use crate::capability::tx;
use crate::entity::deployment_promotion_facts;
use hive_application::deployment::DeploymentLifecycleStatus;
use hive_application::deployment::{Deployment, DeploymentMutationResult, DeploymentProblem};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, NotSet, Set, TransactionTrait,
};
use uuid::Uuid;

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
    rows::audit(&txn, deployment_id, Some(principal_id), "PROMOTION_RECORDED", serde_json::json!({"receiptId": receipt.to_string(), "targetDigest": current.plan.target_digest})).await?;
    let result = rows::deployments(&txn, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| DbErr::RecordNotFound(format!("no deployment with id {deployment_id}")))?;
    raced_revision(db, txn, deployment_id, expected_revision, result).await
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
