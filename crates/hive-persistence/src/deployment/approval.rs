//! Ports the approval-requirement lifecycle machinery `deploy`/`recovery`/the
//! outbox worker's `execute` share: reconciliation, evidence-readiness,
//! handoff eligibility, and blocking. Deliberately excludes the approval
//! inbox/decision surface (`approvalInbox`/`approvalDetail`/`approvalDecisions`/
//! `recordApprovalDecision`, in `queries.rs`).
//!
//! Includes a literal port of `PostgresDeploymentApprovalEvidenceIssue`
//! (`approval_evidence_issue`/`waiting_for_evaluation`): `reconcile_pending`
//! (needed by `automatic_approval_handoff`, needed by every deploy/recovery
//! path) calls it.
//!
//! Also includes RTP-APPROVAL's scheduled reconciliation entry points
//! (`reconcile_approval_expiry`/`reconcile_approval_upgrade`, ports of
//! `PostgresDeploymentRepository.reconcileApprovalExpiry`/`reconcileApprovalUpgrade`)
//! and the maintenance-heartbeat helpers (`reconcile_expired_approval_requirements`,
//! `release_compatible_approval_handoffs`, `approval_maintenance_failed`) the
//! outbox worker's `record_worker_heartbeat` (in `worker.rs`) composes into its
//! own opportunistic maintenance branch.

use crate::deployment::rows::{self, raw_requirement_by_deployment};
use crate::deployment::writes::{audit, next_timeline_sequence, touch_projection};
use crate::sql::json_array;
use hive_domain::deployment::{ApprovalRequirementStatus, DeploymentLifecycleStatus};
use serde_json::json;
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

/// The 0-rows-affected branch below mirrors Java's synthetic `throw new SQLException(..., "40001")`
/// with a plain `RowNotFound` instead of a fabricated SQLSTATE: every call site already holds the
/// row's lock from a `FOR UPDATE` read moments earlier in the same transaction, so DSQL's real
/// commit-time OCC validation — not this defensive check — is what actually catches a genuine
/// concurrent race (as an authentic SQLSTATE 40001 from `tx.commit()`). This check exists only for
/// defense in depth, matching Java's own belt-and-suspenders style.
pub async fn transition_requirement(
    conn: &mut PgConnection,
    requirement_id: Uuid,
    status: ApprovalRequirementStatus,
    code: Option<&str>,
    participants: &[Uuid],
) -> Result<(), sqlx::Error> {
    let participant_strings: Vec<String> = participants.iter().map(ToString::to_string).collect();
    let result = sqlx::query(
        "UPDATE deployment_approval_requirements SET status = $1, revision = revision + 1, \
           satisfied_at = CASE WHEN $1 = 'SATISFIED' THEN CURRENT_TIMESTAMP ELSE NULL END, \
           rejected_at = CASE WHEN $1 = 'REJECTED' THEN CURRENT_TIMESTAMP ELSE NULL END, \
           invalidated_at = CASE WHEN $1 = 'INVALIDATED' THEN CURRENT_TIMESTAMP ELSE NULL END, \
           invalidation_code = CASE WHEN $1 = 'INVALIDATED' THEN $2 ELSE NULL END, \
           satisfied_participants = $3::jsonb \
         WHERE id = $4 AND status = 'PENDING' AND ($1 NOT IN ('SATISFIED', 'REJECTED') OR expires_at > clock_timestamp())",
    )
    .bind(status.as_str())
    .bind(code)
    .bind(json_array(&participant_strings))
    .bind(requirement_id)
    .execute(&mut *conn)
    .await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

pub async fn cancel_rejected_deployment(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Approval rejection terminalized this local deployment.', \
             observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
    )
    .bind(deployment_id)
    .execute(&mut *conn)
    .await?;
    let result = sqlx::query("UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')")
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

pub async fn approve_deployment_for_execution(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<(), sqlx::Error> {
    let result = sqlx::query("UPDATE deployments SET lifecycle_status = 'APPROVED', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')")
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Box::pin(automatic_approval_handoff(conn, deployment_id)).await?;
    Ok(())
}

pub async fn invalidate_pending_requirement(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    code: &str,
    actor: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let Some(raw) = raw_requirement_by_deployment(conn, deployment_id, true).await? else {
        return Ok(());
    };
    if raw.status != ApprovalRequirementStatus::Pending {
        return Ok(());
    }
    transition_requirement(
        conn,
        raw.id,
        ApprovalRequirementStatus::Invalidated,
        Some(code),
        &[],
    )
    .await?;
    audit(
        conn,
        deployment_id,
        actor,
        "APPROVAL_INVALIDATED",
        json!({"requirementId": raw.id.to_string(), "code": code}),
    )
    .await
}

pub async fn requirement_expired(
    conn: &mut PgConnection,
    requirement_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("SELECT 1 FROM deployment_approval_requirements WHERE id = $1 AND expires_at <= clock_timestamp()").bind(requirement_id).fetch_optional(&mut *conn).await?.is_some())
}

pub async fn deployment_archive_boundary(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "SELECT 1 FROM deployments deployment \
           JOIN deployment_approval_project_archive_events event ON event.project_id = deployment.project_id \
         WHERE deployment.id = $1 \
           AND ((deployment.project_lifecycle_revision IS NOT NULL AND event.archived_project_revision IS NOT NULL \
                   AND deployment.project_lifecycle_revision <= event.archived_project_revision) \
               OR ((deployment.project_lifecycle_revision IS NULL OR event.archived_project_revision IS NULL) \
                   AND deployment.requested_at <= event.archived_at))",
    )
    .bind(deployment_id)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

pub async fn worker_ready(conn: &mut PgConnection) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("SELECT 1 FROM deployment_worker_heartbeats heartbeat WHERE heartbeat.approval_execution_compatible AND heartbeat.state = 'READY' AND heartbeat.observed_at > clock_timestamp() - INTERVAL '15 seconds'")
        .fetch_optional(&mut *conn)
        .await?
        .is_some())
}

