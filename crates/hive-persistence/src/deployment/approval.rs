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
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

/// The 0-rows-affected branch below mirrors Java's synthetic `throw new SQLException(..., "40001")`
/// with a plain `RecordNotFound` instead of a fabricated SQLSTATE: every call site already holds the
/// row's lock from a `FOR UPDATE` read moments earlier in the same transaction, so DSQL's real
/// commit-time OCC validation — not this defensive check — is what actually catches a genuine
/// concurrent race (as an authentic SQLSTATE 40001 from `tx.commit()`). This check exists only for
/// defense in depth, matching Java's own belt-and-suspenders style.
pub async fn transition_requirement(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
    status: ApprovalRequirementStatus,
    code: Option<&str>,
    participants: &[Uuid],
) -> Result<(), DbErr> {
    let participant_strings: Vec<String> = participants.iter().map(ToString::to_string).collect();
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployment_approval_requirements SET status = $1, revision = revision + 1, \
           satisfied_at = CASE WHEN $1 = 'SATISFIED' THEN CURRENT_TIMESTAMP ELSE NULL END, \
           rejected_at = CASE WHEN $1 = 'REJECTED' THEN CURRENT_TIMESTAMP ELSE NULL END, \
           invalidated_at = CASE WHEN $1 = 'INVALIDATED' THEN CURRENT_TIMESTAMP ELSE NULL END, \
           invalidation_code = CASE WHEN $1 = 'INVALIDATED' THEN $2 ELSE NULL END, \
           satisfied_participants = $3::jsonb \
         WHERE id = $4 AND status = 'PENDING' AND ($1 NOT IN ('SATISFIED', 'REJECTED') OR expires_at > clock_timestamp())",
        [
            status.as_str().into(),
            code.into(),
            json_array(&participant_strings).into(),
            requirement_id.into(),
        ],
    );
    let result = db.execute_raw(statement).await?;
    if result.rows_affected() != 1 {
        return Err(DbErr::RecordNotFound(format!(
            "no pending approval requirement with id {requirement_id}"
        )));
    }
    Ok(())
}

pub async fn cancel_rejected_deployment(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    let health_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Approval rejection terminalized this local deployment.', \
             observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
        [deployment_id.into()],
    );
    db.execute_raw(health_statement).await?;
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')",
        [deployment_id.into()],
    );
    let result = db.execute_raw(statement).await?;
    if result.rows_affected() != 1 {
        return Err(DbErr::RecordNotFound(format!(
            "no cancelable deployment with id {deployment_id}"
        )));
    }
    Ok(())
}

pub async fn approve_deployment_for_execution(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployments SET lifecycle_status = 'APPROVED', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')",
        [deployment_id.into()],
    );
    let result = db.execute_raw(statement).await?;
    if result.rows_affected() != 1 {
        return Err(DbErr::RecordNotFound(format!(
            "no approvable deployment with id {deployment_id}"
        )));
    }
    Box::pin(automatic_approval_handoff(db, deployment_id)).await?;
    Ok(())
}

pub async fn invalidate_pending_requirement(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    code: &str,
    actor: Option<Uuid>,
) -> Result<(), DbErr> {
    let Some(raw) = raw_requirement_by_deployment(db, deployment_id, true).await? else {
        return Ok(());
    };
    if raw.status != ApprovalRequirementStatus::Pending {
        return Ok(());
    }
    transition_requirement(
        db,
        raw.id,
        ApprovalRequirementStatus::Invalidated,
        Some(code),
        &[],
    )
    .await?;
    audit(
        db,
        deployment_id,
        actor,
        "APPROVAL_INVALIDATED",
        json!({"requirementId": raw.id.to_string(), "code": code}),
    )
    .await
}

pub async fn requirement_expired(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM deployment_approval_requirements WHERE id = $1 AND expires_at <= clock_timestamp()",
        [requirement_id.into()],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
}

pub async fn deployment_archive_boundary(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM deployments deployment \
           JOIN deployment_approval_project_archive_events event ON event.project_id = deployment.project_id \
         WHERE deployment.id = $1 \
           AND ((deployment.project_lifecycle_revision IS NOT NULL AND event.archived_project_revision IS NOT NULL \
                   AND deployment.project_lifecycle_revision <= event.archived_project_revision) \
               OR ((deployment.project_lifecycle_revision IS NULL OR event.archived_project_revision IS NULL) \
                   AND deployment.requested_at <= event.archived_at))",
        [deployment_id.into()],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
}

pub async fn worker_ready(db: &impl ConnectionTrait) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM deployment_worker_heartbeats heartbeat WHERE heartbeat.approval_execution_compatible AND heartbeat.state = 'READY' AND heartbeat.observed_at > clock_timestamp() - INTERVAL '15 seconds'",
        [],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
}

pub async fn compatible_approval_worker(
    db: &impl ConnectionTrait,
    worker: &str,
) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM deployment_worker_heartbeats heartbeat WHERE heartbeat.worker_id = $1 AND heartbeat.approval_execution_compatible \
           AND heartbeat.state = 'READY' AND heartbeat.observed_at > clock_timestamp() - INTERVAL '15 seconds'",
        [worker.into()],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
}

