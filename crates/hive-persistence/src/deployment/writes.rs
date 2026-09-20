//! Ports the shared timeline/audit/outbox/insert helpers every mutation and
//! the worker call: `audit`, `stage`, `enqueue`, `touchProjection`, the
//! timeline-sequence allocators, and the new-deployment-cycle insert helpers
//! (`insertDeployment`/`insertPlan`/.../`insertApprovalRequirement`).

use crate::sql::json_array;
use hive_application::deployment::CompiledRequest;
use hive_domain::deployment::DeploymentLifecycleStatus;
use sea_orm::{ConnectionTrait, DbErr, Statement};
use serde_json::Value;
use uuid::Uuid;

/// Makes every returned detail/timeline append visible to stale-response protection.
pub async fn touch_projection(db: &impl ConnectionTrait, deployment_id: Uuid) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployments SET projection_revision = projection_revision + 1 WHERE id = $1",
        [deployment_id.into()],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

async fn lock_timeline_deployment(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id FROM deployments WHERE id = $1 FOR UPDATE",
        [deployment_id.into()],
    );
    if db.query_one_raw(statement).await?.is_none() {
        return Err(DbErr::RecordNotFound(format!(
            "no deployment with id {deployment_id}"
        )));
    }
    Ok(())
}

/// Allocates a cross-table sequence while the deployment row serializes writers for this target.
pub async fn next_timeline_sequence(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    attempt_number: i64,
) -> Result<i64, DbErr> {
    lock_timeline_deployment(db, deployment_id).await?;
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_timeline_counters (deployment_id, attempt_number, next_sequence) VALUES ($1, $2, 2) \
         ON CONFLICT (deployment_id, attempt_number) DO UPDATE SET next_sequence = deployment_timeline_counters.next_sequence + 1 \
         RETURNING next_sequence - 1 AS next_sequence",
        [deployment_id.into(), attempt_number.into()],
    );
    db.query_one_raw(statement)
        .await?
        .expect("the ON CONFLICT DO UPDATE always returns exactly one row")
        .try_get_by("next_sequence")
}

pub struct TimelineAnchor {
    pub attempt_id: Option<Uuid>,
    pub attempt_number: i64,
    pub sequence: i64,
}

async fn timeline_anchor(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    supplied_attempt: Option<Uuid>,
) -> Result<TimelineAnchor, DbErr> {
    lock_timeline_deployment(db, deployment_id).await?;
    let mut attempt_id = supplied_attempt;
    let mut attempt_number = 0i64;
    if let Some(id) = attempt_id {
        let statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT attempt_number FROM deployment_attempts WHERE id = $1 AND deployment_id = $2",
            [id.into(), deployment_id.into()],
        );
        match db.query_one_raw(statement).await? {
            Some(row) => attempt_number = row.try_get_by("attempt_number")?,
            None => attempt_id = None,
        }
    }
    if attempt_id.is_none() {
        let statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT id, attempt_number FROM deployment_attempts WHERE deployment_id = $1 ORDER BY attempt_number DESC LIMIT 1",
            [deployment_id.into()],
        );
        if let Some(row) = db.query_one_raw(statement).await? {
            attempt_id = Some(row.try_get_by("id")?);
            attempt_number = row.try_get_by("attempt_number")?;
        }
    }
    let sequence = next_timeline_sequence(db, deployment_id, attempt_number).await?;
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
    db: &impl ConnectionTrait,
    attempt_id: Uuid,
) -> Result<AttemptTimelineAnchor, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT deployment_id, attempt_number FROM deployment_attempts WHERE id = $1",
        [attempt_id.into()],
    );
    let row = db.query_one_raw(statement).await?.ok_or_else(|| {
        DbErr::RecordNotFound(format!("no deployment_attempts row with id {attempt_id}"))
    })?;
    Ok(AttemptTimelineAnchor {
        deployment_id: row.try_get_by("deployment_id")?,
        attempt_number: row.try_get_by("attempt_number")?,
    })
}

