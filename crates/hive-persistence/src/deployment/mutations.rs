//! Ports the five deployment lifecycle mutations: `deploy`, `cancel`,
//! `retry`/`rollback` (which share Java's `recovery()` engine — a thin
//! wrapper each in the trait impl, per `RecoveryAction`), and `promote`
//! (which does not go through `recovery()`).

use super::rows;
use super::writes;
use crate::capability::tx;
use crate::sql::{is_serialization_failure_db, is_unique_violation_db};
use hive_application::deployment::compiler::digest;
use hive_application::deployment::{
    ActiveTarget, CompiledRequest, Deployment, DeploymentMutationResult, DeploymentProblem,
    EnvironmentDefinition, PolicySource, VersionSource,
};
use hive_domain::deployment::DeploymentLifecycleStatus;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, TransactionTrait};
use uuid::Uuid;

fn blank(value: Option<&str>) -> bool {
    value.map(str::trim).unwrap_or("").is_empty()
}

fn valid_key(value: &str) -> bool {
    let trimmed = value.trim();
    (8..=160).contains(&trimmed.len())
}

fn safe_reason(value: &str) -> &'static str {
    if blank(Some(value)) {
        "Cancellation was requested from the local deployment detail."
    } else {
        "Cancellation was requested by an authorized project principal."
    }
}

fn safe_recovery_reason(value: Option<&str>) -> &'static str {
    if blank(value) {
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

async fn active_project_check(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let sql = format!(
        "SELECT 1 FROM projects WHERE id = $1 AND lifecycle_status = 'ACTIVE'{}",
        if lock { " FOR SHARE" } else { "" }
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [project_id.into()]);
    Ok(db.query_one_raw(statement).await?.is_some())
}

async fn version_source(
    db: &impl ConnectionTrait,
    version_id: Uuid,
    lock: bool,
) -> Result<Option<VersionSource>, DbErr> {
    let suffix = if lock {
        " FOR KEY SHARE OF version, agent, project"
    } else {
        ""
    };
    let sql = format!(
        "SELECT version.id AS version_id, project.id AS project_id, agent.id AS agent_id, agent.display_name, version.version_number, version.content_digest, \
             version.catalog_release_id, version.catalog_release_digest, project.organization_id, version.canonical_document::text AS canonical_document \
         FROM agent_versions version JOIN agents agent ON agent.id = version.agent_id JOIN projects project ON project.id = agent.project_id \
         WHERE version.id = $1{suffix}"
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [version_id.into()]);
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(rows::version_source_row(&row)?)),
        None => Ok(None),
    }
}

async fn environment(
    db: &impl ConnectionTrait,
    environment_id: Uuid,
    release_id: &str,
) -> Result<Option<EnvironmentDefinition>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, stable_definition_id, version, display_name, logical_environment_class, catalog_release_id, catalog_release_digest, content_digest \
         FROM environment_definition_versions WHERE id = $1 AND catalog_release_id = $2",
        [environment_id.into(), release_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(rows::compiler_environment_row(&row)?)),
        None => Ok(None),
    }
}

async fn policy(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    lock: bool,
) -> Result<Option<PolicySource>, DbErr> {
    let suffix = if lock {
        " FOR SHARE OF policy, version"
    } else {
        ""
    };
    let sql = format!(
        "SELECT policy.id, version.revision, version.digest, version.matrix::text AS matrix \
         FROM project_approval_policies policy JOIN project_approval_policy_versions version \
           ON version.policy_id = policy.id AND version.revision = policy.current_revision WHERE policy.project_id = $1{suffix}"
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [project_id.into()]);
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(PolicySource {
            id: row.try_get_by("id")?,
            revision: row.try_get_by("revision")?,
            digest: row.try_get_by("digest")?,
            matrix: row.try_get_by("matrix")?,
        })),
        None => Ok(None),
    }
}