pub async fn approved_approval_handoff(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM deployment_approval_requirements requirement JOIN deployments deployment ON deployment.id = requirement.deployment_id WHERE requirement.deployment_id = $1 AND requirement.status = 'SATISFIED' AND deployment.lifecycle_status = 'APPROVED'",
        [deployment_id.into()],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
}

/// Returns a raced M13 claim to the compatible-worker gate without consuming retry capacity.
pub async fn defer_incompatible_approval_handoff(
    db: &impl ConnectionTrait,
    event_id: Uuid,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployment_outbox_events SET status = 'PENDING', available_at = CURRENT_TIMESTAMP + INTERVAL '5 seconds' \
             + get_byte(uuid_send(id), 0) * INTERVAL '1 millisecond', \
           claimed_at = NULL, claimed_by = NULL, attempt_count = GREATEST(attempt_count - 1, 0), \
           last_error = 'An incompatible worker cannot execute an approved handoff.' \
         WHERE id = $1 AND status = 'PROCESSING'",
        [event_id.into()],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

/// Returns a retained event to the queue until maintenance establishes its frozen handoff.
pub async fn defer_pending_approval_handoff(
    db: &impl ConnectionTrait,
    event_id: Uuid,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployment_outbox_events SET status = 'PENDING', available_at = CURRENT_TIMESTAMP + INTERVAL '1 second' \
             + get_byte(uuid_send(id), 0) * INTERVAL '1 millisecond', \
           claimed_at = NULL, claimed_by = NULL, attempt_count = GREATEST(attempt_count - 1, 0), \
           last_error = 'The approval handoff is not ready for execution.' \
         WHERE id = $1 AND status = 'PROCESSING'",
        [event_id.into()],
    );
    db.execute_raw(statement).await?;
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

fn policy_row(row: &sea_orm::QueryResult) -> Result<PolicyRow, DbErr> {
    let required_evidence_json: String = row.try_get_by("required_evidence")?;
    Ok(PolicyRow {
        binding_digest: row.try_get_by("binding_digest")?,
        agent_version_id: row.try_get_by("agent_version_id")?,
        environment_definition_version_id: row.try_get_by("environment_definition_version_id")?,
        target_digest: row.try_get_by("target_digest")?,
        plan_digest: row.try_get_by("plan_digest")?,
        package_digest: row.try_get_by("package_digest")?,
        required_evidence: crate::sql::parse_string_array(&required_evidence_json),
    })
}

async fn evidence_issue_policy_row(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<PolicyRow>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT policy.binding_digest, policy.agent_version_id, policy.environment_definition_version_id, \
             policy.target_digest, policy.plan_digest, policy.package_digest, policy.required_evidence::text AS required_evidence \
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
        [deployment_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(policy_row(&row)?)),
        None => Ok(None),
    }
}

async fn waiting_policy_row(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<PolicyRow>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT policy.binding_digest, policy.agent_version_id, policy.environment_definition_version_id, \
             policy.target_digest, policy.plan_digest, policy.package_digest, policy.required_evidence::text AS required_evidence \
         FROM deployment_policy_snapshots policy JOIN deployments deployment ON deployment.id = policy.deployment_id \
         WHERE policy.deployment_id = $1",
        [deployment_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(policy_row(&row)?)),
        None => Ok(None),
    }
}

async fn valid_evidence(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    policy: &PolicyRow,
    kind: &str,
) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT EXISTS (SELECT 1 FROM deployment_evidence_snapshots evidence \
          WHERE evidence.deployment_id = $1 AND evidence.evidence_kind = $2 \
            AND evidence.binding_digest = $3 AND evidence.agent_version_id = $4 \
            AND evidence.environment_definition_version_id = $5 AND evidence.target_digest = $6 \
            AND evidence.plan_digest = $7 AND evidence.package_digest = $8 \
            AND (evidence.expires_at IS NULL OR evidence.expires_at > clock_timestamp()) \
            AND NOT EXISTS (SELECT 1 FROM deployment_evidence_invalidations invalidation WHERE invalidation.evidence_snapshot_id = evidence.id)) AS exists_flag",
        [
            deployment_id.into(),
            kind.into(),
            policy.binding_digest.clone().into(),
            policy.agent_version_id.into(),
            policy.environment_definition_version_id.into(),
            policy.target_digest.clone().into(),
            policy.plan_digest.clone().into(),
            policy.package_digest.clone().into(),
        ],
    );
    db.query_one_raw(statement)
        .await?
        .expect("EXISTS(...) always returns exactly one row")
        .try_get_by("exists_flag")
}

async fn observed_evidence(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    kind: &str,
) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT EXISTS (SELECT 1 FROM deployment_evidence_snapshots evidence WHERE evidence.deployment_id = $1 AND evidence.evidence_kind = $2) AS exists_flag",
        [deployment_id.into(), kind.into()],
    );
    db.query_one_raw(statement)
        .await?
        .expect("EXISTS(...) always returns exactly one row")
        .try_get_by("exists_flag")
}