pub async fn stage(
    db: &impl ConnectionTrait,
    attempt_id: Uuid,
    stage: &str,
    status: &str,
    message: &str,
) -> Result<(), DbErr> {
    let anchor = attempt_timeline_anchor(db, attempt_id).await?;
    let timeline_sequence =
        next_timeline_sequence(db, anchor.deployment_id, anchor.attempt_number).await?;
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_stage_events (id, deployment_attempt_id, sequence_number, timeline_sequence, stage, status, message) \
         SELECT $1, $2, COALESCE(MAX(sequence_number), 0) + 1, $3, $4, $5, $6 FROM deployment_stage_events WHERE deployment_attempt_id = $2",
        [
            Uuid::new_v4().into(),
            attempt_id.into(),
            timeline_sequence.into(),
            stage.into(),
            status.into(),
            message.into(),
        ],
    );
    db.execute_raw(statement).await?;
    touch_projection(db, anchor.deployment_id).await
}

pub async fn enqueue(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    event_type: &str,
    mode: &str,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_outbox_events (id, deployment_id, event_type, payload, status) VALUES ($1, $2, $3, $4::jsonb, 'PENDING')",
        [
            Uuid::new_v4().into(),
            deployment_id.into(),
            event_type.into(),
            format!("{{\"mode\":\"{mode}\"}}").into(),
        ],
    );
    db.execute_raw(statement).await?;
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
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    actor: Option<Uuid>,
    action: &str,
    facts: Value,
) -> Result<(), DbErr> {
    let attempt_id = attempt_id_from_facts(&facts);
    let anchor = timeline_anchor(db, deployment_id, attempt_id).await?;
    let mut values: Vec<sea_orm::Value> = vec![
        Uuid::new_v4().into(),
        deployment_id.into(),
        actor.into(),
        action.into(),
        facts.to_string().into(),
        anchor.attempt_id.into(),
        anchor.attempt_number.into(),
        anchor.sequence.into(),
    ];
    values.extend(crate::audit::context::audit_metadata_values());
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_audit_events \
           (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
            request_id, correlation_id, graphql_operation, source_ip, user_agent) \
         VALUES ($1, $2, $3, $4, $5::jsonb, $6, $7, $8, $9, $10, $11, $12, $13)",
        values,
    );
    db.execute_raw(statement).await?;
    touch_projection(db, deployment_id).await
}

