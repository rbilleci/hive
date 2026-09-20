//! Ports the read-only `DeploymentRepository` methods: compile-context
//! resolution (Group B), list/find/timeline/detail/environments (Group C),
//! and the approval inbox/decision/requirement surface (Group D) — the
//! GraphQL fields for the latter are not wired in this phase (see
//! `hive-persistence::deployment`'s module doc comment), but the repository
//! methods themselves are implemented in full since the trait requires them
//! and every Java source needed was already read while scoping this phase.

use super::cursors::{self, ApprovalCursor, SqlValue};
use super::rows::{self, ApprovalDecisionRow, RawRequirement};
use crate::capability::tx;
use hive_application::deployment::{
    ActiveTarget, ApprovalDecision, ApprovalDecisionConnection, ApprovalDecisionMutationResult,
    ApprovalDecisionPlanner, ApprovalDecisionPreview, ApprovalInboxConnection, ApprovalInboxItem,
    ApprovalPrincipal, ApprovalRequirement, ApprovalRule, ApprovalSnapshot, ApprovalTarget,
    Deployment, DeploymentCompilationContext, DeploymentConnection, DeploymentDetailProjection,
    DeploymentEnvironmentConnection, DeploymentEvidence, DeploymentFilter,
    DeploymentRecoveryCompilationContext, DeploymentTimelineConnection, EnvironmentDefinition,
    PolicySource, VersionSource,
};
use hive_domain::deployment::{
    ApprovalDecisionCommand, ApprovalDecisionFacts,
    ApprovalDecisionProblem as DomainApprovalDecisionProblem,
};
use hive_domain::deployment::{ApprovalRequirementStatus, DeploymentLifecycleStatus};
use sqlx::{PgConnection, PgPool, Row};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

async fn can_view(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    tx::has_deployment_capability(conn, principal_id, tx::DEPLOYMENT_VIEW, project_id, lock).await
}

async fn can_approval_view(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    tx::has_deployment_capability(
        conn,
        principal_id,
        tx::DEPLOYMENT_APPROVAL_VIEW,
        project_id,
        lock,
    )
    .await
}

async fn visible_deployment(
    conn: &mut PgConnection,
    principal_id: Uuid,
    deployment_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let predicate =
        crate::capability::deployment_view_predicate(principal_id, "deployments.project_id");
    let renumbered = cursors::renumber(&predicate.sql, 1);
    let sql = format!("SELECT 1 FROM deployments WHERE id = $1 AND {renumbered}");
    let mut query = sqlx::query(&sql).bind(deployment_id);
    for value in &predicate.values {
        query = query.bind(value);
    }
    Ok(query.fetch_optional(&mut *conn).await?.is_some())
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

pub async fn compilation_context(
    conn: &mut PgConnection,
    principal_id: Uuid,
    version_id: Uuid,
    environment_id: Uuid,
) -> Result<Option<DeploymentCompilationContext>, sqlx::Error> {
    let Some(version) = version_source(conn, version_id, false).await? else {
        return Ok(None);
    };
    if !can_view(conn, principal_id, version.project_id, false).await? {
        return Ok(None);
    }
    let environment_value = environment(conn, environment_id, &version.catalog_release_id).await?;
    let policy_value = policy(conn, version.project_id, false).await?;
    let current_target = active_target(
        conn,
        version.project_id,
        version.agent_id,
        environment_id,
        false,
    )
    .await?;
    match (environment_value, policy_value) {
        (Some(environment_value), Some(policy_value)) => Ok(Some(DeploymentCompilationContext {
            version,
            environment: environment_value,
            policy: policy_value,
            current_target,
        })),
        _ => Ok(None),
    }
}

fn canonical_target_version(value: Option<&str>) -> Option<String> {
    let value = value?;
    match Uuid::parse_str(value) {
        Ok(id) => Some(id.to_string()),
        Err(_) => Some(value.trim().to_string()),
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
    version_id: Option<Uuid>,
    lock: bool,
) -> Result<Option<DeploymentRecoveryCompilationContext>, sqlx::Error> {
    let Some(version_id) = version_id else {
        return Ok(None);
    };
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
        (Some(environment_value), Some(policy_value)) => {
            Ok(Some(DeploymentRecoveryCompilationContext {
                version,
                environment: environment_value,
                policy: policy_value,
                current_target,
                strategy: source.strategy.clone(),
            }))
        }
        _ => Ok(None),
    }
}

pub async fn recovery_compilation_context(
    conn: &mut PgConnection,
    principal_id: Uuid,
    deployment_id: Uuid,
    target_agent_version_id: Option<&str>,
    retry: bool,
) -> Result<Option<DeploymentRecoveryCompilationContext>, sqlx::Error> {
    let Some(source) = rows::deployments(conn, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    if !can_view(conn, principal_id, source.project_id, false).await? {
        return Ok(None);
    }
    let capability = if retry {
        tx::DEPLOYMENT_RETRY
    } else {
        tx::DEPLOYMENT_ROLLBACK
    };
    if !tx::has_deployment_capability(conn, principal_id, capability, source.project_id, false)
        .await?
    {
        return Ok(None);
    }
    let version_id = if retry {
        Some(source.agent_version_id)
    } else {
        let canonical = canonical_target_version(target_agent_version_id);
        rollback_target_version(conn, &source, canonical.as_deref()).await?
    };
    recovery_compilation_inputs(conn, &source, version_id, false).await
}

fn bind_sql_value<'q>(
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    value: &'q SqlValue,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match value {
        SqlValue::Uuid(id) => query.bind(id),
        SqlValue::Text(text) => query.bind(text),
        SqlValue::DateTime(value) => query.bind(value),
    }
}

pub async fn list(
    conn: &mut PgConnection,
    principal_id: Uuid,
    filter: &DeploymentFilter,
    after: Option<&str>,
    first: i32,
) -> Result<Option<DeploymentConnection>, sqlx::Error> {
    let Some(project_id) = filter.project_id else {
        return Ok(None);
    };
    if !can_view(conn, principal_id, project_id, false).await? {
        return Ok(None);
    }
    let filter_key = cursors::canonical_filter(filter);
    let cursor = match cursors::decode_list_cursor(after, &filter_key) {
        Ok(cursor) => cursor,
        Err(()) => return Ok(None),
    };
    let visibility =
        crate::capability::deployment_view_predicate(principal_id, "deployments.project_id");
    let where_clause =
        cursors::deployment_where(filter, &cursor, &visibility.sql, visibility.values);
    let sql = format!(
        "SELECT id FROM deployments {} ORDER BY requested_at DESC, id DESC LIMIT ${}",
        where_clause.sql,
        where_clause.values.len() + 1
    );
    let mut query = sqlx::query(&sql);
    for value in &where_clause.values {
        query = bind_sql_value(query, value);
    }
    query = query.bind(first + 1);
    let rows_found = query.fetch_all(&mut *conn).await?;
    let mut ids: Vec<Uuid> = rows_found.iter().map(|row| row.get(0)).collect();
    let has_next = ids.len() > first as usize;
    if has_next {
        ids.pop();
    }
    let values = rows::deployments(conn, &ids, true).await?;
    let cursors_list: Vec<String> = values
        .iter()
        .map(|value| cursors::encode_list_cursor(&filter_key, value.requested_at, value.id))
        .collect();
    let end_cursor = cursors_list.last().cloned();
    Ok(Some(DeploymentConnection {
        nodes: values,
        cursors: cursors_list,
        end_cursor,
        has_next_page: has_next,
    }))
}

pub async fn find(
    conn: &mut PgConnection,
    principal_id: Uuid,
    deployment_id: Uuid,
) -> Result<Option<Deployment>, sqlx::Error> {
    if !visible_deployment(conn, principal_id, deployment_id).await? {
        return Ok(None);
    }
    Ok(rows::deployments(conn, &[deployment_id], true)
        .await?
        .into_iter()
        .next())
}

