//! Ports the five deployment lifecycle mutations: `deploy`, `cancel`,
//! `retry`/`rollback` (which share Java's `recovery()` engine — a thin
//! wrapper each in the trait impl, per `RecoveryAction`), and `promote`
//! (which does not go through `recovery()`).

use super::rows::{self};
use super::writes;
use crate::capability::tx;
use crate::sql::is_serialization_failure;
use hive_application::deployment::compiler::digest;
use hive_application::deployment::{
    ActiveTarget, CompiledRequest, Deployment, DeploymentMutationResult, DeploymentProblem,
    EnvironmentDefinition, PolicySource, VersionSource,
};
use hive_domain::deployment::DeploymentLifecycleStatus;
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db_error) if db_error.code().as_deref() == Some("23505"))
}

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
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    tx::has_deployment_capability(conn, principal_id, tx::DEPLOYMENT_VIEW, project_id, lock).await
}

async fn active_project_check(
    conn: &mut PgConnection,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    let sql = format!(
        "SELECT 1 FROM projects WHERE id = $1 AND lifecycle_status = 'ACTIVE'{}",
        if lock { " FOR SHARE" } else { "" }
    );
    Ok(sqlx::query(&sql)
        .bind(project_id)
        .fetch_optional(&mut *conn)
        .await?
        .is_some())
}

async fn version_source(
    conn: &mut PgConnection,
    version_id: Uuid,
    lock: bool,
) -> Result<Option<VersionSource>, sqlx::Error> {
    let suffix = if lock {
        " FOR KEY SHARE OF version, agent, project"
    } else {
        ""
    };
    let sql = format!(
        "SELECT version.id, project.id, agent.id, agent.display_name, version.version_number, version.content_digest, \
             version.catalog_release_id, version.catalog_release_digest, project.organization_id, version.canonical_document::text \
         FROM agent_versions version JOIN agents agent ON agent.id = version.agent_id JOIN projects project ON project.id = agent.project_id \
         WHERE version.id = $1{suffix}"
    );
    let row = sqlx::query(&sql)
        .bind(version_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.as_ref().map(rows::version_source_row))
}

async fn environment(
    conn: &mut PgConnection,
    environment_id: Uuid,
    release_id: &str,
) -> Result<Option<EnvironmentDefinition>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, stable_definition_id, version, display_name, logical_environment_class, catalog_release_id, catalog_release_digest, content_digest \
         FROM environment_definition_versions WHERE id = $1 AND catalog_release_id = $2",
    )
    .bind(environment_id)
    .bind(release_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.as_ref().map(rows::compiler_environment_row))
}

async fn policy(
    conn: &mut PgConnection,
    project_id: Uuid,
    lock: bool,
) -> Result<Option<PolicySource>, sqlx::Error> {
    let suffix = if lock {
        " FOR SHARE OF policy, version"
    } else {
        ""
    };
    let sql = format!(
        "SELECT policy.id, version.revision, version.digest, version.matrix::text \
         FROM project_approval_policies policy JOIN project_approval_policy_versions version \
           ON version.policy_id = policy.id AND version.revision = policy.current_revision WHERE policy.project_id = $1{suffix}"
    );
    let row: Option<(Uuid, i64, String, String)> = sqlx::query_as(&sql)
        .bind(project_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|(id, revision, digest, matrix)| PolicySource {
        id,
        revision,
        digest,
        matrix,
    }))
}

async fn active_target(
    conn: &mut PgConnection,
    project_id: Uuid,
    agent_id: Uuid,
    environment_id: Uuid,
    lock: bool,
) -> Result<Option<ActiveTarget>, sqlx::Error> {
    let suffix = if lock { " FOR SHARE OF deployment" } else { "" };
    let sql = format!(
        "SELECT plan.target_digest, version.canonical_document::text, deployment.id, deployment.agent_version_id, version.version_number, deployment.requested_at \
         FROM deployments deployment JOIN deployment_plan_versions plan ON plan.deployment_id = deployment.id AND plan.version_number = 1 \
           JOIN agent_versions version ON version.id = deployment.agent_version_id \
         WHERE deployment.project_id = $1 AND deployment.agent_id = $2 AND deployment.environment_definition_version_id = $3 \
           AND deployment.lifecycle_status = 'ACTIVE' \
         ORDER BY deployment.updated_at DESC, deployment.id DESC LIMIT 1{suffix}"
    );
    let row = sqlx::query(&sql)
        .bind(project_id)
        .bind(agent_id)
        .bind(environment_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|row| ActiveTarget {
        target_digest: row.get(0),
        canonical_document: row.get(1),
        deployment_id: row.get(2),
        agent_version_id: row.get(3),
        agent_version_number: row.get(4),
        requested_at: row.get(5),
    }))
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
    conn: &mut PgConnection,
    version_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT agent.project_id FROM agent_versions version JOIN agents agent ON agent.id = version.agent_id WHERE version.id = $1").bind(version_id).fetch_optional(&mut *conn).await?;
    Ok(row.map(|(id,)| id))
}