pub async fn insert_runtime_health(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_runtime_health (deployment_id, status, summary, generation) VALUES ($1, 'NOT_OBSERVED', 'No local runtime observation is available yet.', 1)",
        [deployment_id.into()],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

/// Ports `PostgresEvaluationTargetProjection.projectDeploymentTarget`.
pub async fn project_deployment_target(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO evaluation_target_projections (project_id, target_kind, target_id, agent_version_id, \
             environment_definition_version_id, logical_environment_class, display_name) \
         SELECT deployment.project_id, 'DEPLOYMENT', deployment.id, deployment.agent_version_id, environment.id, \
             environment.logical_environment_class, 'Deployment ' || deployment.id::text \
         FROM deployments deployment JOIN environment_definition_versions environment ON environment.id = deployment.environment_definition_version_id \
         WHERE deployment.id = $1 \
         ON CONFLICT (target_kind, target_id, environment_definition_version_id) DO UPDATE \
           SET project_id = EXCLUDED.project_id, agent_version_id = EXCLUDED.agent_version_id, \
               logical_environment_class = EXCLUDED.logical_environment_class, display_name = EXCLUDED.display_name",
        [deployment_id.into()],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

pub async fn insert_deployment(
    db: &impl ConnectionTrait,
    id: Uuid,
    actor: Uuid,
    request: &CompiledRequest,
    key: &str,
    fingerprint: &str,
) -> Result<(), DbErr> {
    let lifecycle = if request.rule.approvers > 0 {
        DeploymentLifecycleStatus::AwaitingApproval
    } else {
        DeploymentLifecycleStatus::Requested
    }
    .as_str();
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployments (id, organization_id, project_id, agent_id, agent_version_id, catalog_release_id, catalog_release_digest, \
             environment, environment_definition_version_id, target_digest, strategy, lifecycle_status, revision, idempotency_key, request_fingerprint, requested_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 1, $13, $14, $15)",
        [
            id.into(),
            request.version.organization_id.into(),
            request.version.project_id.into(),
            request.version.agent_id.into(),
            request.version.id.into(),
            request.version.catalog_release_id.clone().into(),
            request.version.catalog_release_digest.clone().into(),
            request.environment.logical_environment_class.clone().into(),
            request.environment.id.into(),
            request.target_digest.clone().into(),
            request.strategy.clone().into(),
            lifecycle.into(),
            key.trim().into(),
            fingerprint.into(),
            actor.into(),
        ],
    );
    db.execute_raw(statement).await?;
    project_deployment_target(db, id).await
}

pub async fn insert_plan(
    db: &impl ConnectionTrait,
    plan_id: Uuid,
    deployment_id: Uuid,
    actor: Uuid,
    request: &CompiledRequest,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_plan_versions (id, deployment_id, version_number, agent_version_id, catalog_release_id, environment, \
             environment_definition_version_id, agent_content_digest, catalog_release_digest, target_digest, compiler_version, canonical_plan, \
             plan_digest, package_digest, package_reference, created_by) \
         VALUES ($1, $2, 1, $3, $4, $5, $6, $7, $8, $9, $10, $11::jsonb, $12, $13, $14, $15)",
        [
            plan_id.into(),
            deployment_id.into(),
            request.version.id.into(),
            request.version.catalog_release_id.clone().into(),
            request.environment.logical_environment_class.clone().into(),
            request.environment.id.into(),
            request.version.content_digest.clone().into(),
            request.version.catalog_release_digest.clone().into(),
            request.target_digest.clone().into(),
            hive_application::deployment::compiler::COMPILER_VERSION.into(),
            request.canonical_plan.clone().into(),
            request.plan_digest.clone().into(),
            request.package_digest.clone().into(),
            format!("local://packages/{}", request.package_digest).into(),
            actor.into(),
        ],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

/// Marks this transaction's plan before PostgreSQL evaluates the retained-writer compatibility trigger.
pub async fn compiler_review_fact_write(db: &impl ConnectionTrait) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT set_config('hive.m14_compiler_review', 'true', TRUE)",
        [],
    );
    db.query_one_raw(statement).await?;
    Ok(())
}