fn unified_timeline_sql() -> &'static str {
    "WITH unified AS ( \
      SELECT audit.id, audit.deployment_attempt_id AS attempt_id, audit.attempt_number, audit.timeline_sequence AS sequence_number, \
        audit.action AS stage, \
        CASE WHEN audit.action IN ('CANCELED', 'APPROVAL_REJECTED', 'APPROVAL_EXECUTION_BLOCKED') THEN 'CANCELED' \
          WHEN audit.action IN ('EXECUTION_FAILED', 'OUTBOX_DEAD_LETTERED', 'APPROVAL_EXPIRED', 'APPROVAL_INVALIDATED') THEN 'FAILED' \
          WHEN audit.action IN ('APPROVAL_REPLAYED', 'OUTBOX_DELIVERY_RETRIED', 'RETRY_RECORDED', 'PROMOTION_RECORDED', 'ROLLBACK_RECORDED') THEN 'RECORDED' \
          ELSE 'SUCCEEDED' END AS status, \
        CASE audit.action WHEN 'REQUESTED' THEN 'A user recorded this deployment request.' WHEN 'CANCELED' THEN 'A user canceled this deployment.' \
          WHEN 'OUTBOX_DEAD_LETTERED' THEN 'The local worker isolated an event.' WHEN 'OUTBOX_LEASE_RECLAIMED' THEN 'The local worker reclaimed an expired lease.' \
          WHEN 'OUTBOX_DELIVERY_RETRIED' THEN 'The local worker recorded a bounded database-delivery retry.' \
          WHEN 'EXECUTION_STARTED' THEN 'The local worker started execution.' WHEN 'EXECUTION_FAILED' THEN 'The local worker recorded a sanitized failure.' \
          WHEN 'APPROVAL_RECORDED' THEN 'An eligible approver recorded an immutable decision.' \
          WHEN 'APPROVAL_REPLAYED' THEN 'The service returned the actor''s immutable decision for this request.' \
          WHEN 'APPROVAL_SATISFIED' THEN 'The frozen approval requirement was satisfied.' \
          WHEN 'APPROVAL_REJECTED' THEN 'An eligible approver rejected this deployment request.' \
          WHEN 'APPROVAL_EXPIRED' THEN 'The approval requirement expired before satisfaction.' \
          WHEN 'APPROVAL_INVALIDATED' THEN 'The approval requirement no longer matched its frozen evidence.' \
          WHEN 'APPROVAL_EXECUTION_BLOCKED' THEN 'The local worker stopped execution because frozen approval requirements were no longer executable.' \
          WHEN 'RETRY_RECORDED' THEN 'An authorized operator recorded a new recovery deployment cycle.' \
          WHEN 'PROMOTION_RECORDED' THEN 'An authorized operator recorded a promotion for the observed healthy target.' \
          WHEN 'ROLLBACK_RECORDED' THEN 'An authorized operator recorded a rollback deployment cycle.' \
          ELSE 'The local worker recorded successful execution.' END AS message, \
        CASE WHEN audit.action LIKE 'APPROVAL_%' THEN 'SERVICE' WHEN audit.actor_principal_id IS NULL THEN 'WORKER' ELSE 'USER' END AS source, audit.occurred_at \
      FROM deployment_audit_events audit WHERE audit.deployment_id = $1 \
      UNION ALL \
      SELECT event.id, attempt.id, attempt.attempt_number, event.timeline_sequence AS sequence_number, event.stage, event.status, event.message, \
        'WORKER' AS source, event.occurred_at \
      FROM deployment_stage_events event JOIN deployment_attempts attempt ON attempt.id = event.deployment_attempt_id \
      WHERE attempt.deployment_id = $2 \
    ) SELECT * FROM unified"
}