async fn quota_anchor(conn: &mut PgConnection, project_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO deployment_project_quota_claims (project_id, claimed_at) VALUES ($1, CURRENT_TIMESTAMP) ON CONFLICT (project_id) DO UPDATE SET claimed_at = EXCLUDED.claimed_at")
        .bind(project_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

const MAX_PENDING_REQUESTS_PER_PRINCIPAL: i64 = 20;
const MAX_PENDING_REQUESTS_PER_PROJECT: i64 = 100;

async fn over_pending_quota(
    conn: &mut PgConnection,
    project_id: Uuid,
    principal_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let per_principal: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM deployments WHERE project_id = $1 AND requested_by = $2 AND lifecycle_status IN ('REQUESTED', 'AWAITING_APPROVAL', 'APPROVED', 'IN_PROGRESS')")
        .bind(project_id)
        .bind(principal_id)
        .fetch_one(&mut *conn)
        .await?;
    if per_principal.0 >= MAX_PENDING_REQUESTS_PER_PRINCIPAL {
        return Ok(true);
    }
    let per_project: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM deployments WHERE project_id = $1 AND lifecycle_status IN ('REQUESTED', 'AWAITING_APPROVAL', 'APPROVED', 'IN_PROGRESS')").bind(project_id).fetch_one(&mut *conn).await?;
    Ok(per_project.0 >= MAX_PENDING_REQUESTS_PER_PROJECT)
}

struct IdempotencyHit {
    id: Uuid,
    fingerprint: String,
}

async fn idempotency(
    conn: &mut PgConnection,
    project_id: Uuid,
    key: &str,
) -> Result<Option<IdempotencyHit>, sqlx::Error> {
    let row: Option<(Uuid, String)> = sqlx::query_as("SELECT id, request_fingerprint FROM deployments WHERE project_id = $1 AND idempotency_key = $2").bind(project_id).bind(key.trim()).fetch_optional(&mut *conn).await?;
    Ok(row.map(|(id, fingerprint)| IdempotencyHit { id, fingerprint }))
}

pub async fn deploy(
    pool: &PgPool,
    principal_id: Uuid,
    request: &CompiledRequest,
    key: &str,
) -> Result<DeploymentMutationResult, sqlx::Error> {
    let version_id = request.version.id;
    let environment_id = request.environment.id;
    let strategy = request.strategy.clone();

    let mut tx = pool.begin().await?;
    let result = deploy_tx(&mut tx, principal_id, request, key).await;
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            if is_unique_violation(&error) && valid_key(key) {
                let mut retry = pool.begin().await?;
                if let Some(project_id) = project_for_version(&mut retry, version_id).await? {
                    if let Some(existing) = idempotency(&mut retry, project_id, key).await? {
                        let fingerprint =
                            digest(&format!("{version_id}|{environment_id}|{strategy}"));
                        return if existing.fingerprint == fingerprint {
                            match rows::deployments(&mut retry, &[existing.id], true)
                                .await?
                                .into_iter()
                                .next()
                            {
                                Some(deployment) => {
                                    Ok(DeploymentMutationResult::success(deployment))
                                }
                                None => Err(sqlx::Error::RowNotFound),
                            }
                        } else {
                            Ok(DeploymentMutationResult::refused(
                                DeploymentProblem::idempotency(),
                            ))
                        };
                    }
                }
            }
            // Two overlapping deploy() calls for the same project both wrote quotaAnchor()'s claim
            // row; DSQL let both proceed and only detected the conflict here, at the loser's commit.
            if is_serialization_failure(&error) {
                return Ok(DeploymentMutationResult::refused(
                    DeploymentProblem::rate_limited(),
                ));
            }
            Err(error)
        }
    }
}