async fn active_target(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    agent_id: Uuid,
    environment_id: Uuid,
    lock: bool,
) -> Result<Option<ActiveTarget>, DbErr> {
    let suffix = if lock { " FOR SHARE OF deployment" } else { "" };
    let sql = format!(
        "SELECT plan.target_digest, version.canonical_document::text AS canonical_document, deployment.id, deployment.agent_version_id, version.version_number, deployment.requested_at \
         FROM deployments deployment JOIN deployment_plan_versions plan ON plan.deployment_id = deployment.id AND plan.version_number = 1 \
           JOIN agent_versions version ON version.id = deployment.agent_version_id \
         WHERE deployment.project_id = $1 AND deployment.agent_id = $2 AND deployment.environment_definition_version_id = $3 \
           AND deployment.lifecycle_status = 'ACTIVE' \
         ORDER BY deployment.updated_at DESC, deployment.id DESC LIMIT 1{suffix}"
    );
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        &sql,
        [project_id.into(), agent_id.into(), environment_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(ActiveTarget {
            target_digest: row.try_get_by("target_digest")?,
            canonical_document: row.try_get_by("canonical_document")?,
            deployment_id: row.try_get_by("id")?,
            agent_version_id: row.try_get_by("agent_version_id")?,
            agent_version_number: row.try_get_by("version_number")?,
            requested_at: row.try_get_by("requested_at")?,
        })),
        None => Ok(None),
    }
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT agent.project_id FROM agent_versions version JOIN agents agent ON agent.id = version.agent_id WHERE version.id = $1",
        [version_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(row.try_get_by("project_id")?)),
        None => Ok(None),
    }
}

async fn quota_anchor(db: &impl ConnectionTrait, project_id: Uuid) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_project_quota_claims (project_id, claimed_at) VALUES ($1, CURRENT_TIMESTAMP) ON CONFLICT (project_id) DO UPDATE SET claimed_at = EXCLUDED.claimed_at",
        [project_id.into()],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

const MAX_PENDING_REQUESTS_PER_PRINCIPAL: i64 = 20;
const MAX_PENDING_REQUESTS_PER_PROJECT: i64 = 100;

async fn over_pending_quota(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    principal_id: Uuid,
) -> Result<bool, DbErr> {
    let per_principal_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT COUNT(*) AS count FROM deployments WHERE project_id = $1 AND requested_by = $2 AND lifecycle_status IN ('REQUESTED', 'AWAITING_APPROVAL', 'APPROVED', 'IN_PROGRESS')",
        [project_id.into(), principal_id.into()],
    );
    let per_principal: i64 = db
        .query_one_raw(per_principal_statement)
        .await?
        .expect("COUNT(*) always returns exactly one row")
        .try_get_by("count")?;
    if per_principal >= MAX_PENDING_REQUESTS_PER_PRINCIPAL {
        return Ok(true);
    }
    let per_project_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT COUNT(*) AS count FROM deployments WHERE project_id = $1 AND lifecycle_status IN ('REQUESTED', 'AWAITING_APPROVAL', 'APPROVED', 'IN_PROGRESS')",
        [project_id.into()],
    );
    let per_project: i64 = db
        .query_one_raw(per_project_statement)
        .await?
        .expect("COUNT(*) always returns exactly one row")
        .try_get_by("count")?;
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, request_fingerprint FROM deployments WHERE project_id = $1 AND idempotency_key = $2",
        [project_id.into(), key.trim().into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(IdempotencyHit {
            id: row.try_get_by("id")?,
            fingerprint: row.try_get_by("request_fingerprint")?,
        })),
        None => Ok(None),
    }
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
    writes::compiler_review_fact_write(txn).await?;
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
    let claim_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND revision = $2",
        [deployment_id.into(), expected_revision.into()],
    );
    let claimed = txn.execute_raw(claim_statement).await?;
    if claimed.rows_affected() == 0 {
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
    let health_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Execution was canceled before runtime health became available.', \
             observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
        [deployment_id.into()],
    );
    txn.execute_raw(health_statement).await?;
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
/// attempts. Backs both `retry` and `rollback` — Java's own `recovery()` engine.
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
        if blank(reason) {
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
    writes::compiler_review_fact_write(txn).await?;
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

async fn deployment_locked(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Option<Deployment>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id FROM deployments WHERE id = $1 FOR UPDATE",
        [id.into()],
    );
    if db.query_one_raw(statement).await?.is_none() {
        return Ok(None);
    }
    Ok(rows::deployments(db, &[id], true).await?.into_iter().next())
}