async fn expired_evidence(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    policy: &PolicyRow,
    kind: &str,
) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT EXISTS (SELECT 1 FROM deployment_evidence_snapshots evidence \
          WHERE evidence.deployment_id = $1 AND evidence.evidence_kind = $2 \
            AND evidence.binding_digest = $3 AND evidence.agent_version_id = $4 \
            AND evidence.environment_definition_version_id = $5 AND evidence.target_digest = $6 \
            AND evidence.plan_digest = $7 AND evidence.package_digest = $8 \
            AND evidence.expires_at <= clock_timestamp() \
            AND NOT EXISTS (SELECT 1 FROM deployment_evidence_invalidations invalidation WHERE invalidation.evidence_snapshot_id = evidence.id)) AS exists_flag",
        [
            deployment_id.into(),
            kind.into(),
            policy.binding_digest.clone().into(),
            policy.agent_version_id.into(),
            policy.environment_definition_version_id.into(),
            policy.target_digest.clone().into(),
            policy.plan_digest.clone().into(),
            policy.package_digest.clone().into(),
        ],
    );
    db.query_one_raw(statement)
        .await?
        .expect("EXISTS(...) always returns exactly one row")
        .try_get_by("exists_flag")
}

/// Ports `PostgresDeploymentApprovalEvidenceIssue.evidenceIssue`. Returns
/// `APPROVAL_EVIDENCE_MISMATCH`/`APPROVAL_EVIDENCE_MISSING`/`APPROVAL_EVIDENCE_EXPIRED`,
/// or `None` when every required evidence kind is valid.
pub async fn approval_evidence_issue(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<String>, DbErr> {
    let Some(policy) = evidence_issue_policy_row(db, deployment_id).await? else {
        return Ok(Some("APPROVAL_EVIDENCE_MISMATCH".to_string()));
    };
    for kind in &policy.required_evidence {
        if valid_evidence(db, deployment_id, &policy, kind).await? {
            continue;
        }
        if !observed_evidence(db, deployment_id, kind).await? {
            return Ok(Some("APPROVAL_EVIDENCE_MISSING".to_string()));
        }
        if expired_evidence(db, deployment_id, &policy, kind).await? {
            return Ok(Some("APPROVAL_EVIDENCE_EXPIRED".to_string()));
        }
        return Ok(Some("APPROVAL_EVIDENCE_MISMATCH".to_string()));
    }
    Ok(None)
}

/// Ports `PostgresDeploymentApprovalEvidenceIssue.waitingForEvaluation`.
pub async fn waiting_for_evaluation(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let Some(policy) = waiting_policy_row(db, deployment_id).await? else {
        return Ok(false);
    };
    if !policy
        .required_evidence
        .iter()
        .any(|kind| kind == "EVALUATION_PASSED")
    {
        return Ok(false);
    }
    if approval_evidence_issue(db, deployment_id).await?.as_deref()
        != Some("APPROVAL_EVIDENCE_MISSING")
    {
        return Ok(false);
    }
    if observed_evidence(db, deployment_id, "EVALUATION_PASSED").await? {
        return Ok(false);
    }
    for kind in &policy.required_evidence {
        if kind == "EVALUATION_PASSED" {
            continue;
        }
        if !valid_evidence(db, deployment_id, &policy, kind).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub async fn evidence_ready(db: &impl ConnectionTrait, deployment_id: Uuid) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
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
        [deployment_id.into()],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
}

/// Java port of `deployment_approval_reconcile_pending()`'s final redefinition (V017).
pub async fn reconcile_pending(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let lifecycle_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT lifecycle_status FROM deployments WHERE id = $1 FOR UPDATE",
        [deployment_id.into()],
    );
    let Some(lifecycle_row) = db.query_one_raw(lifecycle_statement).await? else {
        return Ok(false);
    };
    let lifecycle = rows::lifecycle_status(lifecycle_row.try_get_by("lifecycle_status")?);

    let requirement_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id FROM deployment_approval_requirements WHERE deployment_id = $1 AND status = 'PENDING' FOR UPDATE",
        [deployment_id.into()],
    );
    let Some(requirement_row) = db.query_one_raw(requirement_statement).await? else {
        let satisfied_expired_statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT 1 FROM deployment_approval_requirements requirement WHERE requirement.deployment_id = $1 AND requirement.status = 'SATISFIED' AND requirement.expires_at <= clock_timestamp()",
            [deployment_id.into()],
        );
        let satisfied_expired = db
            .query_one_raw(satisfied_expired_statement)
            .await?
            .is_some();
        if satisfied_expired {
            block_approval_execution(db, deployment_id, None).await?;
            return Ok(true);
        }
        return Ok(false);
    };
    let requirement_id: Uuid = requirement_row.try_get_by("id")?;

    let (action, issue): (&str, Option<String>) = if lifecycle.has_started_execution() {
        (
            "APPROVAL_INVALIDATED",
            Some("TERMINAL_LIFECYCLE".to_string()),
        )
    } else if requirement_expired(db, requirement_id).await? {
        (
            "APPROVAL_EXPIRED",
            Some("APPROVAL_REQUIREMENT_EXPIRED".to_string()),
        )
    } else {
        let issue = approval_evidence_issue(db, deployment_id).await?;
        if issue.is_none()
            || (issue.as_deref() == Some("APPROVAL_EVIDENCE_MISSING")
                && waiting_for_evaluation(db, deployment_id).await?)
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
    transition_requirement(db, requirement_id, status, issue.as_deref(), &[]).await?;
    let sequence = next_timeline_sequence(db, deployment_id, 0).await?;
    let mut audit_values: Vec<sea_orm::Value> = vec![
        Uuid::new_v4().into(),
        deployment_id.into(),
        None::<Uuid>.into(),
        action.into(),
        requirement_id.to_string().into(),
        issue.clone().into(),
        sequence.into(),
    ];
    audit_values.extend(crate::audit::context::audit_metadata_values());
    let audit_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_audit_events \
           (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
            request_id, correlation_id, graphql_operation, source_ip, user_agent) \
         VALUES ($1, $2, $3, $4, jsonb_build_object('requirementId', $5, 'code', $6), NULL, 0, $7, $8, $9, $10, $11, $12)",
        audit_values,
    );
    db.execute_raw(audit_statement).await?;
    touch_projection(db, deployment_id).await?;
    if matches!(
        lifecycle,
        DeploymentLifecycleStatus::AwaitingApproval | DeploymentLifecycleStatus::Requested
    ) {
        let health_statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Approval could not complete with the frozen requirement facts.', \
                 observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
            [deployment_id.into()],
        );
        db.execute_raw(health_statement).await?;
        let status_statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, projection_revision = projection_revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')",
            [deployment_id.into()],
        );
        db.execute_raw(status_statement).await?;
    }
    Ok(true)
}