async fn deploy_tx(
    tx: &mut PgConnection,
    principal_id: Uuid,
    request: &CompiledRequest,
    key: &str,
) -> Result<DeploymentMutationResult, sqlx::Error> {
    let version_id = request.version.id;
    let environment_id = request.environment.id;

    let Some(preauthorized) = version_source(tx, version_id, false).await? else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(tx, principal_id, preauthorized.project_id, false).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    if !tx::has_deployment_capability(
        tx,
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
    quota_anchor(tx, preauthorized.project_id).await?;
    let Some(version) = version_source(tx, version_id, true).await? else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(tx, principal_id, version.project_id, true).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    if !tx::has_deployment_capability(
        tx,
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
    if !active_project_check(tx, version.project_id, true).await? || !valid_key(key) {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::invalid(),
        ));
    }
    let environment_value = environment(tx, environment_id, &version.catalog_release_id).await?;
    let policy_value = policy(tx, version.project_id, true).await?;
    let current = active_target(
        tx,
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
    if let Some(existing) = idempotency(tx, version.project_id, key).await? {
        return if existing.fingerprint == fingerprint {
            match rows::deployments(tx, &[existing.id], true)
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
    if over_pending_quota(tx, version.project_id, principal_id).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::rate_limited(),
        ));
    }
    let deployment_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    writes::insert_deployment(tx, deployment_id, principal_id, request, key, &fingerprint).await?;
    writes::compiler_review_fact_write(tx).await?;
    writes::insert_plan(tx, plan_id, deployment_id, principal_id, request).await?;
    writes::insert_plan_review(tx, plan_id, &request.review).await?;
    writes::insert_policy_snapshot(tx, deployment_id, request).await?;
    writes::insert_approval_requirement(tx, deployment_id, request).await?;
    writes::insert_evidence(tx, deployment_id, request).await?;
    writes::insert_runtime_health(tx, deployment_id).await?;
    writes::audit(
        tx,
        deployment_id,
        Some(principal_id),
        "REQUESTED",
        serde_json::json!({"bindingDigest": request.binding_digest, "environmentDefinitionVersionId": environment_id.to_string(), "planDigest": request.plan_digest}),
    )
    .await?;
    crate::deployment::approval::automatic_approval_handoff(tx, deployment_id).await?;
    let result = rows::deployments(tx, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(DeploymentMutationResult::success(result))
}

pub async fn cancel(
    pool: &PgPool,
    principal_id: Uuid,
    deployment_id: Uuid,
    expected_revision: i64,
    reason: &str,
) -> Result<DeploymentMutationResult, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let Some(current) = rows::deployments(&mut tx, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
    else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(&mut tx, principal_id, current.project_id, true).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    if !tx::has_deployment_capability(
        &mut tx,
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
    let claimed = sqlx::query("UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND revision = $2")
        .bind(deployment_id)
        .bind(expected_revision)
        .execute(&mut *tx)
        .await?;
    if claimed.rows_affected() == 0 {
        let raced = rows::deployments(&mut tx, &[deployment_id], true)
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
        &mut tx,
        deployment_id,
        "TERMINAL_LIFECYCLE",
        Some(principal_id),
    )
    .await?;
    writes::terminalize_running_attempts(
        &mut tx,
        deployment_id,
        "CANCELED",
        "LOCAL_CANCELED",
        "The requester canceled this local deployment.",
        "CANCELED",
        "CANCELED",
    )
    .await?;
    sqlx::query(
        "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Execution was canceled before runtime health became available.', \
             observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
    )
    .bind(deployment_id)
    .execute(&mut *tx)
    .await?;
    writes::audit(
        &mut tx,
        deployment_id,
        Some(principal_id),
        "CANCELED",
        serde_json::json!({"expectedRevision": expected_revision, "reason": safe_reason(reason)}),
    )
    .await?;
    let result = rows::deployments(&mut tx, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
        .ok_or(sqlx::Error::RowNotFound)?;
    match tx.commit().await {
        Ok(()) => Ok(DeploymentMutationResult::success(result)),
        Err(error) if is_serialization_failure(&error) => {
            let mut retry = pool.begin().await?;
            let raced = rows::deployments(&mut retry, &[deployment_id], true)
                .await?
                .into_iter()
                .next();
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
    pool: &PgPool,
    principal_id: Uuid,
    deployment_id: Uuid,
    expected_revision: i64,
    key: &str,
) -> Result<DeploymentMutationResult, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let Some(preauthorized_project) = deployment_project(&mut tx, deployment_id).await? else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(&mut tx, principal_id, preauthorized_project, false).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    if !tx::has_deployment_capability(
        &mut tx,
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
    let Some(current) = deployment_locked(&mut tx, deployment_id).await? else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(&mut tx, principal_id, current.project_id, true).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    if !tx::has_deployment_capability(
        &mut tx,
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
        action_receipt(&mut tx, deployment_id, principal_id, "PROMOTE", Some(key)).await?
    {
        return if replay.fingerprint == fingerprint {
            match rows::deployments(&mut tx, &[replay.result_deployment_id], true)
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
        &mut tx,
        deployment_id,
        principal_id,
        "PROMOTE",
        key,
        &fingerprint,
        deployment_id,
    )
    .await?;
    record_promotion(&mut tx, receipt, &current).await?;
    writes::audit(&mut tx, deployment_id, Some(principal_id), "PROMOTION_RECORDED", serde_json::json!({"receiptId": receipt.to_string(), "targetDigest": current.plan.target_digest})).await?;
    let result = rows::deployments(&mut tx, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
        .ok_or(sqlx::Error::RowNotFound)?;
    match tx.commit().await {
        Ok(()) => Ok(DeploymentMutationResult::success(result)),
        Err(error) if is_serialization_failure(&error) => {
            let mut retry = pool.begin().await?;
            let raced = rows::deployments(&mut retry, &[deployment_id], true)
                .await?
                .into_iter()
                .next();
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
    pool: &PgPool,
    principal_id: Uuid,
    deployment_id: Uuid,
    expected_revision: i64,
    key: &str,
    action: RecoveryAction,
    requested_target_version: Option<&str>,
    reason: Option<&str>,
    production_confirmation: Option<&str>,
    request: Option<&CompiledRequest>,
) -> Result<DeploymentMutationResult, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let result = recovery_tx(
        &mut tx,
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
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            // Same quotaAnchor() commit-time race deploy() catches.
            if is_serialization_failure(&error) {
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
    tx: &mut PgConnection,
    principal_id: Uuid,
    deployment_id: Uuid,
    expected_revision: i64,
    key: &str,
    action: &RecoveryAction,
    requested_target_version: Option<&str>,
    reason: Option<&str>,
    production_confirmation: Option<&str>,
    request: Option<&CompiledRequest>,
) -> Result<DeploymentMutationResult, sqlx::Error> {
    let Some(preauthorized_project) = deployment_project(tx, deployment_id).await? else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(tx, principal_id, preauthorized_project, false).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    let capability = if matches!(action, RecoveryAction::Retry) {
        tx::DEPLOYMENT_RETRY
    } else {
        tx::DEPLOYMENT_ROLLBACK
    };
    if !tx::has_deployment_capability(tx, principal_id, capability, preauthorized_project, false)
        .await?
    {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::forbidden(),
        ));
    }
    quota_anchor(tx, preauthorized_project).await?;
    let Some(source) = deployment_locked(tx, deployment_id).await? else {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    };
    if !can_view(tx, principal_id, source.project_id, true).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::not_found(),
        ));
    }
    if !tx::has_deployment_capability(tx, principal_id, capability, source.project_id, true).await?
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
        action_receipt(tx, deployment_id, principal_id, action.name(), Some(key)).await?
    {
        return if replay.fingerprint == fingerprint {
            match rows::deployments(tx, &[replay.result_deployment_id], true)
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
    if !active_project_check(tx, source.project_id, true).await? {
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
        rollback_target_version(tx, &source, canonical_target.as_deref()).await?
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
    if !same_recovery_compilation_inputs(tx, request, &source, version_id).await? {
        return Ok(DeploymentMutationResult::refused(
            DeploymentProblem::conflict(deployment_id, expected_revision, source.revision),
        ));
    }
    if over_pending_quota(tx, source.project_id, principal_id).await? {
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
        tx,
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
    writes::compiler_review_fact_write(tx).await?;
    writes::insert_plan(tx, plan_id, child_id, principal_id, request).await?;
    writes::insert_plan_review(tx, plan_id, &request.review).await?;
    writes::insert_policy_snapshot(tx, child_id, request).await?;
    writes::insert_approval_requirement(tx, child_id, request).await?;
    writes::insert_evidence(tx, child_id, request).await?;
    writes::insert_runtime_health(tx, child_id).await?;
    writes::audit(
        tx,
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
        tx,
        deployment_id,
        principal_id,
        action.name(),
        key,
        &fingerprint,
        child_id,
    )
    .await?;
    writes::audit(
        tx,
        deployment_id,
        Some(principal_id),
        &format!("{}_RECORDED", action.name()),
        serde_json::json!({"receiptId": receipt.to_string(), "resultDeploymentId": child_id.to_string(), "reason": safe_recovery_reason(reason)}),
    )
    .await?;
    crate::deployment::approval::automatic_approval_handoff(tx, child_id).await?;
    let result = rows::deployments(tx, &[child_id], true)
        .await?
        .into_iter()
        .next()
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(DeploymentMutationResult::success(result))
}

async fn deployment_locked(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<Deployment>, sqlx::Error> {
    if sqlx::query("SELECT id FROM deployments WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    Ok(rows::deployments(conn, &[id], true)
        .await?
        .into_iter()
        .next())
}

async fn deployment_project(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT project_id FROM deployments WHERE id = $1")
        .bind(deployment_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|(id,)| id))
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
    conn: &mut PgConnection,
    source: &Deployment,
    requested: Option<&str>,
) -> Result<Option<Uuid>, sqlx::Error> {
    let requested_id = match requested {
        Some(value) => match Uuid::parse_str(value) {
            Ok(id) => Some(id),
            Err(_) => return Ok(None),
        },
        None => None,
    };
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT deployment.agent_version_id FROM deployments deployment \
           JOIN deployment_runtime_health health ON health.deployment_id = deployment.id \
         WHERE deployment.project_id = $1 AND deployment.agent_id = $2 AND deployment.environment_definition_version_id = $3 \
           AND deployment.lifecycle_status = 'ACTIVE' AND deployment.id <> $4 \
           AND (deployment.requested_at, deployment.id) < ($5::timestamptz, $6::uuid) \
           AND ($7::uuid IS NULL OR deployment.agent_version_id = $7::uuid) \
         ORDER BY deployment.requested_at DESC, deployment.id DESC LIMIT 1",
    )
    .bind(source.project_id)
    .bind(source.agent_id)
    .bind(source.environment.id)
    .bind(source.id)
    .bind(source.requested_at)
    .bind(source.id)
    .bind(requested_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|(id,)| id))
}

async fn recovery_compilation_inputs(
    conn: &mut PgConnection,
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
    sqlx::Error,
> {
    let Some(version) = version_source(conn, version_id, lock).await? else {
        return Ok(None);
    };
    if version.project_id != source.project_id || version.agent_id != source.agent_id {
        return Ok(None);
    }
    let environment_value =
        environment(conn, source.environment.id, &version.catalog_release_id).await?;
    let policy_value = policy(conn, source.project_id, lock).await?;
    let current_target = active_target(
        conn,
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
    conn: &mut PgConnection,
    request: &CompiledRequest,
    source: &Deployment,
    version_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let Some((version, environment_value, policy_value, current_target)) =
        recovery_compilation_inputs(conn, source, version_id, true).await?
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
    conn: &mut PgConnection,
    source: Uuid,
    actor: Uuid,
    action: &str,
    key: Option<&str>,
) -> Result<Option<ActionReceipt>, sqlx::Error> {
    let Some(key) = key else {
        return Ok(None);
    };
    let row: Option<(String, Uuid)> =
        sqlx::query_as("SELECT request_fingerprint, result_deployment_id FROM deployment_recovery_action_receipts WHERE source_deployment_id = $1 AND actor_principal_id = $2 AND action = $3 AND idempotency_key = $4")
            .bind(source)
            .bind(actor)
            .bind(action)
            .bind(key.trim())
            .fetch_optional(&mut *conn)
            .await?;
    Ok(
        row.map(|(fingerprint, result_deployment_id)| ActionReceipt {
            fingerprint,
            result_deployment_id,
        }),
    )
}

async fn record_action_receipt(
    conn: &mut PgConnection,
    source: Uuid,
    actor: Uuid,
    action: &str,
    key: &str,
    fingerprint: &str,
    result_deployment: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO deployment_recovery_action_receipts (id, source_deployment_id, actor_principal_id, action, idempotency_key, request_fingerprint, result_deployment_id) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(id)
        .bind(source)
        .bind(actor)
        .bind(action)
        .bind(key.trim())
        .bind(fingerprint)
        .bind(result_deployment)
        .execute(&mut *conn)
        .await?;
    Ok(id)
}

async fn record_promotion(
    conn: &mut PgConnection,
    receipt: Uuid,
    deployment: &Deployment,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO deployment_promotion_facts (id, deployment_id, action_receipt_id, agent_version_id, target_digest, runtime_health_generation) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(Uuid::new_v4())
        .bind(deployment.id)
        .bind(receipt)
        .bind(deployment.agent_version_id)
        .bind(&deployment.plan.target_digest)
        .bind(deployment.runtime_health.generation)
        .execute(&mut *conn)
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