async fn deployment_project(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<Uuid>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT project_id FROM deployments WHERE id = $1",
        [deployment_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(row.try_get_by("project_id")?)),
        None => Ok(None),
    }
}

fn canonical_target_version(value: Option<&str>) -> Option<String> {
    let value = value?;
    match Uuid::parse_str(value) {
        Ok(id) => Some(id.to_string()),
        Err(_) => Some(value.trim().to_string()),
    }
}

fn valid_optional_uuid(value: Option<&str>) -> bool {
    match value {
        None => true,
        Some(value) => Uuid::parse_str(value).is_ok(),
    }
}

async fn rollback_target_version(
    db: &impl ConnectionTrait,
    source: &Deployment,
    requested: Option<&str>,
) -> Result<Option<Uuid>, DbErr> {
    let requested_id = match requested {
        Some(value) => match Uuid::parse_str(value) {
            Ok(id) => Some(id),
            Err(_) => return Ok(None),
        },
        None => None,
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT deployment.agent_version_id FROM deployments deployment \
           JOIN deployment_runtime_health health ON health.deployment_id = deployment.id \
         WHERE deployment.project_id = $1 AND deployment.agent_id = $2 AND deployment.environment_definition_version_id = $3 \
           AND deployment.lifecycle_status = 'ACTIVE' AND deployment.id <> $4 \
           AND (deployment.requested_at, deployment.id) < ($5::timestamptz, $6::uuid) \
           AND ($7::uuid IS NULL OR deployment.agent_version_id = $7::uuid) \
         ORDER BY deployment.requested_at DESC, deployment.id DESC LIMIT 1",
        [
            source.project_id.into(),
            source.agent_id.into(),
            source.environment.id.into(),
            source.id.into(),
            source.requested_at.into(),
            source.id.into(),
            requested_id.into(),
        ],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(row.try_get_by("agent_version_id")?)),
        None => Ok(None),
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT request_fingerprint, result_deployment_id FROM deployment_recovery_action_receipts WHERE source_deployment_id = $1 AND actor_principal_id = $2 AND action = $3 AND idempotency_key = $4",
        [source.into(), actor.into(), action.into(), key.trim().into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(ActionReceipt {
            fingerprint: row.try_get_by("request_fingerprint")?,
            result_deployment_id: row.try_get_by("result_deployment_id")?,
        })),
        None => Ok(None),
    }
}

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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_recovery_action_receipts (id, source_deployment_id, actor_principal_id, action, idempotency_key, request_fingerprint, result_deployment_id) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        [
            id.into(),
            source.into(),
            actor.into(),
            action.into(),
            key.trim().into(),
            fingerprint.into(),
            result_deployment.into(),
        ],
    );
    db.execute_raw(statement).await?;
    Ok(id)
}

async fn record_promotion(
    db: &impl ConnectionTrait,
    receipt: Uuid,
    deployment: &Deployment,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_promotion_facts (id, deployment_id, action_receipt_id, agent_version_id, target_digest, runtime_health_generation) VALUES ($1, $2, $3, $4, $5, $6)",
        [
            Uuid::new_v4().into(),
            deployment.id.into(),
            receipt.into(),
            deployment.agent_version_id.into(),
            deployment.plan.target_digest.clone().into(),
            deployment.runtime_health.generation.into(),
        ],
    );
    db.execute_raw(statement).await?;
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
    let reason_digest = if blank(reason) {
        String::new()
    } else {
        digest(reason.unwrap().trim())
    };
    let confirmation_digest = if blank(confirmation) {
        String::new()
    } else {
        digest(confirmation.unwrap().trim())
    };
    digest(&format!(
        "{action}|{deployment_id}|{revision}|{}|{reason_digest}|{confirmation_digest}",
        target_version.unwrap_or("")
    ))
}