/// Java port of `deployment_approval_block_invalid_handoff()`'s final redefinition (V033). `actor` is
/// `None` except when `reconcileProjectArchives` (RTP-APPROVAL's job, not ported here) calls the
/// actor-carrying overload with the archive event's own actor.
pub async fn block_approval_execution(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    actor: Option<Uuid>,
) -> Result<bool, DbErr> {
    let satisfied_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM deployment_approval_requirements requirement JOIN deployments deployment ON deployment.id = requirement.deployment_id \
         WHERE requirement.deployment_id = $1 AND requirement.status = 'SATISFIED' AND deployment.lifecycle_status IN ('REQUESTED', 'APPROVED')",
        [deployment_id.into()],
    );
    let satisfied_and_pending = db.query_one_raw(satisfied_statement).await?.is_some();
    if !satisfied_and_pending {
        return Ok(false);
    }
    let archive_boundary = deployment_archive_boundary(db, deployment_id).await?;
    let project_active_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT project.lifecycle_status = 'ACTIVE' AS project_active FROM deployments deployment JOIN projects project ON project.id = deployment.project_id WHERE deployment.id = $1",
        [deployment_id.into()],
    );
    let project_active = db
        .query_one_raw(project_active_statement)
        .await?
        .map(|row| row.try_get_by::<bool, _>("project_active"))
        .transpose()?
        .unwrap_or(false);
    if project_active
        && !archive_boundary
        && evidence_ready(db, deployment_id).await?
        && !{
            let expiry_statement = Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT 1 FROM deployment_approval_requirements WHERE deployment_id = $1 AND expires_at <= clock_timestamp()",
                [deployment_id.into()],
            );
            db.query_one_raw(expiry_statement).await?.is_some()
        }
    {
        return Ok(false);
    }
    let evidence_issue = approval_evidence_issue(db, deployment_id).await?;
    let block_code = if archive_boundary {
        "PROJECT_ARCHIVED".to_string()
    } else if project_active {
        evidence_issue.unwrap_or_else(|| "APPROVAL_EVIDENCE_NO_LONGER_VALID".to_string())
    } else {
        "PROJECT_NOT_ACTIVE".to_string()
    };
    let health_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Execution stopped because frozen approval requirements were no longer executable.', \
             observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
        [deployment_id.into()],
    );
    db.execute_raw(health_statement).await?;
    let update_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, projection_revision = projection_revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('REQUESTED', 'APPROVED')",
        [deployment_id.into()],
    );
    let updated = db.execute_raw(update_statement).await?;
    if updated.rows_affected() == 0 {
        return Ok(false);
    }
    let delete_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "DELETE FROM deployment_approval_handoff_releases WHERE deployment_id = $1",
        [deployment_id.into()],
    );
    db.execute_raw(delete_statement).await?;
    let sequence = next_timeline_sequence(db, deployment_id, 0).await?;
    let mut audit_values: Vec<sea_orm::Value> = vec![
        Uuid::new_v4().into(),
        deployment_id.into(),
        actor.into(),
        block_code.clone().into(),
        sequence.into(),
    ];
    audit_values.extend(crate::audit::context::audit_metadata_values());
    let audit_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_audit_events \
           (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
            request_id, correlation_id, graphql_operation, source_ip, user_agent) \
         VALUES ($1, $2, $3, 'APPROVAL_EXECUTION_BLOCKED', jsonb_build_object('code', $4), NULL, 0, $5, $6, $7, $8, $9, $10)",
        audit_values,
    );
    db.execute_raw(audit_statement).await?;
    touch_projection(db, deployment_id).await?;
    Ok(true)
}