pub async fn compatible_approval_worker(
    conn: &mut PgConnection,
    worker: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "SELECT 1 FROM deployment_worker_heartbeats heartbeat WHERE heartbeat.worker_id = $1 AND heartbeat.approval_execution_compatible \
           AND heartbeat.state = 'READY' AND heartbeat.observed_at > clock_timestamp() - INTERVAL '15 seconds'",
    )
    .bind(worker)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

pub async fn approved_approval_handoff(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("SELECT 1 FROM deployment_approval_requirements requirement JOIN deployments deployment ON deployment.id = requirement.deployment_id WHERE requirement.deployment_id = $1 AND requirement.status = 'SATISFIED' AND deployment.lifecycle_status = 'APPROVED'")
            .bind(deployment_id)
            .fetch_optional(&mut *conn)
            .await?
            .is_some(),
    )
}

/// Returns a raced M13 claim to the compatible-worker gate without consuming retry capacity.
pub async fn defer_incompatible_approval_handoff(
    conn: &mut PgConnection,
    event_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE deployment_outbox_events SET status = 'PENDING', available_at = CURRENT_TIMESTAMP + INTERVAL '5 seconds' \
             + get_byte(uuid_send(id), 0) * INTERVAL '1 millisecond', \
           claimed_at = NULL, claimed_by = NULL, attempt_count = GREATEST(attempt_count - 1, 0), \
           last_error = 'An incompatible worker cannot execute an approved handoff.' \
         WHERE id = $1 AND status = 'PROCESSING'",
    )
    .bind(event_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Returns a retained event to the queue until maintenance establishes its frozen handoff.
pub async fn defer_pending_approval_handoff(
    conn: &mut PgConnection,
    event_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE deployment_outbox_events SET status = 'PENDING', available_at = CURRENT_TIMESTAMP + INTERVAL '1 second' \
             + get_byte(uuid_send(id), 0) * INTERVAL '1 millisecond', \
           claimed_at = NULL, claimed_by = NULL, attempt_count = GREATEST(attempt_count - 1, 0), \
           last_error = 'The approval handoff is not ready for execution.' \
         WHERE id = $1 AND status = 'PROCESSING'",
    )
    .bind(event_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

struct PolicyRow {
    binding_digest: String,
    agent_version_id: Uuid,
    environment_definition_version_id: Uuid,
    target_digest: String,
    plan_digest: String,
    package_digest: String,
    required_evidence: Vec<String>,
}

fn policy_row(row: &sqlx::postgres::PgRow) -> PolicyRow {
    let required_evidence_json: String = row.get(6);
    PolicyRow {
        binding_digest: row.get(0),
        agent_version_id: row.get(1),
        environment_definition_version_id: row.get(2),
        target_digest: row.get(3),
        plan_digest: row.get(4),
        package_digest: row.get(5),
        required_evidence: crate::sql::parse_string_array(&required_evidence_json),
    }
}

async fn evidence_issue_policy_row(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<Option<PolicyRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT policy.binding_digest, policy.agent_version_id, policy.environment_definition_version_id, \
             policy.target_digest, policy.plan_digest, policy.package_digest, policy.required_evidence::text \
         FROM deployments deployment \
           JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id \
           JOIN deployment_plan_versions plan ON plan.deployment_id = deployment.id AND plan.version_number = 1 \
         WHERE deployment.id = $1 \
           AND policy.agent_version_id = deployment.agent_version_id \
           AND policy.environment_definition_version_id = deployment.environment_definition_version_id \
           AND policy.target_digest = plan.target_digest \
           AND policy.plan_digest = plan.plan_digest \
           AND policy.package_digest = plan.package_digest \
           AND policy.logical_environment_class = deployment.environment \
           AND policy.policy_matrix -> (policy.logical_environment_class || '_' || policy.risk) -> 'requiredApprovers' = to_jsonb(policy.required_approvers) \
           AND policy.policy_matrix -> (policy.logical_environment_class || '_' || policy.risk) -> 'requiredEvidence' = policy.required_evidence",
    )
    .bind(deployment_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.as_ref().map(policy_row))
}

async fn waiting_policy_row(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<Option<PolicyRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT policy.binding_digest, policy.agent_version_id, policy.environment_definition_version_id, \
             policy.target_digest, policy.plan_digest, policy.package_digest, policy.required_evidence::text \
         FROM deployment_policy_snapshots policy JOIN deployments deployment ON deployment.id = policy.deployment_id \
         WHERE policy.deployment_id = $1",
    )
    .bind(deployment_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.as_ref().map(policy_row))
}

async fn valid_evidence(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    policy: &PolicyRow,
    kind: &str,
) -> Result<bool, sqlx::Error> {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM deployment_evidence_snapshots evidence \
          WHERE evidence.deployment_id = $1 AND evidence.evidence_kind = $2 \
            AND evidence.binding_digest = $3 AND evidence.agent_version_id = $4 \
            AND evidence.environment_definition_version_id = $5 AND evidence.target_digest = $6 \
            AND evidence.plan_digest = $7 AND evidence.package_digest = $8 \
            AND (evidence.expires_at IS NULL OR evidence.expires_at > clock_timestamp()) \
            AND NOT EXISTS (SELECT 1 FROM deployment_evidence_invalidations invalidation WHERE invalidation.evidence_snapshot_id = evidence.id))",
    )
    .bind(deployment_id)
    .bind(kind)
    .bind(&policy.binding_digest)
    .bind(policy.agent_version_id)
    .bind(policy.environment_definition_version_id)
    .bind(&policy.target_digest)
    .bind(&policy.plan_digest)
    .bind(&policy.package_digest)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.0)
}

async fn observed_evidence(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    kind: &str,
) -> Result<bool, sqlx::Error> {
    let row: (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM deployment_evidence_snapshots evidence WHERE evidence.deployment_id = $1 AND evidence.evidence_kind = $2)").bind(deployment_id).bind(kind).fetch_one(&mut *conn).await?;
    Ok(row.0)
}