async fn timeline(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<DeploymentTimelineConnection, sqlx::Error> {
    let cursor = cursors::decode_timeline_cursor(after).unwrap_or(None);
    let relation = if cursor.is_some() {
        " WHERE (attempt_number, sequence_number, id) > ($3, $4, $5)"
    } else {
        ""
    };
    let limit_index = if cursor.is_some() { 6 } else { 3 };
    let sql = format!("{} {relation} ORDER BY attempt_number ASC, sequence_number ASC, id ASC LIMIT ${limit_index}", unified_timeline_sql());
    let mut query = sqlx::query(&sql).bind(deployment_id).bind(deployment_id);
    if let Some(cursor) = &cursor {
        query = query
            .bind(cursor.attempt_number)
            .bind(cursor.sequence)
            .bind(cursor.id);
    }
    query = query.bind(first + 1);
    let result = query.fetch_all(&mut *conn).await?;
    let mut values: Vec<_> = result.iter().map(rows::timeline_row).collect();
    let has_next = values.len() > first as usize;
    if has_next {
        values.pop();
    }
    let cursors_list: Vec<String> = values
        .iter()
        .map(|value| {
            cursors::encode_timeline_cursor(value.attempt_number, value.sequence, value.id)
        })
        .collect();
    let end_cursor = cursors_list.last().cloned();
    Ok(DeploymentTimelineConnection {
        nodes: values,
        cursors: cursors_list,
        end_cursor,
        has_next_page: has_next,
    })
}

pub async fn timeline_page(
    conn: &mut PgConnection,
    principal_id: Uuid,
    deployment_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<DeploymentTimelineConnection>, sqlx::Error> {
    if !visible_deployment(conn, principal_id, deployment_id).await? {
        return Ok(None);
    }
    if after.is_some()
        && cursors::decode_timeline_cursor(after)
            .unwrap_or(None)
            .is_none()
    {
        return Ok(None);
    }
    Ok(Some(timeline(conn, deployment_id, after, first).await?))
}

pub async fn detail(
    conn: &mut PgConnection,
    principal_id: Uuid,
    deployment_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<DeploymentDetailProjection>, sqlx::Error> {
    if !visible_deployment(conn, principal_id, deployment_id).await? {
        return Ok(None);
    }
    if after.is_some()
        && cursors::decode_timeline_cursor(after)
            .unwrap_or(None)
            .is_none()
    {
        return Ok(None);
    }
    let Some(deployment) = rows::deployments(conn, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let timeline_value = timeline(conn, deployment_id, after, first).await?;
    Ok(Some(DeploymentDetailProjection {
        deployment,
        timeline: timeline_value,
    }))
}

pub async fn environments(
    conn: &mut PgConnection,
    principal_id: Uuid,
    version_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<DeploymentEnvironmentConnection>, sqlx::Error> {
    let Some(version) = version_source(conn, version_id, false).await? else {
        return Ok(None);
    };
    if !can_view(conn, principal_id, version.project_id, false).await? {
        return Ok(None);
    }
    let boundary = match cursors::decode_environment_cursor(after) {
        Ok(boundary) => boundary,
        Err(()) => return Ok(None),
    };
    let rows_found = sqlx::query(
        "SELECT id, stable_definition_id, version, display_name, logical_environment_class, catalog_release_id, catalog_release_digest, content_digest \
         FROM environment_definition_versions \
         WHERE catalog_release_id = $1 AND ($2::text IS NULL OR (stable_definition_id, version, id) > ($2, $3, $4)) \
         ORDER BY stable_definition_id ASC, version ASC, id ASC LIMIT $5",
    )
    .bind(&version.catalog_release_id)
    .bind(boundary.as_ref().map(|value| value.stable_definition_id.clone()))
    .bind(boundary.as_ref().map(|value| value.version.clone()))
    .bind(boundary.as_ref().map(|value| value.id))
    .bind(first + 1)
    .fetch_all(&mut *conn)
    .await?;
    let mut values: Vec<_> = rows_found.iter().map(rows::environment_row).collect();
    let has_next = values.len() > first as usize;
    if has_next {
        values.pop();
    }
    let cursors_list: Vec<String> = values
        .iter()
        .map(|value| {
            cursors::encode_environment_cursor(
                &value.stable_definition_id,
                &value.version,
                &value.id,
            )
        })
        .collect();
    let end_cursor = cursors_list.last().cloned();
    Ok(Some(DeploymentEnvironmentConnection {
        nodes: values,
        cursors: cursors_list,
        end_cursor,
        has_next_page: has_next,
    }))
}

// --- approval inbox / detail / decisions / recordApprovalDecision ---

async fn approval_evidence_for(
    conn: &mut PgConnection,
    deployment_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<DeploymentEvidence>>, sqlx::Error> {
    let mut values: HashMap<Uuid, Vec<DeploymentEvidence>> =
        deployment_ids.iter().map(|id| (*id, Vec::new())).collect();
    if deployment_ids.is_empty() {
        return Ok(values);
    }
    let rows_found = sqlx::query(
        "SELECT policy.deployment_id, required.evidence_kind, evidence.evidence_digest, evidence.binding_digest, evidence.expires_at, \
           CASE WHEN evidence.id IS NULL THEN 'MISSING' \
                WHEN EXISTS (SELECT 1 FROM deployment_evidence_invalidations invalidation \
                             WHERE invalidation.evidence_snapshot_id = evidence.id AND invalidation.kind = 'FAILED') THEN 'FAILED' \
                WHEN EXISTS (SELECT 1 FROM deployment_evidence_invalidations invalidation \
                             WHERE invalidation.evidence_snapshot_id = evidence.id AND invalidation.kind = 'REVOKED') THEN 'REVOKED' \
                WHEN evidence.expires_at <= clock_timestamp() THEN 'EXPIRED' \
                WHEN evidence.binding_digest = policy.binding_digest AND evidence.agent_version_id = policy.agent_version_id \
                  AND evidence.environment_definition_version_id = policy.environment_definition_version_id \
                  AND evidence.target_digest = policy.target_digest AND evidence.plan_digest = policy.plan_digest \
                  AND evidence.package_digest = policy.package_digest THEN 'VALID' \
                ELSE 'MISMATCH' END AS evidence_state \
         FROM deployment_policy_snapshots policy \
         JOIN deployments deployment ON deployment.id = policy.deployment_id \
         CROSS JOIN LATERAL jsonb_array_elements_text(policy.required_evidence) required(evidence_kind) \
         LEFT JOIN deployment_evidence_snapshots evidence ON evidence.deployment_id = deployment.id \
           AND evidence.evidence_kind = required.evidence_kind \
         WHERE policy.deployment_id = ANY($1) \
         ORDER BY policy.deployment_id, required.evidence_kind",
    )
    .bind(deployment_ids)
    .fetch_all(&mut *conn)
    .await?;
    for row in rows_found {
        let deployment_id: Uuid = row.get("deployment_id");
        let evidence = DeploymentEvidence {
            kind: row.get("evidence_kind"),
            digest: row.get("evidence_digest"),
            binding_digest: row.get("binding_digest"),
            expires_at: row.get("expires_at"),
            state: row.get("evidence_state"),
        };
        values.entry(deployment_id).or_default().push(evidence);
    }
    Ok(values)
}

async fn approval_principals_for(
    conn: &mut PgConnection,
    requirements: &[RawRequirement],
) -> Result<HashMap<Uuid, ApprovalPrincipal>, sqlx::Error> {
    let mut ids: Vec<Uuid> = Vec::new();
    for requirement in requirements {
        if !ids.contains(&requirement.requester_id) {
            ids.push(requirement.requester_id);
        }
        for participant in &requirement.satisfied_participants {
            if !ids.contains(participant) {
                ids.push(*participant);
            }
        }
    }
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows_found = sqlx::query("SELECT id, subject FROM principals WHERE id = ANY($1)")
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows_found
        .into_iter()
        .map(|row| {
            (
                row.get::<Uuid, _>(0),
                ApprovalPrincipal {
                    id: row.get(0),
                    subject: row.get(1),
                },
            )
        })
        .collect())
}

async fn approval_decision_previews(
    conn: &mut PgConnection,
    requirement_ids: &[Uuid],
) -> Result<HashMap<Uuid, ApprovalDecisionPreview>, sqlx::Error> {
    if requirement_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut by_requirement: HashMap<Uuid, Vec<ApprovalDecisionRow>> =
        requirement_ids.iter().map(|id| (*id, Vec::new())).collect();
    let rows_found = sqlx::query(
        "SELECT decision.id, decision.approval_requirement_id, decision.actor_principal_id, decision.decision, \
           decision.comment, decision.rejection_reason, decision.eligibility_checked_at, decision.decided_at \
         FROM unnest($1::uuid[]) requirement(id) \
         CROSS JOIN LATERAL ( \
           SELECT id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, decided_at \
           FROM deployment_approval_decisions WHERE approval_requirement_id = requirement.id \
           ORDER BY decided_at ASC, id ASC LIMIT 51 \
         ) decision \
         ORDER BY decision.approval_requirement_id ASC, decision.decided_at ASC, decision.id ASC",
    )
    .bind(requirement_ids)
    .fetch_all(&mut *conn)
    .await?;
    for row in rows_found {
        let decision = rows::decision_row(&row);
        by_requirement
            .entry(decision.requirement_id)
            .or_default()
            .push(decision);
    }
    Ok(by_requirement
        .into_iter()
        .map(|(requirement_id, decisions)| {
            let cursors_list: Vec<String> = decisions
                .iter()
                .map(|decision| {
                    cursors::encode_approval_decision_cursor(
                        requirement_id,
                        decision.decided_at,
                        decision.id,
                    )
                })
                .collect();
            let nodes = decisions
                .into_iter()
                .map(approval_decision_from_row)
                .collect();
            (
                requirement_id,
                ApprovalDecisionPreview {
                    nodes,
                    cursors: cursors_list,
                },
            )
        })
        .collect())
}

async fn approval_decision_requirement_ids(
    conn: &mut PgConnection,
    requirement_ids: &[Uuid],
    principal_id: Uuid,
) -> Result<HashSet<Uuid>, sqlx::Error> {
    if requirement_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let rows_found: Vec<(Uuid,)> =
        sqlx::query_as("SELECT DISTINCT approval_requirement_id FROM deployment_approval_decisions WHERE actor_principal_id = $1 AND approval_requirement_id = ANY($2)")
            .bind(principal_id)
            .bind(requirement_ids)
            .fetch_all(&mut *conn)
            .await?;
    Ok(rows_found.into_iter().map(|(id,)| id).collect())
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

async fn eligible_approver(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    Ok(tx::has_deployment_capability(
        conn,
        principal_id,
        tx::DEPLOYMENT_APPROVAL_DECIDE,
        project_id,
        lock,
    )
    .await?
        && active_project_check(conn, project_id, lock).await?)
}

async fn eligible_approver_cached(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
    cache: &mut HashMap<(Uuid, Uuid), bool>,
) -> Result<bool, sqlx::Error> {
    let key = (principal_id, project_id);
    if let Some(value) = cache.get(&key) {
        return Ok(*value);
    }
    let value = eligible_approver(conn, principal_id, project_id, false).await?;
    cache.insert(key, value);
    Ok(value)
}

/// Acquires every current and prospective approver authority lock in one stable principal order.
async fn lock_approval_authorities(
    conn: &mut PgConnection,
    requirement_id: Uuid,
    prospective_actor: Uuid,
    project_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut actors: Vec<(Uuid,)> = sqlx::query_as("SELECT actor_principal_id FROM deployment_approval_decisions WHERE approval_requirement_id = $1 AND decision = 'APPROVE' ORDER BY actor_principal_id")
        .bind(requirement_id)
        .fetch_all(&mut *conn)
        .await?;
    let mut actor_ids: std::collections::BTreeSet<Uuid> =
        actors.drain(..).map(|(id,)| id).collect();
    actor_ids.insert(prospective_actor);
    for actor in actor_ids {
        tx::deployment_approval_capabilities(conn, actor, project_id, true).await?;
    }
    Ok(())
}

async fn qualifying_approvers(
    conn: &mut PgConnection,
    requirement_id: Uuid,
    project_id: Uuid,
    requester_id: Uuid,
    lock: bool,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let actors: Vec<(Uuid,)> = sqlx::query_as("SELECT actor_principal_id FROM deployment_approval_decisions WHERE approval_requirement_id = $1 AND decision = 'APPROVE' ORDER BY actor_principal_id")
        .bind(requirement_id)
        .fetch_all(&mut *conn)
        .await?;
    let mut qualified = Vec::new();
    for (actor,) in actors {
        if actor != requester_id && eligible_approver(conn, actor, project_id, lock).await? {
            qualified.push(actor);
        }
    }
    Ok(qualified)
}

async fn qualifying_approval_count(
    conn: &mut PgConnection,
    requirement: &RawRequirement,
) -> Result<i32, sqlx::Error> {
    if requirement.status == ApprovalRequirementStatus::Satisfied {
        return Ok(requirement.satisfied_participants.len() as i32);
    }
    Ok(
        qualifying_approval_counts(conn, std::slice::from_ref(requirement))
            .await?
            .get(&requirement.id)
            .copied()
            .unwrap_or(0),
    )
}

async fn qualifying_approval_counts(
    conn: &mut PgConnection,
    requirements: &[RawRequirement],
) -> Result<HashMap<Uuid, i32>, sqlx::Error> {
    if requirements.is_empty() {
        return Ok(HashMap::new());
    }
    let mut counts = HashMap::new();
    let mut pending = Vec::new();
    for requirement in requirements {
        if requirement.status == ApprovalRequirementStatus::Satisfied {
            counts.insert(
                requirement.id,
                requirement.satisfied_participants.len() as i32,
            );
        } else {
            pending.push(requirement.id);
            counts.insert(requirement.id, 0);
        }
    }
    if pending.is_empty() {
        return Ok(counts);
    }
    let rows_found: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT DISTINCT requirement.id, requirement.project_id, decision.actor_principal_id \
         FROM deployment_approval_requirements requirement \
         JOIN projects project ON project.id = requirement.project_id AND project.lifecycle_status = 'ACTIVE' \
         JOIN deployments deployment ON deployment.id = requirement.deployment_id \
         JOIN deployment_approval_decisions decision ON decision.approval_requirement_id = requirement.id \
           AND decision.decision = 'APPROVE' AND decision.actor_principal_id <> deployment.requested_by \
         WHERE requirement.id = ANY($1)",
    )
    .bind(&pending)
    .fetch_all(&mut *conn)
    .await?;
    let mut qualified_cache: HashMap<(Uuid, Uuid), bool> = HashMap::new();
    let mut qualifying_counts: HashMap<Uuid, i32> = HashMap::new();
    for (requirement_id, project_id, actor_id) in rows_found {
        let key = (actor_id, project_id);
        let qualified = match qualified_cache.get(&key) {
            Some(value) => *value,
            None => {
                let value = tx::deployment_approval_capabilities(conn, actor_id, project_id, false)
                    .await?
                    .contains(tx::DEPLOYMENT_APPROVAL_DECIDE);
                qualified_cache.insert(key, value);
                value
            }
        };
        if qualified {
            *qualifying_counts.entry(requirement_id).or_insert(0) += 1;
        }
    }
    for id in pending {
        counts.insert(id, *qualifying_counts.get(&id).unwrap_or(&0));
    }
    Ok(counts)
}

async fn has_decision(
    conn: &mut PgConnection,
    requirement_id: Uuid,
    actor: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("SELECT 1 FROM deployment_approval_decisions WHERE approval_requirement_id = $1 AND actor_principal_id = $2").bind(requirement_id).bind(actor).fetch_optional(&mut *conn).await?.is_some())
}

#[allow(clippy::too_many_arguments)]
async fn insert_decision(
    conn: &mut PgConnection,
    id: Uuid,
    requirement_id: Uuid,
    actor: Uuid,
    value: &str,
    comment: Option<&str>,
    rejection_reason: Option<&str>,
    request_id: Uuid,
    correlation_id: Uuid,
    expected_revision: i64,
    request_fingerprint: &str,
) -> Result<(), sqlx::Error> {
    let blank = |value: Option<&str>| value.map(str::trim).unwrap_or("").is_empty();
    sqlx::query(
        "INSERT INTO deployment_approval_decisions (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, request_key, correlation_id, eligibility_checked_at, request_expected_revision, request_fingerprint) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, CURRENT_TIMESTAMP, $9, $10)",
    )
    .bind(id)
    .bind(requirement_id)
    .bind(actor)
    .bind(value)
    .bind(if value == "REJECT" || blank(comment) { None } else { comment.map(str::trim) })
    .bind(if value == "REJECT" { rejection_reason.map(str::trim) } else { None })
    .bind(request_id)
    .bind(correlation_id)
    .bind(expected_revision)
    .bind(request_fingerprint)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Hashes the normalized command fields that a retry key owns without exposing review text in storage.
fn decision_request_fingerprint(command: &ApprovalDecisionCommand) -> String {
    let canonical = format!(
        "{}\u{0}{}\u{0}{}\u{0}{}",
        command.value,
        command.expected_revision,
        command.comment.as_deref().unwrap_or("\u{1}"),
        command.rejection_reason.as_deref().unwrap_or("\u{1}")
    );
    hive_application::deployment::compiler::digest(&canonical)
}

fn requirement_state_problem(raw: &RawRequirement) -> DomainApprovalDecisionProblem {
    if raw.status == ApprovalRequirementStatus::Expired {
        return DomainApprovalDecisionProblem::of("APPROVAL_REQUIREMENT_EXPIRED");
    }
    if raw.status == ApprovalRequirementStatus::Invalidated {
        if let Some(code) = &raw.invalidation_code {
            if matches!(
                code.as_str(),
                "APPROVAL_EVIDENCE_MISSING"
                    | "APPROVAL_EVIDENCE_EXPIRED"
                    | "APPROVAL_EVIDENCE_MISMATCH"
            ) {
                return DomainApprovalDecisionProblem::of(code.clone());
            }
        }
    }
    DomainApprovalDecisionProblem::of("APPROVAL_REQUIREMENT_NOT_PENDING")
}

fn decision_refusal(problem: DomainApprovalDecisionProblem) -> ApprovalDecisionMutationResult {
    ApprovalDecisionMutationResult::refused(problem.into())
}

fn approval_decision_from_row(row: ApprovalDecisionRow) -> ApprovalDecision {
    ApprovalDecision {
        id: row.id,
        requirement_id: row.requirement_id,
        actor_principal_id: row.actor_principal_id,
        value: row.value,
        comment: row.comment,
        rejection_reason: row.rejection_reason,
        eligibility_checked_at: row.eligibility_checked_at,
        decided_at: row.decided_at,
    }
}

fn requirement_from_raw(
    raw: &RawRequirement,
    deployment: &Deployment,
    qualifying: i32,
    evidence: Vec<DeploymentEvidence>,
    principals: &HashMap<Uuid, ApprovalPrincipal>,
    decision_preview: Option<ApprovalDecisionPreview>,
) -> ApprovalRequirement {
    let status = raw.status;
    let participants = raw.satisfied_participants.clone();
    let qualifying = if status == ApprovalRequirementStatus::Satisfied {
        participants.len() as i32
    } else {
        qualifying
    };
    let target = ApprovalTarget {
        agent_version_id: deployment.plan.agent_version_id,
        agent_version_digest: deployment.plan.agent_content_digest.clone(),
        environment_definition_version_id: deployment.plan.environment_definition_version_id,
        environment_definition_digest: deployment.environment.content_digest.clone(),
        target_digest: deployment.plan.target_digest.clone(),
        deployment_plan_digest: deployment.plan.plan_digest.clone(),
        artifact_digest: deployment.plan.package_digest.clone(),
    };
    let rule = ApprovalRule {
        required_evidence: deployment.policy.required_evidence.clone(),
        required_distinct_approver_count: raw.required_approvers,
    };
    let snapshot = ApprovalSnapshot {
        policy_digest: deployment.policy.policy_digest.clone(),
        policy_revision: deployment.policy.policy_revision,
        environment_class: deployment.policy.logical_environment_class.clone(),
        risk: deployment.policy.risk.clone(),
        rule,
        target,
        evidence,
        expires_at: raw.expires_at.unwrap_or(raw.requested_at),
    };
    ApprovalRequirement {
        id: raw.id,
        deployment_id: raw.deployment_id,
        project_id: raw.project_id,
        revision: raw.revision,
        status,
        expires_at: raw.expires_at,
        satisfied_at: raw.satisfied_at,
        rejected_at: raw.rejected_at,
        invalidated_at: raw.invalidated_at,
        requester_id: raw.requester_id,
        requester: principals.get(&raw.requester_id).cloned(),
        required_distinct_approver_count: raw.required_approvers,
        qualifying_approval_count: qualifying,
        satisfied_participants: participants.clone(),
        satisfied_participant_details: participants
            .iter()
            .filter_map(|id| principals.get(id).cloned())
            .collect(),
        approval_snapshot: snapshot,
        decision_preview,
    }
}

#[allow(clippy::too_many_arguments)]
async fn approval_item(
    conn: &mut PgConnection,
    principal_id: Uuid,
    raw: &RawRequirement,
    deployment: &Deployment,
    qualifying: i32,
    eligibility: &mut HashMap<(Uuid, Uuid), bool>,
    evidence: Vec<DeploymentEvidence>,
    principals: &HashMap<Uuid, ApprovalPrincipal>,
    decision_preview: Option<ApprovalDecisionPreview>,
    prior_decision: Option<bool>,
) -> Result<ApprovalInboxItem, sqlx::Error> {
    let requirement = requirement_from_raw(
        raw,
        deployment,
        qualifying,
        evidence,
        principals,
        decision_preview,
    );
    let eligible =
        eligible_approver_cached(conn, principal_id, raw.project_id, eligibility).await?;
    let prior_decision = match prior_decision {
        Some(value) => value,
        None => has_decision(conn, raw.id, principal_id).await?,
    };
    let available = eligible
        && requirement.required_distinct_approver_count > 0
        && requirement.status == ApprovalRequirementStatus::Pending
        && deployment.lifecycle_status == DeploymentLifecycleStatus::AwaitingApproval
        && principal_id != requirement.requester_id
        && !prior_decision;
    Ok(ApprovalInboxItem {
        requirement,
        deployment: deployment.clone(),
        eligible,
        decision_available: available,
    })
}

async fn approval_item_for_requirement(
    conn: &mut PgConnection,
    principal_id: Uuid,
    raw: &RawRequirement,
) -> Result<ApprovalInboxItem, sqlx::Error> {
    let Some(deployment) = rows::deployments(conn, &[raw.deployment_id], false)
        .await?
        .into_iter()
        .next()
    else {
        return Err(sqlx::Error::RowNotFound);
    };
    let qualifying = qualifying_approval_count(conn, raw).await?;
    let evidence = approval_evidence_for(conn, &[raw.deployment_id])
        .await?
        .remove(&raw.deployment_id)
        .unwrap_or_default();
    let principals = approval_principals_for(conn, std::slice::from_ref(raw)).await?;
    let mut eligibility = HashMap::new();
    approval_item(
        conn,
        principal_id,
        raw,
        &deployment,
        qualifying,
        &mut eligibility,
        evidence,
        &principals,
        None,
        None,
    )
    .await
}

async fn requirement_for(
    conn: &mut PgConnection,
    raw: &RawRequirement,
) -> Result<ApprovalRequirement, sqlx::Error> {
    let Some(deployment) = rows::deployments(conn, &[raw.deployment_id], true)
        .await?
        .into_iter()
        .next()
    else {
        return Err(sqlx::Error::RowNotFound);
    };
    let qualifying = qualifying_approval_count(conn, raw).await?;
    let evidence = approval_evidence_for(conn, &[raw.deployment_id])
        .await?
        .remove(&raw.deployment_id)
        .unwrap_or_default();
    let principals = approval_principals_for(conn, std::slice::from_ref(raw)).await?;
    Ok(requirement_from_raw(
        raw,
        &deployment,
        qualifying,
        evidence,
        &principals,
        None,
    ))
}

async fn has_approval_inbox_scope(
    conn: &mut PgConnection,
    principal_id: Uuid,
    organization_id: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    if let Some(organization_id) = organization_id {
        if sqlx::query("SELECT 1 FROM organizations WHERE id = $1")
            .bind(organization_id)
            .fetch_optional(&mut *conn)
            .await?
            .is_none()
        {
            return Ok(false);
        }
    }
    let administrator = tx::has_platform_admin_read(conn, principal_id).await?;
    let candidates: Vec<(Uuid,)> = sqlx::query_as(
        "WITH params AS (SELECT $1::uuid AS authority_principal, $2::uuid AS scoped_organization, $3 AS administrator), \
           candidate_projects AS ( \
             (SELECT scope.project_id \
             FROM deployment_approval_principal_project_scopes scope, params \
             WHERE scope.principal_id = params.authority_principal AND scope.valid_after <= CURRENT_TIMESTAMP \
             ORDER BY scope.project_id ASC LIMIT 1) \
             UNION ALL \
             (SELECT membership.project_id \
             FROM project_memberships membership \
             JOIN project_membership_roles role ON role.membership_id = membership.id \
             JOIN organization_memberships organization_membership \
               ON organization_membership.principal_id = membership.principal_id \
             JOIN projects project \
               ON project.id = membership.project_id AND project.organization_id = organization_membership.organization_id \
             , params \
             WHERE membership.principal_id = params.authority_principal \
               AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
               AND organization_membership.started_at <= CURRENT_TIMESTAMP AND organization_membership.ended_at IS NULL \
               AND role.role_code IN ('PROJECT_ADMIN', 'DEPLOYMENT_APPROVER', 'AUDITOR') \
               AND (params.scoped_organization IS NULL OR project.organization_id = params.scoped_organization) \
             ORDER BY membership.project_id ASC LIMIT 1) \
             UNION ALL \
             (SELECT project.id \
             FROM deployment_approval_principal_organization_scopes scope, params \
             CROSS JOIN LATERAL ( \
               SELECT candidate.id FROM projects candidate \
               WHERE candidate.organization_id = scope.organization_id ORDER BY candidate.id ASC LIMIT 1 \
             ) project \
             WHERE scope.principal_id = params.authority_principal AND scope.valid_after <= CURRENT_TIMESTAMP \
               AND (params.scoped_organization IS NULL OR scope.organization_id = params.scoped_organization) \
             ORDER BY project.id ASC LIMIT 1) \
             UNION ALL \
             (SELECT project.id \
             FROM organization_memberships membership \
             JOIN organization_membership_roles role ON role.membership_id = membership.id \
             , params \
             CROSS JOIN LATERAL ( \
               SELECT candidate.id FROM projects candidate \
               WHERE candidate.organization_id = membership.organization_id ORDER BY candidate.id ASC LIMIT 1 \
             ) project \
             WHERE membership.principal_id = params.authority_principal \
               AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
               AND role.role_code IN ('ORGANIZATION_ADMIN', 'AUDITOR') \
               AND (params.scoped_organization IS NULL OR membership.organization_id = params.scoped_organization) \
             ORDER BY project.id ASC LIMIT 1) \
             UNION ALL \
             (SELECT project.id \
             FROM projects project, params \
             WHERE params.administrator \
               AND (params.scoped_organization IS NULL OR project.organization_id = params.scoped_organization) \
             ORDER BY project.id ASC LIMIT 1) \
           ) \
         SELECT DISTINCT candidate.project_id \
         FROM candidate_projects candidate JOIN projects project ON project.id = candidate.project_id, params \
         WHERE params.scoped_organization IS NULL OR project.organization_id = params.scoped_organization",
    )
    .bind(principal_id)
    .bind(organization_id)
    .bind(administrator)
    .fetch_all(&mut *conn)
    .await?;
    for (project_id,) in candidates {
        if tx::deployment_approval_capabilities(conn, principal_id, project_id, false)
            .await?
            .contains(tx::DEPLOYMENT_APPROVAL_VIEW)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Java port of `deployment_approval_visible_requirements()`'s final redefinition (V034). See the
/// scoping report: the capability recheck applies once over the fetched union rather than per branch
/// — safe for the platform-admin and direct-membership branches (their own `WHERE` already re-derives
/// what the recheck checks) but the three scope-cache-backed branches fetch unbounded (limited only
/// by the outer application-level limit) so a stale-cache row can never crowd a viewable requirement
/// out of a per-branch window before the recheck runs.
#[allow(clippy::too_many_arguments)]
async fn approval_inbox_requirement_ids(
    conn: &mut PgConnection,
    principal_id: Uuid,
    organization_id: Option<Uuid>,
    project_id: Option<Uuid>,
    cursor: &Option<ApprovalCursor>,
    first: i32,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let administrator = tx::has_platform_admin_read(conn, principal_id).await?;
    let cursor_requested_at = cursor.as_ref().map(|value| value.requested_at);
    let cursor_id = cursor.as_ref().map(|value| value.id);
    let rows_found: Vec<(Uuid, chrono::DateTime<chrono::Utc>, Uuid)> = sqlx::query_as(
        "WITH limits AS ( \
           SELECT LEAST(GREATEST($1, 1), 51) AS rows \
         ), candidates AS ( \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM deployment_approval_requirements requirement \
            WHERE $2 \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6)) \
            ORDER BY requirement.requested_at DESC, requirement.id DESC LIMIT (SELECT rows FROM limits)) \
           UNION ALL \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM deployment_approval_principal_organization_scopes scope \
            JOIN deployment_approval_requirements requirement ON requirement.organization_id = scope.organization_id \
            WHERE scope.principal_id = $7 AND scope.valid_after <= CURRENT_TIMESTAMP \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6))) \
           UNION ALL \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM organization_memberships membership \
            JOIN organization_membership_roles role ON role.membership_id = membership.id \
            JOIN projects project ON project.organization_id = membership.organization_id \
            JOIN deployment_approval_requirements requirement ON requirement.project_id = project.id \
            WHERE membership.principal_id = $7 \
              AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
              AND role.role_code IN ('ORGANIZATION_ADMIN', 'AUDITOR') \
              AND NOT EXISTS (SELECT 1 FROM deployment_approval_principal_organization_scopes scope \
                              WHERE scope.principal_id = membership.principal_id AND scope.organization_id = membership.organization_id) \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6)) \
            ORDER BY requirement.requested_at DESC, requirement.id DESC LIMIT (SELECT rows FROM limits)) \
           UNION ALL \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM deployment_approval_principal_organization_membership_scopes membership_scope \
            JOIN deployment_approval_principal_project_scopes scope ON scope.principal_id = membership_scope.principal_id \
            JOIN deployment_approval_requirements requirement ON requirement.project_id = scope.project_id \
              AND requirement.organization_id = membership_scope.organization_id \
            WHERE membership_scope.principal_id = $7 AND scope.valid_after <= CURRENT_TIMESTAMP \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6))) \
           UNION ALL \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM project_memberships membership \
            JOIN project_membership_roles role ON role.membership_id = membership.id \
            JOIN projects project ON project.id = membership.project_id \
            JOIN organization_memberships organization_membership ON organization_membership.organization_id = project.organization_id \
              AND organization_membership.principal_id = membership.principal_id \
            JOIN deployment_approval_requirements requirement ON requirement.project_id = membership.project_id \
            WHERE membership.principal_id = $7 \
              AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
              AND organization_membership.started_at <= CURRENT_TIMESTAMP AND organization_membership.ended_at IS NULL \
              AND role.role_code IN ('PROJECT_ADMIN', 'DEPLOYMENT_APPROVER', 'AUDITOR') \
              AND NOT EXISTS (SELECT 1 FROM deployment_approval_principal_project_scopes scope \
                              WHERE scope.principal_id = membership.principal_id AND scope.project_id = membership.project_id) \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6)) \
            ORDER BY requirement.requested_at DESC, requirement.id DESC LIMIT (SELECT rows FROM limits)) \
           UNION ALL \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM deployment_approval_principal_project_scopes scope \
            JOIN deployment_approval_requirements requirement ON requirement.project_id = scope.project_id \
            WHERE scope.principal_id = $7 AND scope.valid_after <= CURRENT_TIMESTAMP \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6))) \
         ) \
         SELECT DISTINCT id, requested_at, project_id FROM candidates",
    )
    .bind(first + 1)
    .bind(administrator)
    .bind(organization_id)
    .bind(project_id)
    .bind(cursor_requested_at)
    .bind(cursor_id)
    .bind(principal_id)
    .fetch_all(&mut *conn)
    .await?;

    let mut viewable_projects: HashMap<Uuid, bool> = HashMap::new();
    let mut viewable: Vec<(Uuid, chrono::DateTime<chrono::Utc>)> = Vec::new();
    for (id, requested_at, project_id) in rows_found {
        let viewable_project = match viewable_projects.get(&project_id) {
            Some(value) => *value,
            None => {
                let value =
                    tx::deployment_approval_capabilities(conn, principal_id, project_id, false)
                        .await?
                        .contains(tx::DEPLOYMENT_APPROVAL_VIEW);
                viewable_projects.insert(project_id, value);
                value
            }
        };
        if viewable_project {
            viewable.push((id, requested_at));
        }
    }
    let mut deduped: HashMap<Uuid, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let mut order: Vec<Uuid> = Vec::new();
    for (id, requested_at) in viewable {
        if let std::collections::hash_map::Entry::Vacant(entry) = deduped.entry(id) {
            entry.insert(requested_at);
            order.push(id);
        }
    }
    order.sort_by(|a, b| deduped[b].cmp(&deduped[a]).then_with(|| b.cmp(a)));
    order.truncate((first + 1) as usize);
    Ok(order)
}