/// Java port of `deployment_approval_execution_eligible()`'s final redefinition (V033).
pub async fn approval_execution_eligible(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
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
        [deployment_id.into()],
    );
    let Some(row) = db.query_one_raw(statement).await? else {
        return Ok(false);
    };
    let required: i32 = row.try_get_by("required_approvers")?;
    let participant_count: i32 = row.try_get_by("participant_count")?;
    let distinct_count: i64 = row.try_get_by("distinct_count")?;
    let participants_valid: bool = row.try_get_by("participants_valid")?;
    if participant_count != required || distinct_count != required as i64 || !participants_valid {
        return Ok(false);
    }
    if deployment_archive_boundary(db, deployment_id).await? {
        return Ok(false);
    }
    evidence_ready(db, deployment_id).await
}

/// Java port of `deployment_approval_ensure_requirement()`'s final redefinition (V032).
pub async fn ensure_requirement(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let archived = deployment_archive_boundary(db, deployment_id).await?;
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
    let insert_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        &sql,
        [archived.into(), Uuid::new_v4().into(), deployment_id.into()],
    );
    if let Some(row) = db.query_one_raw(insert_statement).await? {
        let requirement_id: Uuid = row.try_get_by("id")?;
        let status = rows::requirement_status(row.try_get_by("status")?);
        if status == ApprovalRequirementStatus::Invalidated {
            if archived {
                invalidate_archived_approval_requirement(db, deployment_id, requirement_id).await?;
            } else {
                record_terminal_invalidation(db, deployment_id, requirement_id).await?;
            }
        }
    }
    let exists_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM deployment_approval_requirements WHERE deployment_id = $1",
        [deployment_id.into()],
    );
    Ok(db.query_one_raw(exists_statement).await?.is_some())
}

/// Ported alongside `ensure_requirement` as its archived-at-creation branch (V032's final
/// `deployment_approval_ensure_requirement()` body).
async fn invalidate_archived_approval_requirement(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    requirement_id: Uuid,
) -> Result<(), DbErr> {
    let archive_actor_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT event.actor_principal_id \
         FROM deployment_approval_project_archive_events event JOIN deployments deployment ON deployment.project_id = event.project_id \
         WHERE deployment.id = $1 \
           AND ((deployment.project_lifecycle_revision IS NOT NULL AND event.archived_project_revision IS NOT NULL \
                 AND deployment.project_lifecycle_revision <= event.archived_project_revision) \
             OR ((deployment.project_lifecycle_revision IS NULL OR event.archived_project_revision IS NULL) \
                 AND deployment.requested_at <= event.archived_at)) \
         ORDER BY event.archived_project_revision DESC NULLS LAST, event.archived_at DESC, event.id DESC LIMIT 1",
        [deployment_id.into()],
    );
    let archive_actor: Option<Uuid> = db
        .query_one_raw(archive_actor_statement)
        .await?
        .map(|row| row.try_get_by("actor_principal_id"))
        .transpose()?
        .flatten();
    let sequence = next_timeline_sequence(db, deployment_id, 0).await?;
    let mut audit_values: Vec<sea_orm::Value> = vec![
        Uuid::new_v4().into(),
        deployment_id.into(),
        archive_actor.into(),
        requirement_id.to_string().into(),
        sequence.into(),
    ];
    audit_values.extend(crate::audit::context::audit_metadata_values());
    let audit_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_audit_events \
           (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
            request_id, correlation_id, graphql_operation, source_ip, user_agent) \
         VALUES ($1, $2, $3, 'APPROVAL_INVALIDATED', jsonb_build_object('requirementId', $4, 'code', 'PROJECT_ARCHIVED'), NULL, 0, $5, $6, $7, $8, $9, $10)",
        audit_values,
    );
    db.execute_raw(audit_statement).await?;
    let health_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Project archive terminalized this delayed approval cycle.', observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
        [deployment_id.into()],
    );
    db.execute_raw(health_statement).await?;
    let status_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, projection_revision = projection_revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')",
        [deployment_id.into()],
    );
    db.execute_raw(status_statement).await?;
    let delete_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "DELETE FROM deployment_approval_handoff_releases WHERE deployment_id = $1",
        [deployment_id.into()],
    );
    db.execute_raw(delete_statement).await?;
    touch_projection(db, deployment_id).await
}