async fn expired_evidence(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    policy: &PolicyRow,
    kind: &str,
) -> Result<bool, sqlx::Error> {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM deployment_evidence_snapshots evidence \
          WHERE evidence.deployment_id = $1 AND evidence.evidence_kind = $2 \
            AND evidence.binding_digest = $3 AND evidence.agent_version_id = $4 \
            AND evidence.environment_definition_version_id = $5 AND evidence.target_digest = $6 \
            AND evidence.plan_digest = $7 AND evidence.package_digest = $8 \
            AND evidence.expires_at <= clock_timestamp() \
            AND NOT EXISTS (SELECT 1 FROM deployment_evidence_invalidations invalidation WHERE invalidation.evidence_snapshot_id = evidence.id))",
    )
    .bind(deployment_id)
    .bind(kind)
    .bind(&policy.binding_digest)
    .bind(policy.agent_version_id)
    .bind(policy.environment_definition_version_id)
    .bind(&policy.target_digest)
    .bind(&policy.plan_digest)
    .bind(&policy.package_digest)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.0)
}

/// Ports `PostgresDeploymentApprovalEvidenceIssue.evidenceIssue`. Returns
/// `APPROVAL_EVIDENCE_MISMATCH`/`APPROVAL_EVIDENCE_MISSING`/`APPROVAL_EVIDENCE_EXPIRED`,
/// or `None` when every required evidence kind is valid.
pub async fn approval_evidence_issue(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let Some(policy) = evidence_issue_policy_row(conn, deployment_id).await? else {
        return Ok(Some("APPROVAL_EVIDENCE_MISMATCH".to_string()));
    };
    for kind in &policy.required_evidence {
        if valid_evidence(conn, deployment_id, &policy, kind).await? {
            continue;
        }
        if !observed_evidence(conn, deployment_id, kind).await? {
            return Ok(Some("APPROVAL_EVIDENCE_MISSING".to_string()));
        }
        if expired_evidence(conn, deployment_id, &policy, kind).await? {
            return Ok(Some("APPROVAL_EVIDENCE_EXPIRED".to_string()));
        }
        return Ok(Some("APPROVAL_EVIDENCE_MISMATCH".to_string()));
    }
    Ok(None)
}

