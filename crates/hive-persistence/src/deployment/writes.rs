//! Ports the shared timeline/audit/outbox/insert helpers every mutation and
//! the worker call: `audit`, `stage`, `enqueue`, `touchProjection`, the
//! timeline-sequence allocators, and the new-deployment-cycle insert helpers
//! (`insertDeployment`/`insertPlan`/.../`insertApprovalRequirement`).

use crate::sql::json_array;
use hive_application::deployment::CompiledRequest;
use hive_domain::deployment::DeploymentLifecycleStatus;
use serde_json::Value;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

/// Makes every returned detail/timeline append visible to stale-response protection.
pub async fn touch_projection(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE deployments SET projection_revision = projection_revision + 1 WHERE id = $1",
    )
    .bind(deployment_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn lock_timeline_deployment(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<(), sqlx::Error> {
    let found = sqlx::query("SELECT id FROM deployments WHERE id = $1 FOR UPDATE")
        .bind(deployment_id)
        .fetch_optional(&mut *conn)
        .await?;
    if found.is_none() {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

/// Allocates a cross-table sequence while the deployment row serializes writers for this target.
pub async fn next_timeline_sequence(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    attempt_number: i64,
) -> Result<i64, sqlx::Error> {
    lock_timeline_deployment(conn, deployment_id).await?;
    let row = sqlx::query(
        "INSERT INTO deployment_timeline_counters (deployment_id, attempt_number, next_sequence) VALUES ($1, $2, 2) \
         ON CONFLICT (deployment_id, attempt_number) DO UPDATE SET next_sequence = deployment_timeline_counters.next_sequence + 1 \
         RETURNING next_sequence - 1",
    )
    .bind(deployment_id)
    .bind(attempt_number)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.get(0))
}

pub struct TimelineAnchor {
    pub attempt_id: Option<Uuid>,
    pub attempt_number: i64,
    pub sequence: i64,
}

async fn timeline_anchor(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    supplied_attempt: Option<Uuid>,
) -> Result<TimelineAnchor, sqlx::Error> {
    lock_timeline_deployment(conn, deployment_id).await?;
    let mut attempt_id = supplied_attempt;
    let mut attempt_number = 0i64;
    if let Some(id) = attempt_id {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT attempt_number FROM deployment_attempts WHERE id = $1 AND deployment_id = $2",
        )
        .bind(id)
        .bind(deployment_id)
        .fetch_optional(&mut *conn)
        .await?;
        match row {
            Some((number,)) => attempt_number = number,
            None => attempt_id = None,
        }
    }
    if attempt_id.is_none() {
        let row: Option<(Uuid, i64)> = sqlx::query_as("SELECT id, attempt_number FROM deployment_attempts WHERE deployment_id = $1 ORDER BY attempt_number DESC LIMIT 1").bind(deployment_id).fetch_optional(&mut *conn).await?;
        if let Some((id, number)) = row {
            attempt_id = Some(id);
            attempt_number = number;
        }
    }
    let sequence = next_timeline_sequence(conn, deployment_id, attempt_number).await?;
    Ok(TimelineAnchor {
        attempt_id,
        attempt_number,
        sequence,
    })
}

pub struct AttemptTimelineAnchor {
    pub deployment_id: Uuid,
    pub attempt_number: i64,
}

async fn attempt_timeline_anchor(
    conn: &mut PgConnection,
    attempt_id: Uuid,
) -> Result<AttemptTimelineAnchor, sqlx::Error> {
    let row: Option<(Uuid, i64)> = sqlx::query_as(
        "SELECT deployment_id, attempt_number FROM deployment_attempts WHERE id = $1",
    )
    .bind(attempt_id)
    .fetch_optional(&mut *conn)
    .await?;
    let (deployment_id, attempt_number) = row.ok_or(sqlx::Error::RowNotFound)?;
    Ok(AttemptTimelineAnchor {
        deployment_id,
        attempt_number,
    })
}

pub async fn stage(
    conn: &mut PgConnection,
    attempt_id: Uuid,
    stage: &str,
    status: &str,
    message: &str,
) -> Result<(), sqlx::Error> {
    let anchor = attempt_timeline_anchor(conn, attempt_id).await?;
    let timeline_sequence =
        next_timeline_sequence(conn, anchor.deployment_id, anchor.attempt_number).await?;
    sqlx::query(
        "INSERT INTO deployment_stage_events (id, deployment_attempt_id, sequence_number, timeline_sequence, stage, status, message) \
         SELECT $1, $2, COALESCE(MAX(sequence_number), 0) + 1, $3, $4, $5, $6 FROM deployment_stage_events WHERE deployment_attempt_id = $2",
    )
    .bind(Uuid::new_v4())
    .bind(attempt_id)
    .bind(timeline_sequence)
    .bind(stage)
    .bind(status)
    .bind(message)
    .execute(&mut *conn)
    .await?;
    touch_projection(conn, anchor.deployment_id).await
}

pub async fn enqueue(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    event_type: &str,
    mode: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO deployment_outbox_events (id, deployment_id, event_type, payload, status) VALUES ($1, $2, $3, $4::jsonb, 'PENDING')")
        .bind(Uuid::new_v4())
        .bind(deployment_id)
        .bind(event_type)
        .bind(format!("{{\"mode\":\"{mode}\"}}"))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

fn attempt_id_from_facts(facts: &Value) -> Option<Uuid> {
    facts
        .get("attemptId")?
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
}

/// `actor` is `None` for a system-attributed event (Java passes `null`), matching every worker-side
/// audit call. Request metadata (`request_id`/`correlation_id`/`graphql_operation`/`source_ip`/
/// `user_agent`) is bound from `crate::audit::context`'s task-local (`None` outside any
/// `/graphql` request, e.g. a worker call), matching Java's `PostgresAuditRequestContext`.
pub async fn audit(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    actor: Option<Uuid>,
    action: &str,
    facts: Value,
) -> Result<(), sqlx::Error> {
    let attempt_id = attempt_id_from_facts(&facts);
    let anchor = timeline_anchor(conn, deployment_id, attempt_id).await?;
    crate::audit::bind_audit_metadata(
        sqlx::query(
            "INSERT INTO deployment_audit_events \
               (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
                request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, $3, $4, $5::jsonb, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(Uuid::new_v4())
        .bind(deployment_id)
        .bind(actor)
        .bind(action)
        .bind(facts.to_string())
        .bind(anchor.attempt_id)
        .bind(anchor.attempt_number)
        .bind(anchor.sequence),
    )
    .execute(&mut *conn)
    .await?;
    touch_projection(conn, deployment_id).await
}

pub async fn insert_runtime_health(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO deployment_runtime_health (deployment_id, status, summary, generation) VALUES ($1, 'NOT_OBSERVED', 'No local runtime observation is available yet.', 1)")
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Ports `PostgresEvaluationTargetProjection.projectDeploymentTarget`.
pub async fn project_deployment_target(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO evaluation_target_projections (project_id, target_kind, target_id, agent_version_id, \
             environment_definition_version_id, logical_environment_class, display_name) \
         SELECT deployment.project_id, 'DEPLOYMENT', deployment.id, deployment.agent_version_id, environment.id, \
             environment.logical_environment_class, 'Deployment ' || deployment.id::text \
         FROM deployments deployment JOIN environment_definition_versions environment ON environment.id = deployment.environment_definition_version_id \
         WHERE deployment.id = $1 \
         ON CONFLICT (target_kind, target_id, environment_definition_version_id) DO UPDATE \
           SET project_id = EXCLUDED.project_id, agent_version_id = EXCLUDED.agent_version_id, \
               logical_environment_class = EXCLUDED.logical_environment_class, display_name = EXCLUDED.display_name",
    )
    .bind(deployment_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn insert_deployment(
    conn: &mut PgConnection,
    id: Uuid,
    actor: Uuid,
    request: &CompiledRequest,
    key: &str,
    fingerprint: &str,
) -> Result<(), sqlx::Error> {
    let lifecycle = if request.rule.approvers > 0 {
        DeploymentLifecycleStatus::AwaitingApproval
    } else {
        DeploymentLifecycleStatus::Requested
    }
    .as_str();
    sqlx::query(
        "INSERT INTO deployments (id, organization_id, project_id, agent_id, agent_version_id, catalog_release_id, catalog_release_digest, \
             environment, environment_definition_version_id, target_digest, strategy, lifecycle_status, revision, idempotency_key, request_fingerprint, requested_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 1, $13, $14, $15)",
    )
    .bind(id)
    .bind(request.version.organization_id)
    .bind(request.version.project_id)
    .bind(request.version.agent_id)
    .bind(request.version.id)
    .bind(&request.version.catalog_release_id)
    .bind(&request.version.catalog_release_digest)
    .bind(&request.environment.logical_environment_class)
    .bind(request.environment.id)
    .bind(&request.target_digest)
    .bind(&request.strategy)
    .bind(lifecycle)
    .bind(key.trim())
    .bind(fingerprint)
    .bind(actor)
    .execute(&mut *conn)
    .await?;
    project_deployment_target(conn, id).await
}

pub async fn insert_plan(
    conn: &mut PgConnection,
    plan_id: Uuid,
    deployment_id: Uuid,
    actor: Uuid,
    request: &CompiledRequest,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO deployment_plan_versions (id, deployment_id, version_number, agent_version_id, catalog_release_id, environment, \
             environment_definition_version_id, agent_content_digest, catalog_release_digest, target_digest, compiler_version, canonical_plan, \
             plan_digest, package_digest, package_reference, created_by) \
         VALUES ($1, $2, 1, $3, $4, $5, $6, $7, $8, $9, $10, $11::jsonb, $12, $13, $14, $15)",
    )
    .bind(plan_id)
    .bind(deployment_id)
    .bind(request.version.id)
    .bind(&request.version.catalog_release_id)
    .bind(&request.environment.logical_environment_class)
    .bind(request.environment.id)
    .bind(&request.version.content_digest)
    .bind(&request.version.catalog_release_digest)
    .bind(&request.target_digest)
    .bind(hive_application::deployment::compiler::COMPILER_VERSION)
    .bind(&request.canonical_plan)
    .bind(&request.plan_digest)
    .bind(&request.package_digest)
    .bind(format!("local://packages/{}", request.package_digest))
    .bind(actor)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Marks this transaction's plan before PostgreSQL evaluates the retained-writer compatibility trigger.
pub async fn compiler_review_fact_write(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('hive.m14_compiler_review', 'true', TRUE)")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn insert_plan_review(
    conn: &mut PgConnection,
    plan_id: Uuid,
    review: &hive_application::deployment::CompilerReview,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO deployment_plan_review_facts (plan_id, active_agent_version_number, change_summary, \
             requested_dependency_versions, added_dependency_versions, removed_dependency_versions) \
         SELECT $1, $2, $3, $4::jsonb, $5::jsonb, $6::jsonb \
         WHERE NOT EXISTS (SELECT 1 FROM deployment_plan_review_facts WHERE plan_id = $1)",
    )
    .bind(plan_id)
    .bind(review.active_agent_version_number)
    .bind(&review.change_summary)
    .bind(json_array(&review.requested_dependency_versions))
    .bind(json_array(&review.added_dependency_versions))
    .bind(json_array(&review.removed_dependency_versions))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn insert_policy_snapshot(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO deployment_policy_snapshots (deployment_id, policy_id, policy_revision, policy_digest, policy_matrix, \
             logical_environment_class, risk, required_evidence, required_approvers, agent_version_id, environment_definition_version_id, \
             target_digest, plan_digest, package_digest, binding_digest, risk_verification_digest, evaluation_requirement_expires_at) \
         SELECT $1, $2, $3, $4, $5::jsonb, $6, $7, $8::jsonb, $9, $10, $11, $12, $13, $14, $15, $16, $17 \
         FROM deployments deployment WHERE deployment.id = $1",
    )
    .bind(deployment_id)
    .bind(request.policy.id)
    .bind(request.policy.revision)
    .bind(&request.policy.digest)
    .bind(&request.policy.matrix)
    .bind(&request.environment.logical_environment_class)
    .bind(&request.risk)
    .bind(json_array(&request.rule.evidence))
    .bind(request.rule.approvers)
    .bind(request.version.id)
    .bind(request.environment.id)
    .bind(&request.target_digest)
    .bind(&request.plan_digest)
    .bind(&request.package_digest)
    .bind(&request.binding_digest)
    .bind(&request.risk_verification_digest)
    .bind(request.evaluation_requirement_expires_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn insert_evidence(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<(), sqlx::Error> {
    for kind in &request.rule.evidence {
        // M16 owns evaluation authoring. M14 freezes and validates only an evaluation fact that
        // that owner has already appended for this exact deployment target.
        if kind == "EVALUATION_PASSED" {
            continue;
        }
        sqlx::query(
            "INSERT INTO deployment_evidence_snapshots (id, deployment_id, evidence_kind, evidence_digest, expires_at, agent_version_id, \
                 environment_definition_version_id, target_digest, plan_digest, package_digest, binding_digest) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(Uuid::new_v4())
        .bind(deployment_id)
        .bind(kind)
        .bind(hive_application::deployment::compiler::digest(&format!("{}|{}", request.binding_digest, kind)))
        .bind(None::<chrono::DateTime<chrono::Utc>>)
        .bind(request.version.id)
        .bind(request.environment.id)
        .bind(&request.target_digest)
        .bind(&request.plan_digest)
        .bind(&request.package_digest)
        .bind(&request.binding_digest)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Inserts the one frozen approval requirement for this idempotent deployment cycle.
pub async fn insert_approval_requirement(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query(
        "INSERT INTO deployment_approval_requirements \
           (id, deployment_id, revision, organization_id, project_id, requested_at, required_approvers, status, expires_at, satisfied_at) \
         SELECT $1, deployment.id, 1, deployment.organization_id, deployment.project_id, deployment.requested_at, \
           $2, 'PENDING', deployment.requested_at + INTERVAL '24 hours', NULL \
         FROM deployments deployment WHERE deployment.id = $3 \
         ON CONFLICT (deployment_id) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(request.rule.approvers)
    .bind(deployment_id)
    .execute(&mut *conn)
    .await?;
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM deployment_approval_requirements WHERE deployment_id = $1")
            .bind(deployment_id)
            .fetch_optional(&mut *conn)
            .await?;
    row.map(|(id,)| id).ok_or(sqlx::Error::RowNotFound)
}

/// Terminalizes every still-`QUEUED`/`RUNNING` attempt for a deployment being force-terminated
/// (cancel, or the worker's dead-letter path), staging a matching timeline event for each.
pub async fn terminalize_running_attempts(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    status: &str,
    code: &str,
    summary: &str,
    stage_name: &str,
    stage_status: &str,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows = sqlx::query(
        "UPDATE deployment_attempts SET status = $1, completed_at = CURRENT_TIMESTAMP, generation = generation + 1, \
             failure_code = $2, failure_summary = $3 WHERE deployment_id = $4 AND status IN ('QUEUED', 'RUNNING') RETURNING id",
    )
    .bind(status)
    .bind(code)
    .bind(summary)
    .bind(deployment_id)
    .fetch_all(&mut *conn)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|row| row.get(0)).collect();
    for attempt_id in &ids {
        stage(conn, *attempt_id, stage_name, stage_status, summary).await?;
    }
    Ok(ids)
}