/// Java port of `deployment_approval_record_terminal_invalidation()`'s final redefinition (V018),
/// `ensure_requirement`'s non-archived INVALIDATED branch.
async fn record_terminal_invalidation(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    requirement_id: Uuid,
) -> Result<(), DbErr> {
    let sequence = next_timeline_sequence(db, deployment_id, 0).await?;
    let mut audit_values: Vec<sea_orm::Value> = vec![
        Uuid::new_v4().into(),
        deployment_id.into(),
        requirement_id.to_string().into(),
        sequence.into(),
    ];
    audit_values.extend(crate::audit::context::audit_metadata_values());
    let audit_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_audit_events \
           (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
            request_id, correlation_id, graphql_operation, source_ip, user_agent) \
         VALUES ($1, $2, NULL, 'APPROVAL_INVALIDATED', jsonb_build_object('requirementId', $3, 'code', 'TERMINAL_LIFECYCLE'), NULL, 0, $4, $5, $6, $7, $8, $9)",
        audit_values,
    );
    db.execute_raw(audit_statement).await?;
    touch_projection(db, deployment_id).await
}

/// The migration-owned handoff atomically validates evidence, satisfies a zero-approver rule, and
/// queues local execution. Java port of `deployment_approval_automatic_handoff()`'s final
/// redefinition (V024), called with its `enqueue_execution` default (`TRUE`) — the only value any
/// caller in this port's scope needs.
pub async fn automatic_approval_handoff(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    if !ensure_requirement(db, deployment_id).await? {
        return Ok(false);
    }
    if deployment_archive_boundary(db, deployment_id).await? {
        return Ok(false);
    }
    reconcile_pending(db, deployment_id).await?;

    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT requirement.id, requirement.status, requirement.required_approvers, deployment.lifecycle_status, requirement.expires_at \
         FROM deployment_approval_requirements requirement JOIN deployments deployment ON deployment.id = requirement.deployment_id \
         WHERE requirement.deployment_id = $1 FOR UPDATE OF requirement, deployment",
        [deployment_id.into()],
    );
    let Some(row) = db.query_one_raw(statement).await? else {
        return Ok(false);
    };
    let requirement_id: Uuid = row.try_get_by("id")?;
    let mut requirement_status = rows::requirement_status(row.try_get_by("status")?);
    let required_approvers: i32 = row.try_get_by("required_approvers")?;
    let lifecycle = rows::lifecycle_status(row.try_get_by("lifecycle_status")?);
    let requirement_expiry: chrono::DateTime<chrono::Utc> = row.try_get_by("expires_at")?;

    let requested_or_approved = lifecycle.awaits_execution();
    if requirement_status == ApprovalRequirementStatus::Satisfied
        && requested_or_approved
        && requirement_expiry <= chrono::Utc::now()
    {
        block_approval_execution(db, deployment_id, None).await?;
        return Ok(false);
    }
    if requirement_status == ApprovalRequirementStatus::Pending
        && required_approvers == 0
        && !lifecycle.has_started_execution()
        && requirement_expiry > chrono::Utc::now()
        && evidence_ready(db, deployment_id).await?
    {
        let satisfy_statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "UPDATE deployment_approval_requirements SET status = 'SATISFIED', revision = revision + 1, satisfied_at = CURRENT_TIMESTAMP, satisfied_participants = '[]'::jsonb WHERE id = $1 AND status = 'PENDING'",
            [requirement_id.into()],
        );
        db.execute_raw(satisfy_statement).await?;
        let sequence = next_timeline_sequence(db, deployment_id, 0).await?;
        let mut audit_values: Vec<sea_orm::Value> = vec![
            Uuid::new_v4().into(),
            deployment_id.into(),
            requirement_id.to_string().into(),
            sequence.into(),
        ];
        audit_values.extend(crate::audit::context::audit_metadata_values());
        let audit_statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO deployment_audit_events \
               (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
                request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, NULL, 'APPROVAL_SATISFIED', jsonb_build_object('requirementId', $3, 'participantIds', jsonb_build_array()), NULL, 0, $4, $5, $6, $7, $8, $9)",
            audit_values,
        );
        db.execute_raw(audit_statement).await?;
        touch_projection(db, deployment_id).await?;
        requirement_status = ApprovalRequirementStatus::Satisfied;
    }
    if requirement_status == ApprovalRequirementStatus::Satisfied
        && requested_or_approved
        && !evidence_ready(db, deployment_id).await?
        && !waiting_for_evaluation(db, deployment_id).await?
    {
        block_approval_execution(db, deployment_id, None).await?;
        return Ok(false);
    }
    if requirement_status == ApprovalRequirementStatus::Satisfied && requested_or_approved {
        if !waiting_for_evaluation(db, deployment_id).await? {
            let release_statement = Statement::from_sql_and_values(
                db.get_database_backend(),
                "INSERT INTO deployment_approval_handoff_releases (deployment_id) VALUES ($1) ON CONFLICT (deployment_id) DO NOTHING",
                [deployment_id.into()],
            );
            db.execute_raw(release_statement).await?;
            let pending_statement = Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT 1 FROM deployment_outbox_events event WHERE event.deployment_id = $1 AND event.event_type = 'EXECUTE_DEPLOYMENT' AND event.status IN ('PENDING', 'PROCESSING')",
                [deployment_id.into()],
            );
            let pending_execute = db.query_one_raw(pending_statement).await?.is_some();
            if worker_ready(db).await? && !pending_execute {
                crate::deployment::writes::enqueue(
                    db,
                    deployment_id,
                    "EXECUTE_DEPLOYMENT",
                    "SUCCESS",
                )
                .await?;
            }
            let now_pending_statement = Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT 1 FROM deployment_outbox_events event WHERE event.deployment_id = $1 AND event.event_type = 'EXECUTE_DEPLOYMENT' AND event.status IN ('PENDING', 'PROCESSING')",
                [deployment_id.into()],
            );
            let now_pending_execute = db.query_one_raw(now_pending_statement).await?.is_some();
            if now_pending_execute {
                let delete_statement = Statement::from_sql_and_values(
                    db.get_database_backend(),
                    "DELETE FROM deployment_approval_handoff_releases WHERE deployment_id = $1",
                    [deployment_id.into()],
                );
                db.execute_raw(delete_statement).await?;
            }
        }
        return Ok(true);
    }
    Ok(requirement_status == ApprovalRequirementStatus::Satisfied
        && evidence_ready(db, deployment_id).await?)
}