/// Ports `PostgresDeploymentApprovalEvidenceIssue.waitingForEvaluation`.
pub async fn waiting_for_evaluation(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let Some(policy) = waiting_policy_row(conn, deployment_id).await? else {
        return Ok(false);
    };
    if !policy
        .required_evidence
        .iter()
        .any(|kind| kind == "EVALUATION_PASSED")
    {
        return Ok(false);
    }
    if approval_evidence_issue(conn, deployment_id)
        .await?
        .as_deref()
        != Some("APPROVAL_EVIDENCE_MISSING")
    {
        return Ok(false);
    }
    if observed_evidence(conn, deployment_id, "EVALUATION_PASSED").await? {
        return Ok(false);
    }
    for kind in &policy.required_evidence {
        if kind == "EVALUATION_PASSED" {
            continue;
        }
        if !valid_evidence(conn, deployment_id, &policy, kind).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub async fn evidence_ready(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "SELECT 1 FROM deployments deployment \
           JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id \
           JOIN deployment_plan_versions plan ON plan.deployment_id = deployment.id AND plan.version_number = 1 \
         WHERE deployment.id = $1 \
           AND policy.agent_version_id = deployment.agent_version_id \
           AND policy.environment_definition_version_id = deployment.environment_definition_version_id \
           AND policy.target_digest = plan.target_digest AND policy.plan_digest = plan.plan_digest AND policy.package_digest = plan.package_digest \
           AND policy.logical_environment_class = deployment.environment \
           AND policy.policy_matrix -> (policy.logical_environment_class || '_' || policy.risk) -> 'requiredApprovers' = to_jsonb(policy.required_approvers) \
           AND policy.policy_matrix -> (policy.logical_environment_class || '_' || policy.risk) -> 'requiredEvidence' = policy.required_evidence \
           AND NOT EXISTS (SELECT 1 FROM jsonb_array_elements_text(policy.required_evidence) required(evidence_kind) \
             WHERE NOT EXISTS (SELECT 1 FROM deployment_evidence_snapshots evidence \
               WHERE evidence.deployment_id = deployment.id AND evidence.evidence_kind = required.evidence_kind \
                 AND evidence.binding_digest = policy.binding_digest AND evidence.agent_version_id = policy.agent_version_id \
                 AND evidence.environment_definition_version_id = policy.environment_definition_version_id \
                 AND evidence.target_digest = policy.target_digest AND evidence.plan_digest = policy.plan_digest AND evidence.package_digest = policy.package_digest \
                 AND (evidence.expires_at IS NULL OR evidence.expires_at > clock_timestamp()) \
                 AND NOT EXISTS (SELECT 1 FROM deployment_evidence_invalidations invalidation WHERE invalidation.evidence_snapshot_id = evidence.id)))",
    )
    .bind(deployment_id)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

/// Java port of `deployment_approval_reconcile_pending()`'s final redefinition (V017).
pub async fn reconcile_pending(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let lifecycle: Option<(String,)> =
        sqlx::query_as("SELECT lifecycle_status FROM deployments WHERE id = $1 FOR UPDATE")
            .bind(deployment_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((lifecycle,)) = lifecycle else {
        return Ok(false);
    };
    let lifecycle = rows::lifecycle_status(lifecycle);

    let requirement_id: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM deployment_approval_requirements WHERE deployment_id = $1 AND status = 'PENDING' FOR UPDATE").bind(deployment_id).fetch_optional(&mut *conn).await?;
    let Some((requirement_id,)) = requirement_id else {
        let satisfied_expired = sqlx::query(
            "SELECT 1 FROM deployment_approval_requirements requirement WHERE requirement.deployment_id = $1 AND requirement.status = 'SATISFIED' AND requirement.expires_at <= clock_timestamp()",
        )
        .bind(deployment_id)
        .fetch_optional(&mut *conn)
        .await?
        .is_some();
        if satisfied_expired {
            block_approval_execution(conn, deployment_id, None).await?;
            return Ok(true);
        }
        return Ok(false);
    };

    let (action, issue): (&str, Option<String>) = if lifecycle.has_started_execution() {
        (
            "APPROVAL_INVALIDATED",
            Some("TERMINAL_LIFECYCLE".to_string()),
        )
    } else if requirement_expired(conn, requirement_id).await? {
        (
            "APPROVAL_EXPIRED",
            Some("APPROVAL_REQUIREMENT_EXPIRED".to_string()),
        )
    } else {
        let issue = approval_evidence_issue(conn, deployment_id).await?;
        if issue.is_none()
            || (issue.as_deref() == Some("APPROVAL_EVIDENCE_MISSING")
                && waiting_for_evaluation(conn, deployment_id).await?)
        {
            return Ok(false);
        }
        ("APPROVAL_INVALIDATED", issue)
    };
    let status = if action == "APPROVAL_EXPIRED" {
        ApprovalRequirementStatus::Expired
    } else {
        ApprovalRequirementStatus::Invalidated
    };
    transition_requirement(conn, requirement_id, status, issue.as_deref(), &[]).await?;
    let sequence = next_timeline_sequence(conn, deployment_id, 0).await?;
    crate::audit::bind_audit_metadata(
        sqlx::query(
            "INSERT INTO deployment_audit_events \
               (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
                request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, NULL, $3, jsonb_build_object('requirementId', $4, 'code', $5), NULL, 0, $6, $7, $8, $9, $10, $11)",
        )
        .bind(Uuid::new_v4())
        .bind(deployment_id)
        .bind(action)
        .bind(requirement_id.to_string())
        .bind(&issue)
        .bind(sequence),
    )
    .execute(&mut *conn)
    .await?;
    touch_projection(conn, deployment_id).await?;
    if matches!(
        lifecycle,
        DeploymentLifecycleStatus::AwaitingApproval | DeploymentLifecycleStatus::Requested
    ) {
        sqlx::query(
            "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Approval could not complete with the frozen requirement facts.', \
                 observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
        )
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;
        sqlx::query("UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, projection_revision = projection_revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')")
            .bind(deployment_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(true)
}

/// Java port of `deployment_approval_block_invalid_handoff()`'s final redefinition (V033). `actor` is
/// `None` except when `reconcileProjectArchives` (RTP-APPROVAL's job, not ported here) calls the
/// actor-carrying overload with the archive event's own actor.
pub async fn block_approval_execution(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    actor: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    let satisfied_and_pending = sqlx::query(
        "SELECT 1 FROM deployment_approval_requirements requirement JOIN deployments deployment ON deployment.id = requirement.deployment_id \
         WHERE requirement.deployment_id = $1 AND requirement.status = 'SATISFIED' AND deployment.lifecycle_status IN ('REQUESTED', 'APPROVED')",
    )
    .bind(deployment_id)
    .fetch_optional(&mut *conn)
    .await?
    .is_some();
    if !satisfied_and_pending {
        return Ok(false);
    }
    let archive_boundary = deployment_archive_boundary(conn, deployment_id).await?;
    let project_active: (bool,) = sqlx::query_as("SELECT project.lifecycle_status = 'ACTIVE' FROM deployments deployment JOIN projects project ON project.id = deployment.project_id WHERE deployment.id = $1")
        .bind(deployment_id)
        .fetch_one(&mut *conn)
        .await
        .unwrap_or((false,));
    let project_active = project_active.0;
    if project_active
        && !archive_boundary
        && evidence_ready(conn, deployment_id).await?
        && !sqlx::query("SELECT 1 FROM deployment_approval_requirements WHERE deployment_id = $1 AND expires_at <= clock_timestamp()").bind(deployment_id).fetch_optional(&mut *conn).await?.is_some()
    {
        return Ok(false);
    }
    let evidence_issue = approval_evidence_issue(conn, deployment_id).await?;
    let block_code = if archive_boundary {
        "PROJECT_ARCHIVED".to_string()
    } else if project_active {
        evidence_issue.unwrap_or_else(|| "APPROVAL_EVIDENCE_NO_LONGER_VALID".to_string())
    } else {
        "PROJECT_NOT_ACTIVE".to_string()
    };
    sqlx::query(
        "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Execution stopped because frozen approval requirements were no longer executable.', \
             observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
    )
    .bind(deployment_id)
    .execute(&mut *conn)
    .await?;
    let updated = sqlx::query("UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, projection_revision = projection_revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('REQUESTED', 'APPROVED')")
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;
    if updated.rows_affected() == 0 {
        return Ok(false);
    }
    sqlx::query("DELETE FROM deployment_approval_handoff_releases WHERE deployment_id = $1")
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;
    let sequence = next_timeline_sequence(conn, deployment_id, 0).await?;
    crate::audit::bind_audit_metadata(
        sqlx::query(
            "INSERT INTO deployment_audit_events \
               (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
                request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, $3, 'APPROVAL_EXECUTION_BLOCKED', jsonb_build_object('code', $4), NULL, 0, $5, $6, $7, $8, $9, $10)",
        )
        .bind(Uuid::new_v4())
        .bind(deployment_id)
        .bind(actor)
        .bind(&block_code)
        .bind(sequence),
    )
    .execute(&mut *conn)
    .await?;
    touch_projection(conn, deployment_id).await?;
    Ok(true)
}

/// Java port of `deployment_approval_execution_eligible()`'s final redefinition (V033).
pub async fn approval_execution_eligible(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query(
        "SELECT requirement.required_approvers, jsonb_array_length(requirement.satisfied_participants) AS participant_count, \
           (SELECT count(DISTINCT participant) FROM jsonb_array_elements_text(requirement.satisfied_participants) participant) AS distinct_count, \
           NOT EXISTS (SELECT 1 FROM jsonb_array_elements_text(requirement.satisfied_participants) participant \
             WHERE NOT EXISTS (SELECT 1 FROM deployment_approval_decisions decision \
               WHERE decision.approval_requirement_id = requirement.id AND decision.actor_principal_id = participant::uuid AND decision.decision = 'APPROVE')) AS participants_valid \
         FROM deployment_approval_requirements requirement \
           JOIN deployments deployment ON deployment.id = requirement.deployment_id \
           JOIN projects project ON project.id = deployment.project_id \
         WHERE requirement.deployment_id = $1 AND requirement.status = 'SATISFIED' AND requirement.expires_at > clock_timestamp() \
           AND deployment.lifecycle_status IN ('APPROVED', 'REQUESTED') AND project.lifecycle_status = 'ACTIVE'",
    )
    .bind(deployment_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let required: i32 = row.get(0);
    let participant_count: i32 = row.get(1);
    let distinct_count: i64 = row.get(2);
    let participants_valid: bool = row.get(3);
    if participant_count != required || distinct_count != required as i64 || !participants_valid {
        return Ok(false);
    }
    if deployment_archive_boundary(conn, deployment_id).await? {
        return Ok(false);
    }
    evidence_ready(conn, deployment_id).await
}

/// Java port of `deployment_approval_ensure_requirement()`'s final redefinition (V032).
pub async fn ensure_requirement(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let archived = deployment_archive_boundary(conn, deployment_id).await?;
    let terminal_lifecycle_case = "CASE WHEN $1 OR deployment.lifecycle_status IN ('IN_PROGRESS', 'ACTIVE', 'FAILED', 'CANCELED', 'ROLLED_BACK')";
    let sql = format!(
        "INSERT INTO deployment_approval_requirements (id, deployment_id, revision, organization_id, project_id, requested_at, \
                                                       required_approvers, status, expires_at, invalidated_at, invalidation_code) \
         SELECT $2, deployment.id, 1, deployment.organization_id, deployment.project_id, deployment.requested_at, \
                policy.required_approvers, \
                {terminal_lifecycle_case} THEN 'INVALIDATED' ELSE 'PENDING' END, \
                deployment.requested_at + INTERVAL '24 hours', \
                {terminal_lifecycle_case} THEN CURRENT_TIMESTAMP ELSE NULL END, \
                CASE WHEN $1 THEN 'PROJECT_ARCHIVED' \
                     WHEN deployment.lifecycle_status IN ('IN_PROGRESS', 'ACTIVE', 'FAILED', 'CANCELED', 'ROLLED_BACK') THEN 'TERMINAL_LIFECYCLE' \
                     ELSE NULL END \
         FROM deployments deployment JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id \
         WHERE deployment.id = $3 \
         ON CONFLICT (deployment_id) DO NOTHING \
         RETURNING id, status"
    );
    let inserted = sqlx::query(&sql)
        .bind(archived)
        .bind(Uuid::new_v4())
        .bind(deployment_id)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(row) = inserted {
        let requirement_id: Uuid = row.get(0);
        let status = rows::requirement_status(row.get(1));
        if status == ApprovalRequirementStatus::Invalidated {
            if archived {
                invalidate_archived_approval_requirement(conn, deployment_id, requirement_id)
                    .await?;
            } else {
                record_terminal_invalidation(conn, deployment_id, requirement_id).await?;
            }
        }
    }
    Ok(
        sqlx::query("SELECT 1 FROM deployment_approval_requirements WHERE deployment_id = $1")
            .bind(deployment_id)
            .fetch_optional(&mut *conn)
            .await?
            .is_some(),
    )
}

/// Ported alongside `ensure_requirement` as its archived-at-creation branch (V032's final
/// `deployment_approval_ensure_requirement()` body).
async fn invalidate_archived_approval_requirement(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    requirement_id: Uuid,
) -> Result<(), sqlx::Error> {
    let archive_actor: Option<(Option<Uuid>,)> = sqlx::query_as(
        "SELECT event.actor_principal_id \
         FROM deployment_approval_project_archive_events event JOIN deployments deployment ON deployment.project_id = event.project_id \
         WHERE deployment.id = $1 \
           AND ((deployment.project_lifecycle_revision IS NOT NULL AND event.archived_project_revision IS NOT NULL \
                 AND deployment.project_lifecycle_revision <= event.archived_project_revision) \
             OR ((deployment.project_lifecycle_revision IS NULL OR event.archived_project_revision IS NULL) \
                 AND deployment.requested_at <= event.archived_at)) \
         ORDER BY event.archived_project_revision DESC NULLS LAST, event.archived_at DESC, event.id DESC LIMIT 1",
    )
    .bind(deployment_id)
    .fetch_optional(&mut *conn)
    .await?;
    let archive_actor = archive_actor.and_then(|(actor,)| actor);
    let sequence = next_timeline_sequence(conn, deployment_id, 0).await?;
    crate::audit::bind_audit_metadata(
        sqlx::query(
            "INSERT INTO deployment_audit_events \
               (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
                request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, $3, 'APPROVAL_INVALIDATED', jsonb_build_object('requirementId', $4, 'code', 'PROJECT_ARCHIVED'), NULL, 0, $5, $6, $7, $8, $9, $10)",
        )
        .bind(Uuid::new_v4())
        .bind(deployment_id)
        .bind(archive_actor)
        .bind(requirement_id.to_string())
        .bind(sequence),
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query("UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Project archive terminalized this delayed approval cycle.', observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1")
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, projection_revision = projection_revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')")
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM deployment_approval_handoff_releases WHERE deployment_id = $1")
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;
    touch_projection(conn, deployment_id).await
}

/// Java port of `deployment_approval_record_terminal_invalidation()`'s final redefinition (V018),
/// `ensure_requirement`'s non-archived INVALIDATED branch.
async fn record_terminal_invalidation(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    requirement_id: Uuid,
) -> Result<(), sqlx::Error> {
    let sequence = next_timeline_sequence(conn, deployment_id, 0).await?;
    crate::audit::bind_audit_metadata(
        sqlx::query(
            "INSERT INTO deployment_audit_events \
               (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
                request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, NULL, 'APPROVAL_INVALIDATED', jsonb_build_object('requirementId', $3, 'code', 'TERMINAL_LIFECYCLE'), NULL, 0, $4, $5, $6, $7, $8, $9)",
        )
        .bind(Uuid::new_v4())
        .bind(deployment_id)
        .bind(requirement_id.to_string())
        .bind(sequence),
    )
    .execute(&mut *conn)
    .await?;
    touch_projection(conn, deployment_id).await
}

/// The migration-owned handoff atomically validates evidence, satisfies a zero-approver rule, and
/// queues local execution. Java port of `deployment_approval_automatic_handoff()`'s final
/// redefinition (V024), called with its `enqueue_execution` default (`TRUE`) — the only value any
/// caller in this port's scope needs.
pub async fn automatic_approval_handoff(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<bool, sqlx::Error> {
    if !ensure_requirement(conn, deployment_id).await? {
        return Ok(false);
    }
    if deployment_archive_boundary(conn, deployment_id).await? {
        return Ok(false);
    }
    reconcile_pending(conn, deployment_id).await?;

    let row = sqlx::query(
        "SELECT requirement.id, requirement.status, requirement.required_approvers, deployment.lifecycle_status, requirement.expires_at \
         FROM deployment_approval_requirements requirement JOIN deployments deployment ON deployment.id = requirement.deployment_id \
         WHERE requirement.deployment_id = $1 FOR UPDATE OF requirement, deployment",
    )
    .bind(deployment_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let requirement_id: Uuid = row.get(0);
    let mut requirement_status = rows::requirement_status(row.get(1));
    let required_approvers: i32 = row.get(2);
    let lifecycle = rows::lifecycle_status(row.get(3));
    let requirement_expiry: chrono::DateTime<chrono::Utc> = row.get(4);

    let requested_or_approved = lifecycle.awaits_execution();
    if requirement_status == ApprovalRequirementStatus::Satisfied
        && requested_or_approved
        && requirement_expiry <= chrono::Utc::now()
    {
        block_approval_execution(conn, deployment_id, None).await?;
        return Ok(false);
    }
    if requirement_status == ApprovalRequirementStatus::Pending
        && required_approvers == 0
        && !lifecycle.has_started_execution()
        && requirement_expiry > chrono::Utc::now()
        && evidence_ready(conn, deployment_id).await?
    {
        sqlx::query("UPDATE deployment_approval_requirements SET status = 'SATISFIED', revision = revision + 1, satisfied_at = CURRENT_TIMESTAMP, satisfied_participants = '[]'::jsonb WHERE id = $1 AND status = 'PENDING'")
            .bind(requirement_id)
            .execute(&mut *conn)
            .await?;
        let sequence = next_timeline_sequence(conn, deployment_id, 0).await?;
        crate::audit::bind_audit_metadata(
            sqlx::query(
                "INSERT INTO deployment_audit_events \
                   (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
                    request_id, correlation_id, graphql_operation, source_ip, user_agent) \
                 VALUES ($1, $2, NULL, 'APPROVAL_SATISFIED', jsonb_build_object('requirementId', $3, 'participantIds', jsonb_build_array()), NULL, 0, $4, $5, $6, $7, $8, $9)",
            )
            .bind(Uuid::new_v4())
            .bind(deployment_id)
            .bind(requirement_id.to_string())
            .bind(sequence),
        )
        .execute(&mut *conn)
        .await?;
        touch_projection(conn, deployment_id).await?;
        requirement_status = ApprovalRequirementStatus::Satisfied;
    }
    if requirement_status == ApprovalRequirementStatus::Satisfied
        && requested_or_approved
        && !evidence_ready(conn, deployment_id).await?
        && !waiting_for_evaluation(conn, deployment_id).await?
    {
        block_approval_execution(conn, deployment_id, None).await?;
        return Ok(false);
    }
    if requirement_status == ApprovalRequirementStatus::Satisfied && requested_or_approved {
        if !waiting_for_evaluation(conn, deployment_id).await? {
            sqlx::query("INSERT INTO deployment_approval_handoff_releases (deployment_id) VALUES ($1) ON CONFLICT (deployment_id) DO NOTHING").bind(deployment_id).execute(&mut *conn).await?;
            let pending_execute = sqlx::query("SELECT 1 FROM deployment_outbox_events event WHERE event.deployment_id = $1 AND event.event_type = 'EXECUTE_DEPLOYMENT' AND event.status IN ('PENDING', 'PROCESSING')")
                .bind(deployment_id)
                .fetch_optional(&mut *conn)
                .await?
                .is_some();
            if worker_ready(conn).await? && !pending_execute {
                crate::deployment::writes::enqueue(
                    conn,
                    deployment_id,
                    "EXECUTE_DEPLOYMENT",
                    "SUCCESS",
                )
                .await?;
            }
            let now_pending_execute = sqlx::query("SELECT 1 FROM deployment_outbox_events event WHERE event.deployment_id = $1 AND event.event_type = 'EXECUTE_DEPLOYMENT' AND event.status IN ('PENDING', 'PROCESSING')")
                .bind(deployment_id)
                .fetch_optional(&mut *conn)
                .await?
                .is_some();
            if now_pending_execute {
                sqlx::query(
                    "DELETE FROM deployment_approval_handoff_releases WHERE deployment_id = $1",
                )
                .bind(deployment_id)
                .execute(&mut *conn)
                .await?;
            }
        }
        return Ok(true);
    }
    Ok(requirement_status == ApprovalRequirementStatus::Satisfied
        && evidence_ready(conn, deployment_id).await?)
}

/// Ports `expiredApprovalRequirementDeployments`.
async fn expired_approval_requirement_deployments(
    conn: &mut PgConnection,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT requirement.deployment_id FROM deployment_approval_requirements requirement \
         WHERE requirement.expires_at <= clock_timestamp() AND requirement.status IN ('PENDING', 'SATISFIED') \
         ORDER BY requirement.expires_at ASC, requirement.id ASC LIMIT 50",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(|row| row.get(0)).collect())
}

/// Ports `reconcileExpiredApprovalRequirements`: fetches one page of expired requirements and
/// reconciles each in its own transaction, so one bad row cannot abort the batch. Every call site
/// already relies on `reconcile_pending`'s own conditional-UPDATE race safety, so no advisory lock
/// is taken here — matching the DSQL-compatibility reasoning `reconcile_pending` itself documents.
pub(crate) async fn reconcile_expired_approval_requirements(
    pool: &PgPool,
) -> Result<crate::ApprovalMaintenanceHealth, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let deployments = expired_approval_requirement_deployments(&mut conn).await?;
    drop(conn);
    let mut attempted = 0i64;
    let mut reconciled = 0i64;
    let mut failed = 0i64;
    for deployment_id in deployments {
        attempted += 1;
        let mut tx = pool.begin().await?;
        match reconcile_pending(&mut tx, deployment_id).await {
            Ok(_) => {
                tx.commit().await?;
                reconciled += 1;
            }
            Err(_) => {
                let _ = tx.rollback().await;
                failed += 1;
            }
        }
    }
    Ok(if failed > 0 {
        crate::ApprovalMaintenanceHealth {
            healthy: false,
            failure_code: Some("PARTIAL_RECONCILIATION".to_string()),
            attempted,
            reconciled,
            failed,
        }
    } else {
        crate::ApprovalMaintenanceHealth {
            healthy: true,
            failure_code: None,
            attempted,
            reconciled,
            failed: 0,
        }
    })
}

/// Ports the public `reconcileApprovalExpiry()` wrapper: the `MaintenanceJobs` scheduled task's
/// entry point, publishing directly into the shared `/health` state.
pub async fn reconcile_approval_expiry(pool: &PgPool, state: &crate::ApprovalMaintenanceState) {
    state.set_maintenance(crate::ApprovalMaintenanceHealth::in_progress());
    let health = match reconcile_expired_approval_requirements(pool).await {
        Ok(health) => health,
        Err(error) => crate::ApprovalMaintenanceHealth {
            healthy: false,
            failure_code: Some(super::worker::sql_failure_code(&error)),
            attempted: 0,
            reconciled: 0,
            failed: 0,
        },
    };
    state.set_maintenance(health);
}

/// Ports `approvalProjectArchivePending`.
async fn approval_project_archive_pending(conn: &mut PgConnection) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "SELECT 1 FROM deployment_approval_project_archive_events WHERE processed_at IS NULL",
    )
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

/// Ports `reconcileProjectArchives`: terminalizes every pending/satisfied approval cycle an
/// archived project's boundary now covers. One transaction per archive event (not per candidate
/// row, unlike `reconcile_expired_approval_requirements`): Java takes `FOR UPDATE OF deployment,
/// requirement` locks across the whole candidate page and every write that follows, which requires
/// one open transaction for the event's full batch; a partial failure leaves `processed_at` NULL so
/// the whole event retries next tick, which is safe because every write here is a conditional
/// UPDATE already idempotent against a re-run.
async fn reconcile_project_archives(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let events: Vec<(Uuid, Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, project_id, actor_principal_id FROM deployment_approval_project_archive_events \
         WHERE processed_at IS NULL ORDER BY archived_at ASC, id ASC LIMIT 10",
    )
    .fetch_all(&mut *conn)
    .await?;
    drop(conn);

    for (event_id, project_id, actor) in events {
        let mut tx = pool.begin().await?;
        let candidates: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
            "SELECT requirement.id, deployment.id, requirement.status \
             FROM deployments deployment \
               JOIN deployment_approval_requirements requirement ON requirement.deployment_id = deployment.id \
               JOIN deployment_approval_project_archive_events archive_event \
                 ON archive_event.id = $1 AND archive_event.project_id = deployment.project_id \
             WHERE deployment.project_id = $2 \
               AND ((deployment.project_lifecycle_revision IS NOT NULL AND archive_event.archived_project_revision IS NOT NULL \
                       AND deployment.project_lifecycle_revision <= archive_event.archived_project_revision) \
                   OR ((deployment.project_lifecycle_revision IS NULL OR archive_event.archived_project_revision IS NULL) \
                       AND deployment.requested_at <= archive_event.archived_at)) \
               AND ((deployment.lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED') AND requirement.status = 'PENDING') \
                 OR (deployment.lifecycle_status IN ('APPROVED', 'REQUESTED') AND requirement.status = 'SATISFIED')) \
             ORDER BY deployment.id ASC LIMIT 50 FOR UPDATE OF deployment, requirement",
        )
        .bind(event_id)
        .bind(project_id)
        .fetch_all(&mut *tx)
        .await?;

        for (requirement_id, deployment_id, status) in candidates {
            if rows::requirement_status(status) == ApprovalRequirementStatus::Satisfied {
                block_approval_execution(&mut tx, deployment_id, actor).await?;
                continue;
            }
            let updated = sqlx::query(
                "UPDATE deployment_approval_requirements SET status = 'INVALIDATED', revision = revision + 1, \
                   invalidated_at = CURRENT_TIMESTAMP, invalidation_code = 'PROJECT_ARCHIVED' WHERE id = $1 AND status = 'PENDING'",
            )
            .bind(requirement_id)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() != 1 {
                continue;
            }
            audit(
                &mut tx,
                deployment_id,
                actor,
                "APPROVAL_INVALIDATED",
                json!({"requirementId": requirement_id.to_string(), "code": "PROJECT_ARCHIVED"}),
            )
            .await?;
            sqlx::query(
                "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Project archive terminalized this pending approval cycle.', \
                   observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
            )
            .bind(deployment_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, projection_revision = projection_revision + 1, \
                   updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')",
            )
            .bind(deployment_id)
            .execute(&mut *tx)
            .await?;
            touch_projection(&mut tx, deployment_id).await?;
        }

        let still_pending: (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM deployments deployment \
                 JOIN deployment_approval_requirements requirement ON requirement.deployment_id = deployment.id \
                 JOIN deployment_approval_project_archive_events archive_event \
                   ON archive_event.id = $1 AND archive_event.project_id = deployment.project_id \
               WHERE deployment.project_id = $2 \
                 AND ((deployment.project_lifecycle_revision IS NOT NULL AND archive_event.archived_project_revision IS NOT NULL \
                         AND deployment.project_lifecycle_revision <= archive_event.archived_project_revision) \
                     OR ((deployment.project_lifecycle_revision IS NULL OR archive_event.archived_project_revision IS NULL) \
                         AND deployment.requested_at <= archive_event.archived_at)) \
                 AND ((deployment.lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED') AND requirement.status = 'PENDING') \
                   OR (deployment.lifecycle_status IN ('APPROVED', 'REQUESTED') AND requirement.status = 'SATISFIED')))",
        )
        .bind(event_id)
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        if !still_pending.0 {
            sqlx::query(
                "UPDATE deployment_approval_project_archive_events SET processed_at = CURRENT_TIMESTAMP WHERE id = $1",
            )
            .bind(event_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

/// Ports the public `reconcileApprovalUpgrade()` wrapper. The original also carried a "compatibility
/// backfill" phase for rows a Java migration history accumulated before a unified write path
/// existed; a greenfield rewrite has no such backlog, so only archive reconciliation remains here.
pub async fn reconcile_approval_upgrade(pool: &PgPool, state: &crate::ApprovalMaintenanceState) {
    let outcome = async {
        reconcile_project_archives(pool).await?;
        let mut conn = pool.acquire().await?;
        approval_project_archive_pending(&mut conn).await
    }
    .await;
    let health = match outcome {
        Ok(archive_pending) => crate::ApprovalMaintenanceHealth {
            healthy: !archive_pending,
            failure_code: if archive_pending {
                Some("ARCHIVE_RECONCILIATION_PENDING".to_string())
            } else {
                None
            },
            attempted: 0,
            reconciled: 0,
            failed: 0,
        },
        Err(error) => crate::ApprovalMaintenanceHealth {
            healthy: false,
            failure_code: Some(format!(
                "ARCHIVE_RECONCILIATION_{}",
                super::worker::sql_failure_code(&error)
            )),
            attempted: 0,
            reconciled: 0,
            failed: 0,
        },
    };
    state.set_upgrade(health);
}

/// Ports `compatibleApprovalHandoffDeployments`. `worker_ready` is read once by the caller (not
/// re-evaluated per row): every row this selects already re-checks worker readiness inside
/// `automatic_approval_handoff` for the `required_approvers > 0` case, but that later re-check
/// alone would under-filter the candidate set the `required_approvers = 0 OR $1` clause narrows.
async fn compatible_approval_handoff_deployments(
    conn: &mut PgConnection,
    worker_ready: bool,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT release.deployment_id \
         FROM deployment_approval_handoff_releases release \
           JOIN deployments deployment ON deployment.id = release.deployment_id \
           JOIN deployment_approval_requirements requirement ON requirement.deployment_id = deployment.id \
         WHERE (deployment.lifecycle_status = 'APPROVED' AND requirement.status = 'SATISFIED' \
                OR deployment.lifecycle_status = 'REQUESTED' AND requirement.required_approvers = 0 \
                   AND requirement.status IN ('PENDING', 'SATISFIED')) \
           AND (requirement.required_approvers = 0 OR $1) \
           AND NOT EXISTS (SELECT 1 FROM deployment_approval_project_archive_events archive_event \
                           WHERE archive_event.project_id = deployment.project_id \
                             AND ((deployment.project_lifecycle_revision IS NOT NULL AND archive_event.archived_project_revision IS NOT NULL \
                                     AND deployment.project_lifecycle_revision <= archive_event.archived_project_revision) \
                                 OR ((deployment.project_lifecycle_revision IS NULL OR archive_event.archived_project_revision IS NULL) \
                                     AND deployment.requested_at <= archive_event.archived_at))) \
           AND requirement.expires_at > clock_timestamp() \
           AND NOT EXISTS (SELECT 1 FROM deployment_outbox_events event \
                           WHERE event.deployment_id = deployment.id AND event.event_type = 'EXECUTE_DEPLOYMENT' \
                             AND event.status IN ('PENDING', 'PROCESSING')) \
         ORDER BY release.created_at ASC, release.deployment_id ASC LIMIT 50",
    )
    .bind(worker_ready)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(|row| row.get(0)).collect())
}

async fn release_compatible_approval_handoffs_inner(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let ready = worker_ready(&mut conn).await?;
    let deployments = compatible_approval_handoff_deployments(&mut conn, ready).await?;
    drop(conn);
    let mut all_succeeded = true;
    for deployment_id in deployments {
        let mut tx = pool.begin().await?;
        match automatic_approval_handoff(&mut tx, deployment_id).await {
            Ok(_) => tx.commit().await?,
            Err(_) => {
                let _ = tx.rollback().await;
                all_succeeded = false;
            }
        }
    }
    Ok(all_succeeded)
}

/// Ports `releaseCompatibleApprovalHandoffs`: `true` only if the candidate page was read AND every
/// row's handoff attempt succeeded, matching every failure mode there folding into the same
/// `APPROVAL_MAINTENANCE_FAILED` heartbeat report in Java's `recordWorkerHeartbeat`.
pub(crate) async fn release_compatible_approval_handoffs(pool: &PgPool) -> bool {
    match release_compatible_approval_handoffs_inner(pool).await {
        Ok(all_succeeded) => all_succeeded,
        Err(error) => {
            tracing::warn!(code = %super::worker::sql_failure_code(&error), "approval handoff maintenance failed");
            false
        }
    }
}

/// Ports `approvalMaintenanceFailed`: whether this worker's own currently-stored heartbeat row
/// already reports a sticky, unrecovered maintenance failure.
pub(crate) async fn approval_maintenance_failed(
    pool: &PgPool,
    worker: &str,
) -> Result<bool, sqlx::Error> {
    let worker = worker.trim();
    let row: Option<(bool,)> = sqlx::query_as(
        "SELECT state = 'DEGRADED' AND failure_code = 'APPROVAL_MAINTENANCE_FAILED' \
         FROM deployment_worker_heartbeats WHERE worker_id = $1",
    )
    .bind(worker)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| row.0).unwrap_or(false))
}