pub async fn approval_inbox(
    conn: &mut PgConnection,
    principal_id: Uuid,
    organization_id: Option<Uuid>,
    project_id: Option<Uuid>,
    after: Option<&str>,
    first: i32,
    include_decision_preview: bool,
) -> Result<Option<ApprovalInboxConnection>, sqlx::Error> {
    if organization_id.is_some() && project_id.is_some() {
        return Ok(None);
    }
    if let Some(project_id) = project_id {
        if !can_approval_view(conn, principal_id, project_id, false).await? {
            return Ok(None);
        }
    } else if !has_approval_inbox_scope(conn, principal_id, organization_id).await? {
        return Ok(None);
    }
    let cursor = match cursors::decode_approval_cursor(after, organization_id, project_id) {
        Ok(cursor) => cursor,
        Err(()) => return Ok(None),
    };
    let mut ids = approval_inbox_requirement_ids(
        conn,
        principal_id,
        organization_id,
        project_id,
        &cursor,
        first,
    )
    .await?;
    // Re-run the P-10 keyset query after the ordered authority locks. This establishes the read at
    // the same authority boundary as the loaded facts without post-filtering a page.
    let mut requirements = rows::raw_requirements(conn, &ids, true).await?;
    let distinct_projects: Vec<Uuid> = requirements
        .values()
        .map(|value| value.project_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    for project in distinct_projects {
        tx::deployment_approval_capabilities(conn, principal_id, project, true).await?;
    }
    ids = approval_inbox_requirement_ids(
        conn,
        principal_id,
        organization_id,
        project_id,
        &cursor,
        first,
    )
    .await?;
    requirements = rows::raw_requirements(conn, &ids, true).await?;

    let deployment_ids: Vec<Uuid> = requirements
        .values()
        .map(|value| value.deployment_id)
        .collect();
    let deployments_list = rows::deployments(conn, &deployment_ids, false).await?;
    let deployments: HashMap<Uuid, Deployment> = deployments_list
        .into_iter()
        .map(|value| (value.id, value))
        .collect();
    let mut evidence = approval_evidence_for(conn, &deployment_ids).await?;
    let requirement_values: Vec<RawRequirement> =
        requirements.values().map(clone_raw_requirement).collect();
    let qualifying = qualifying_approval_counts(conn, &requirement_values).await?;
    let principals = approval_principals_for(conn, &requirement_values).await?;
    let decision_previews = if include_decision_preview {
        approval_decision_previews(conn, &ids).await?
    } else {
        HashMap::new()
    };
    let prior_decisions = approval_decision_requirement_ids(conn, &ids, principal_id).await?;
    let mut eligibility = HashMap::new();

    let mut rows_out = Vec::new();
    let mut cursors_out = Vec::new();
    for id in &ids {
        let Some(raw) = requirements.remove(id) else {
            continue;
        };
        let Some(deployment) = deployments.get(&raw.deployment_id) else {
            continue;
        };
        let deployment_evidence = evidence.remove(&raw.deployment_id).unwrap_or_default();
        let item = approval_item(
            conn,
            principal_id,
            &raw,
            deployment,
            *qualifying.get(&raw.id).unwrap_or(&0),
            &mut eligibility,
            deployment_evidence,
            &principals,
            decision_previews.get(&raw.id).cloned(),
            Some(prior_decisions.contains(&raw.id)),
        )
        .await?;
        cursors_out.push(cursors::encode_approval_cursor(
            organization_id,
            project_id,
            raw.requested_at,
            raw.id,
        ));
        rows_out.push(item);
    }
    let has_next = rows_out.len() > first as usize;
    if has_next {
        rows_out.pop();
        cursors_out.pop();
    }
    let end_cursor = cursors_out.last().cloned();
    Ok(Some(ApprovalInboxConnection {
        nodes: rows_out,
        cursors: cursors_out,
        end_cursor,
        has_next_page: has_next,
    }))
}

fn clone_raw_requirement(value: &RawRequirement) -> RawRequirement {
    RawRequirement {
        id: value.id,
        deployment_id: value.deployment_id,
        project_id: value.project_id,
        requester_id: value.requester_id,
        requested_at: value.requested_at,
        revision: value.revision,
        status: value.status,
        expires_at: value.expires_at,
        satisfied_at: value.satisfied_at,
        rejected_at: value.rejected_at,
        invalidated_at: value.invalidated_at,
        invalidation_code: value.invalidation_code.clone(),
        satisfied_participants: value.satisfied_participants.clone(),
        required_approvers: value.required_approvers,
    }
}

pub async fn approval_detail(
    conn: &mut PgConnection,
    principal_id: Uuid,
    approval_requirement_id: Uuid,
) -> Result<Option<ApprovalInboxItem>, sqlx::Error> {
    let Some(initial) = rows::raw_requirement(conn, approval_requirement_id, false).await? else {
        return Ok(None);
    };
    if rows::deployments(conn, &[initial.deployment_id], false)
        .await?
        .is_empty()
    {
        return Ok(None);
    }
    if !can_approval_view(conn, principal_id, initial.project_id, true).await? {
        return Ok(None);
    }
    let Some(raw) = rows::raw_requirement(conn, approval_requirement_id, true).await? else {
        return Ok(None);
    };
    let raw = reconcile_requirement(conn, raw).await?;
    Ok(Some(
        approval_item_for_requirement(conn, principal_id, &raw).await?,
    ))
}

async fn reconcile_requirement(
    conn: &mut PgConnection,
    raw: RawRequirement,
) -> Result<RawRequirement, sqlx::Error> {
    if raw.status != ApprovalRequirementStatus::Pending {
        return Ok(raw);
    }
    crate::deployment::approval::reconcile_pending(conn, raw.deployment_id).await?;
    rows::raw_requirement(conn, raw.id, false)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

pub async fn approval_decisions(
    conn: &mut PgConnection,
    principal_id: Uuid,
    approval_requirement_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<ApprovalDecisionConnection>, sqlx::Error> {
    if !(1..=50).contains(&first) {
        return Ok(None);
    }
    let Some(initial) = rows::raw_requirement(conn, approval_requirement_id, false).await? else {
        return Ok(None);
    };
    if rows::deployments(conn, &[initial.deployment_id], false)
        .await?
        .is_empty()
        || !can_approval_view(conn, principal_id, initial.project_id, true).await?
    {
        return Ok(None);
    }
    let cursor = match cursors::decode_approval_decision_cursor(after) {
        Ok(cursor) => cursor,
        Err(()) => return Ok(None),
    };
    if let Some(cursor) = &cursor {
        if cursor.requirement_id != approval_requirement_id {
            return Ok(None);
        }
    }
    let rows_found = sqlx::query(
        "SELECT id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, decided_at \
         FROM deployment_approval_decisions \
         WHERE approval_requirement_id = $1 \
           AND ($2::timestamptz IS NULL OR (decided_at, id) > ($2, $3)) \
         ORDER BY decided_at ASC, id ASC LIMIT $4",
    )
    .bind(approval_requirement_id)
    .bind(cursor.as_ref().map(|value| value.decided_at))
    .bind(cursor.as_ref().map(|value| value.id))
    .bind(first + 1)
    .fetch_all(&mut *conn)
    .await?;
    let mut values: Vec<ApprovalDecision> = rows_found
        .iter()
        .map(rows::decision_row)
        .map(approval_decision_from_row)
        .collect();
    let has_next = values.len() > first as usize;
    if has_next {
        values.pop();
    }
    let cursors_list: Vec<String> = values
        .iter()
        .map(|value| {
            cursors::encode_approval_decision_cursor(
                approval_requirement_id,
                value.decided_at,
                value.id,
            )
        })
        .collect();
    let end_cursor = cursors_list.last().cloned();
    Ok(Some(ApprovalDecisionConnection {
        nodes: values,
        cursors: cursors_list,
        end_cursor,
        has_next_page: has_next,
    }))
}

async fn decision_by_id(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<ApprovalDecision>, sqlx::Error> {
    let row = sqlx::query("SELECT id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, decided_at FROM deployment_approval_decisions WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row
        .as_ref()
        .map(rows::decision_row)
        .map(approval_decision_from_row))
}

struct ReplayDecision {
    decision: ApprovalDecision,
    request_fingerprint: String,
}

async fn decision_for_request(
    conn: &mut PgConnection,
    requirement_id: Uuid,
    actor: Uuid,
    request_id: Uuid,
) -> Result<Option<ReplayDecision>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, decided_at, request_fingerprint \
         FROM deployment_approval_decisions WHERE approval_requirement_id = $1 AND actor_principal_id = $2 AND request_key = $3",
    )
    .bind(requirement_id)
    .bind(actor)
    .bind(request_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| {
        let fingerprint: String = row.get("request_fingerprint");
        ReplayDecision {
            decision: approval_decision_from_row(rows::decision_row(&row)),
            request_fingerprint: fingerprint,
        }
    }))
}

pub async fn record_approval_decision(
    pool: &PgPool,
    command: ApprovalDecisionCommand,
    planner: ApprovalDecisionPlanner,
) -> Result<ApprovalDecisionMutationResult, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let result = record_approval_decision_tx(&mut tx, &command, planner).await;
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

async fn record_approval_decision_tx(
    conn: &mut PgConnection,
    command: &ApprovalDecisionCommand,
    planner: ApprovalDecisionPlanner,
) -> Result<ApprovalDecisionMutationResult, sqlx::Error> {
    let Some(initial) = rows::raw_requirement(conn, command.requirement_id, false).await? else {
        return Ok(decision_refusal(
            DomainApprovalDecisionProblem::unavailable(),
        ));
    };
    if rows::deployments(conn, &[initial.deployment_id], false)
        .await?
        .is_empty()
    {
        return Ok(decision_refusal(
            DomainApprovalDecisionProblem::unavailable(),
        ));
    }
    let Some(raw) = rows::raw_requirement(conn, command.requirement_id, true).await? else {
        return Ok(decision_refusal(
            DomainApprovalDecisionProblem::unavailable(),
        ));
    };
    lock_approval_authorities(conn, raw.id, command.principal_id, raw.project_id).await?;
    let visible = can_approval_view(conn, command.principal_id, initial.project_id, true).await?;
    if !visible {
        let facts = ApprovalDecisionFacts {
            requirement_id: raw.id,
            requester_id: raw.requester_id,
            revision: raw.revision,
            status: raw.status,
            invalidation_code: raw.invalidation_code.clone(),
            required_approvers: raw.required_approvers,
            qualifying_approvers: 0,
            visible: false,
            eligible: false,
            duplicate: false,
            expired: false,
            evidence_issue: None,
            waiting_for_evaluation: false,
        };
        let hidden = planner.plan(command, &facts);
        return Ok(decision_refusal(
            hidden
                .problem
                .unwrap_or_else(DomainApprovalDecisionProblem::unavailable),
        ));
    }
    if let Some(replay) = decision_for_request(
        conn,
        command.requirement_id,
        command.principal_id,
        command.request_id,
    )
    .await?
    {
        if decision_request_fingerprint(command) != replay.request_fingerprint {
            return Ok(decision_refusal(DomainApprovalDecisionProblem::of(
                "IDEMPOTENCY_CONFLICT",
            )));
        }
        let facts = serde_json::json!({
            "decisionId": replay.decision.id.to_string(),
            "requirementId": command.requirement_id.to_string(),
            "requestId": command.request_id.to_string(),
            "correlationId": command.correlation_id.to_string(),
            "outcome": "IMMUTABLE_DECISION_RETURNED",
        });
        super::writes::audit(
            conn,
            raw.deployment_id,
            Some(command.principal_id),
            "APPROVAL_REPLAYED",
            facts,
        )
        .await?;
        let current_requirement =
            rows::raw_requirement(conn, command.requirement_id, false).await?;
        let current_deployment = rows::deployments(conn, &[raw.deployment_id], false)
            .await?
            .into_iter()
            .next();
        return match (current_requirement, current_deployment) {
            (Some(current_requirement), Some(current_deployment)) => {
                let requirement = requirement_for(conn, &current_requirement).await?;
                Ok(ApprovalDecisionMutationResult::success(
                    replay.decision,
                    requirement,
                    current_deployment,
                ))
            }
            _ => Ok(decision_refusal(
                DomainApprovalDecisionProblem::unavailable(),
            )),
        };
    }
    if crate::deployment::approval::deployment_archive_boundary(conn, raw.deployment_id).await? {
        return Ok(decision_refusal(DomainApprovalDecisionProblem::of(
            "PROJECT_ARCHIVED",
        )));
    }
    let eligible = eligible_approver(conn, command.principal_id, raw.project_id, true).await?;
    if !eligible {
        return Ok(decision_refusal(DomainApprovalDecisionProblem::of(
            "APPROVER_INELIGIBLE",
        )));
    }
    let expired = crate::deployment::approval::requirement_expired(conn, raw.id).await?;
    let evidence_issue =
        crate::deployment::approval::approval_evidence_issue(conn, raw.deployment_id).await?;
    let waiting_for_evaluation = evidence_issue.as_deref() == Some("APPROVAL_EVIDENCE_MISSING")
        && crate::deployment::approval::waiting_for_evaluation(conn, raw.deployment_id).await?;
    let qualifying = qualifying_approvers(conn, raw.id, raw.project_id, raw.requester_id, true)
        .await?
        .len() as i32;
    let facts = ApprovalDecisionFacts {
        requirement_id: raw.id,
        requester_id: raw.requester_id,
        revision: raw.revision,
        status: raw.status,
        invalidation_code: raw.invalidation_code.clone(),
        required_approvers: raw.required_approvers,
        qualifying_approvers: qualifying,
        visible,
        eligible,
        duplicate: false,
        expired,
        evidence_issue: evidence_issue.clone(),
        waiting_for_evaluation,
    };
    let duplicate = has_decision(conn, command.requirement_id, command.principal_id).await?;
    let plan = planner.plan(command, &facts);
    if !plan.accepted() {
        return Ok(decision_refusal(
            plan.problem
                .unwrap_or_else(DomainApprovalDecisionProblem::unavailable),
        ));
    }
    let facts_with_duplicate = ApprovalDecisionFacts { duplicate, ..facts };
    let plan = planner.plan(command, &facts_with_duplicate);
    let raw = reconcile_requirement(conn, raw).await?;
    if raw.status != ApprovalRequirementStatus::Pending {
        let reconciled_facts = ApprovalDecisionFacts {
            status: raw.status,
            invalidation_code: raw.invalidation_code.clone(),
            expired: false,
            evidence_issue: None,
            waiting_for_evaluation: false,
            ..facts_with_duplicate
        };
        let reconciled = planner.plan(command, &reconciled_facts);
        return Ok(decision_refusal(
            reconciled
                .problem
                .unwrap_or_else(DomainApprovalDecisionProblem::unavailable),
        ));
    }
    if !plan.accepted() {
        return Ok(decision_refusal(
            plan.problem
                .unwrap_or_else(DomainApprovalDecisionProblem::unavailable),
        ));
    }
    // Repeat the expiry check at the write boundary.
    if crate::deployment::approval::requirement_expired(conn, raw.id).await? {
        let raw = reconcile_requirement(conn, raw).await?;
        return Ok(decision_refusal(requirement_state_problem(&raw)));
    }
    let decision_id = Uuid::new_v4();
    insert_decision(
        conn,
        decision_id,
        command.requirement_id,
        command.principal_id,
        &command.value,
        command.comment.as_deref(),
        command.rejection_reason.as_deref(),
        command.request_id,
        command.correlation_id,
        command.expected_revision,
        &decision_request_fingerprint(command),
    )
    .await?;
    super::writes::audit(
        conn,
        raw.deployment_id,
        Some(command.principal_id),
        "APPROVAL_RECORDED",
        serde_json::json!({
            "decisionId": decision_id.to_string(), "decision": command.value, "requirementId": command.requirement_id.to_string(),
            "requestId": command.request_id.to_string(), "correlationId": command.correlation_id.to_string(),
            "eligibilityCapability": tx::DEPLOYMENT_APPROVAL_DECIDE, "eligibilityGranted": true,
        }),
    )
    .await?;
    if plan.rejection {
        crate::deployment::approval::transition_requirement(
            conn,
            raw.id,
            ApprovalRequirementStatus::Rejected,
            None,
            &[],
        )
        .await?;
        crate::deployment::approval::cancel_rejected_deployment(conn, raw.deployment_id).await?;
        super::writes::audit(
            conn,
            raw.deployment_id,
            Some(command.principal_id),
            "APPROVAL_REJECTED",
            serde_json::json!({
                "decisionId": decision_id.to_string(), "requirementId": command.requirement_id.to_string(),
                "rejectionReason": command.rejection_reason.as_deref().unwrap_or("").trim(), "correlationId": command.correlation_id.to_string(),
            }),
        )
        .await?;
    } else if plan.satisfies_requirement {
        let participants =
            qualifying_approvers(conn, raw.id, raw.project_id, raw.requester_id, true).await?;
        crate::deployment::approval::transition_requirement(
            conn,
            raw.id,
            ApprovalRequirementStatus::Satisfied,
            None,
            &participants,
        )
        .await?;
        crate::deployment::approval::approve_deployment_for_execution(conn, raw.deployment_id)
            .await?;
        super::writes::audit(
            conn,
            raw.deployment_id,
            Some(command.principal_id),
            "APPROVAL_SATISFIED",
            serde_json::json!({
                "requirementId": command.requirement_id.to_string(),
                "participantIds": participants.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "correlationId": command.correlation_id.to_string(),
            }),
        )
        .await?;
    }
    let result = rows::raw_requirement(conn, command.requirement_id, false).await?;
    let current = rows::deployments(conn, &[raw.deployment_id], false)
        .await?
        .into_iter()
        .next();
    let recorded = decision_by_id(conn, decision_id).await?;
    match (result, current, recorded) {
        (Some(result), Some(current), Some(recorded)) => {
            let response_requirement = requirement_for(conn, &result).await?;
            Ok(ApprovalDecisionMutationResult::success(
                recorded,
                response_requirement,
                current,
            ))
        }
        _ => Ok(decision_refusal(
            DomainApprovalDecisionProblem::unavailable(),
        )),
    }
}