/// Ports `expiredApprovalRequirementDeployments`.
async fn expired_approval_requirement_deployments(
    db: &impl ConnectionTrait,
) -> Result<Vec<Uuid>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT requirement.deployment_id FROM deployment_approval_requirements requirement \
         WHERE requirement.expires_at <= clock_timestamp() AND requirement.status IN ('PENDING', 'SATISFIED') \
         ORDER BY requirement.expires_at ASC, requirement.id ASC LIMIT 50",
        [],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values = Vec::with_capacity(rows_found.len());
    for row in rows_found {
        values.push(row.try_get_by::<Uuid, _>("deployment_id")?);
    }
    Ok(values)
}

/// Ports `reconcileExpiredApprovalRequirements`: fetches one page of expired requirements and
/// reconciles each in its own transaction, so one bad row cannot abort the batch. Every call site
/// already relies on `reconcile_pending`'s own conditional-UPDATE race safety, so no advisory lock
/// is taken here — matching the DSQL-compatibility reasoning `reconcile_pending` itself documents.
pub(crate) async fn reconcile_expired_approval_requirements(
    db: &DatabaseConnection,
) -> Result<crate::ApprovalMaintenanceHealth, DbErr> {
    let deployments = expired_approval_requirement_deployments(db).await?;
    let mut attempted = 0i64;
    let mut reconciled = 0i64;
    let mut failed = 0i64;
    for deployment_id in deployments {
        attempted += 1;
        let txn = db.begin().await?;
        match reconcile_pending(&txn, deployment_id).await {
            Ok(_) => {
                txn.commit().await?;
                reconciled += 1;
            }
            Err(_) => {
                let _ = txn.rollback().await;
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
pub async fn reconcile_approval_expiry(
    db: &DatabaseConnection,
    state: &crate::ApprovalMaintenanceState,
) {
    state.set_maintenance(crate::ApprovalMaintenanceHealth::in_progress());
    let health = match reconcile_expired_approval_requirements(db).await {
        Ok(health) => health,
        Err(error) => crate::ApprovalMaintenanceHealth {
            healthy: false,
            failure_code: Some(super::worker::db_failure_code(&error)),
            attempted: 0,
            reconciled: 0,
            failed: 0,
        },
    };
    state.set_maintenance(health);
}

/// Ports `approvalProjectArchivePending`.
async fn approval_project_archive_pending(db: &impl ConnectionTrait) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM deployment_approval_project_archive_events WHERE processed_at IS NULL",
        [],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
}

/// Ports `reconcileProjectArchives`: terminalizes every pending/satisfied approval cycle an
/// archived project's boundary now covers. One transaction per archive event (not per candidate
/// row, unlike `reconcile_expired_approval_requirements`): Java takes `FOR UPDATE OF deployment,
/// requirement` locks across the whole candidate page and every write that follows, which requires
/// one open transaction for the event's full batch; a partial failure leaves `processed_at` NULL so
/// the whole event retries next tick, which is safe because every write here is a conditional
/// UPDATE already idempotent against a re-run.
async fn reconcile_project_archives(db: &DatabaseConnection) -> Result<(), DbErr> {
    let events_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, project_id, actor_principal_id FROM deployment_approval_project_archive_events \
         WHERE processed_at IS NULL ORDER BY archived_at ASC, id ASC LIMIT 10",
        [],
    );
    let event_rows = db.query_all_raw(events_statement).await?;
    let mut events = Vec::with_capacity(event_rows.len());
    for row in event_rows {
        events.push((
            row.try_get_by::<Uuid, _>("id")?,
            row.try_get_by::<Uuid, _>("project_id")?,
            row.try_get_by::<Option<Uuid>, _>("actor_principal_id")?,
        ));
    }

    for (event_id, project_id, actor) in events {
        let txn = db.begin().await?;
        let candidates_statement = Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT requirement.id, deployment.id AS deployment_id, requirement.status \
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
            [event_id.into(), project_id.into()],
        );
        let candidate_rows = txn.query_all_raw(candidates_statement).await?;
        let mut candidates = Vec::with_capacity(candidate_rows.len());
        for row in candidate_rows {
            candidates.push((
                row.try_get_by::<Uuid, _>("id")?,
                row.try_get_by::<Uuid, _>("deployment_id")?,
                row.try_get_by::<String, _>("status")?,
            ));
        }

        for (requirement_id, deployment_id, status) in candidates {
            if rows::requirement_status(status) == ApprovalRequirementStatus::Satisfied {
                block_approval_execution(&txn, deployment_id, actor).await?;
                continue;
            }
            let update_statement = Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE deployment_approval_requirements SET status = 'INVALIDATED', revision = revision + 1, \
                   invalidated_at = CURRENT_TIMESTAMP, invalidation_code = 'PROJECT_ARCHIVED' WHERE id = $1 AND status = 'PENDING'",
                [requirement_id.into()],
            );
            let updated = txn.execute_raw(update_statement).await?;
            if updated.rows_affected() != 1 {
                continue;
            }
            audit(
                &txn,
                deployment_id,
                actor,
                "APPROVAL_INVALIDATED",
                json!({"requirementId": requirement_id.to_string(), "code": "PROJECT_ARCHIVED"}),
            )
            .await?;
            let health_statement = Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Project archive terminalized this pending approval cycle.', \
                   observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
                [deployment_id.into()],
            );
            txn.execute_raw(health_statement).await?;
            let status_statement = Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, projection_revision = projection_revision + 1, \
                   updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')",
                [deployment_id.into()],
            );
            txn.execute_raw(status_statement).await?;
            touch_projection(&txn, deployment_id).await?;
        }

        let still_pending_statement = Statement::from_sql_and_values(
            txn.get_database_backend(),
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
                   OR (deployment.lifecycle_status IN ('APPROVED', 'REQUESTED') AND requirement.status = 'SATISFIED'))) AS still_pending",
            [event_id.into(), project_id.into()],
        );
        let still_pending: bool = txn
            .query_one_raw(still_pending_statement)
            .await?
            .expect("EXISTS(...) always returns exactly one row")
            .try_get_by("still_pending")?;
        if !still_pending {
            let processed_statement = Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE deployment_approval_project_archive_events SET processed_at = CURRENT_TIMESTAMP WHERE id = $1",
                [event_id.into()],
            );
            txn.execute_raw(processed_statement).await?;
        }
        txn.commit().await?;
    }
    Ok(())
}