pub async fn insert_plan_review(
    db: &impl ConnectionTrait,
    plan_id: Uuid,
    review: &hive_application::deployment::CompilerReview,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_plan_review_facts (plan_id, active_agent_version_number, change_summary, \
             requested_dependency_versions, added_dependency_versions, removed_dependency_versions) \
         SELECT $1, $2, $3, $4::jsonb, $5::jsonb, $6::jsonb \
         WHERE NOT EXISTS (SELECT 1 FROM deployment_plan_review_facts WHERE plan_id = $1)",
        [
            plan_id.into(),
            review.active_agent_version_number.into(),
            review.change_summary.clone().into(),
            json_array(&review.requested_dependency_versions).into(),
            json_array(&review.added_dependency_versions).into(),
            json_array(&review.removed_dependency_versions).into(),
        ],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

pub async fn insert_policy_snapshot(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_policy_snapshots (deployment_id, policy_id, policy_revision, policy_digest, policy_matrix, \
             logical_environment_class, risk, required_evidence, required_approvers, agent_version_id, environment_definition_version_id, \
             target_digest, plan_digest, package_digest, binding_digest, risk_verification_digest, evaluation_requirement_expires_at) \
         SELECT $1, $2, $3, $4, $5::jsonb, $6, $7, $8::jsonb, $9, $10, $11, $12, $13, $14, $15, $16, $17 \
         FROM deployments deployment WHERE deployment.id = $1",
        [
            deployment_id.into(),
            request.policy.id.into(),
            request.policy.revision.into(),
            request.policy.digest.clone().into(),
            request.policy.matrix.clone().into(),
            request.environment.logical_environment_class.clone().into(),
            request.risk.clone().into(),
            json_array(&request.rule.evidence).into(),
            request.rule.approvers.into(),
            request.version.id.into(),
            request.environment.id.into(),
            request.target_digest.clone().into(),
            request.plan_digest.clone().into(),
            request.package_digest.clone().into(),
            request.binding_digest.clone().into(),
            request.risk_verification_digest.clone().into(),
            request.evaluation_requirement_expires_at.into(),
        ],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

pub async fn insert_evidence(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<(), DbErr> {
    for kind in &request.rule.evidence {
        // M16 owns evaluation authoring. M14 freezes and validates only an evaluation fact that
        // that owner has already appended for this exact deployment target.
        if kind == "EVALUATION_PASSED" {
            continue;
        }
        let statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO deployment_evidence_snapshots (id, deployment_id, evidence_kind, evidence_digest, expires_at, agent_version_id, \
                 environment_definition_version_id, target_digest, plan_digest, package_digest, binding_digest) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            [
                Uuid::new_v4().into(),
                deployment_id.into(),
                kind.clone().into(),
                hive_application::deployment::compiler::digest(&format!(
                    "{}|{}",
                    request.binding_digest, kind
                ))
                .into(),
                sea_orm::Value::ChronoDateTimeUtc(None),
                request.version.id.into(),
                request.environment.id.into(),
                request.target_digest.clone().into(),
                request.plan_digest.clone().into(),
                request.package_digest.clone().into(),
                request.binding_digest.clone().into(),
            ],
        );
        db.execute_raw(statement).await?;
    }
    Ok(())
}

/// Inserts the one frozen approval requirement for this idempotent deployment cycle.
pub async fn insert_approval_requirement(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<Uuid, DbErr> {
    let insert_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_approval_requirements \
           (id, deployment_id, revision, organization_id, project_id, requested_at, required_approvers, status, expires_at, satisfied_at) \
         SELECT $1, deployment.id, 1, deployment.organization_id, deployment.project_id, deployment.requested_at, \
           $2, 'PENDING', deployment.requested_at + INTERVAL '24 hours', NULL \
         FROM deployments deployment WHERE deployment.id = $3 \
         ON CONFLICT (deployment_id) DO NOTHING",
        [
            Uuid::new_v4().into(),
            request.rule.approvers.into(),
            deployment_id.into(),
        ],
    );
    db.execute_raw(insert_statement).await?;
    let select_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id FROM deployment_approval_requirements WHERE deployment_id = $1",
        [deployment_id.into()],
    );
    db.query_one_raw(select_statement)
        .await?
        .ok_or_else(|| {
            DbErr::RecordNotFound(format!(
                "no deployment_approval_requirements row for deployment {deployment_id}"
            ))
        })?
        .try_get_by("id")
}

/// Terminalizes every still-`QUEUED`/`RUNNING` attempt for a deployment being force-terminated
/// (cancel, or the worker's dead-letter path), staging a matching timeline event for each.
pub async fn terminalize_running_attempts(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    status: &str,
    code: &str,
    summary: &str,
    stage_name: &str,
    stage_status: &str,
) -> Result<Vec<Uuid>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployment_attempts SET status = $1, completed_at = CURRENT_TIMESTAMP, generation = generation + 1, \
             failure_code = $2, failure_summary = $3 WHERE deployment_id = $4 AND status IN ('QUEUED', 'RUNNING') RETURNING id",
        [
            status.into(),
            code.into(),
            summary.into(),
            deployment_id.into(),
        ],
    );
    let rows = db.query_all_raw(statement).await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows {
        ids.push(row.try_get_by::<Uuid, _>("id")?);
    }
    for attempt_id in &ids {
        stage(db, *attempt_id, stage_name, stage_status, summary).await?;
    }
    Ok(ids)
}