/// Ports the public `reconcileApprovalUpgrade()` wrapper. The original also carried a "compatibility
/// backfill" phase for rows a Java migration history accumulated before a unified write path
/// existed; a greenfield rewrite has no such backlog, so only archive reconciliation remains here.
pub async fn reconcile_approval_upgrade(
    db: &DatabaseConnection,
    state: &crate::ApprovalMaintenanceState,
) {
    let outcome = async {
        reconcile_project_archives(db).await?;
        approval_project_archive_pending(db).await
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
                super::worker::db_failure_code(&error)
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
    db: &impl ConnectionTrait,
    worker_ready: bool,
) -> Result<Vec<Uuid>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
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
        [worker_ready.into()],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values = Vec::with_capacity(rows_found.len());
    for row in rows_found {
        values.push(row.try_get_by::<Uuid, _>("deployment_id")?);
    }
    Ok(values)
}

async fn release_compatible_approval_handoffs_inner(
    db: &DatabaseConnection,
) -> Result<bool, DbErr> {
    let ready = worker_ready(db).await?;
    let deployments = compatible_approval_handoff_deployments(db, ready).await?;
    let mut all_succeeded = true;
    for deployment_id in deployments {
        let txn = db.begin().await?;
        match automatic_approval_handoff(&txn, deployment_id).await {
            Ok(_) => txn.commit().await?,
            Err(_) => {
                let _ = txn.rollback().await;
                all_succeeded = false;
            }
        }
    }
    Ok(all_succeeded)
}

/// Ports `releaseCompatibleApprovalHandoffs`: `true` only if the candidate page was read AND every
/// row's handoff attempt succeeded, matching every failure mode there folding into the same
/// `APPROVAL_MAINTENANCE_FAILED` heartbeat report in Java's `recordWorkerHeartbeat`.
pub(crate) async fn release_compatible_approval_handoffs(db: &DatabaseConnection) -> bool {
    match release_compatible_approval_handoffs_inner(db).await {
        Ok(all_succeeded) => all_succeeded,
        Err(error) => {
            tracing::warn!(code = %super::worker::db_failure_code(&error), "approval handoff maintenance failed");
            false
        }
    }
}

/// Ports `approvalMaintenanceFailed`: whether this worker's own currently-stored heartbeat row
/// already reports a sticky, unrecovered maintenance failure.
pub(crate) async fn approval_maintenance_failed(
    db: &impl ConnectionTrait,
    worker: &str,
) -> Result<bool, DbErr> {
    let worker = worker.trim();
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT state = 'DEGRADED' AND failure_code = 'APPROVAL_MAINTENANCE_FAILED' AS sticky_failure \
         FROM deployment_worker_heartbeats WHERE worker_id = $1",
        [worker.into()],
    );
    Ok(db
        .query_one_raw(statement)
        .await?
        .map(|row| row.try_get_by("sticky_failure"))
        .transpose()?
        .unwrap_or(false))
}
