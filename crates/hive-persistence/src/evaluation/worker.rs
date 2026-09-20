//! Ports the worker-delivery methods `PostgresEvaluationWorkStore` delegates
//! to (`claimNext`/`commit`/`idle`/`delivered`/`failed`/`claimFailed`) and
//! their private helpers: `claim`/`prepareWork`/`start`/`executeCase`/
//! `finalizeRun`/`appendEvidence`/`reclaim`/`retryOrDeadLetter`/`heartbeat`.

use hive_application::configuration::canonical::digest as sha256;
use hive_application::evaluation::{
    EvaluationExecutionDecision, EvaluationFinalizationDecision, EvaluationWorkDecision,
    EvaluationWorkItem,
};
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

use super::mutations::{audit_run, enqueue};
use super::queries::{raw_run, version};
use super::rows::{self, Event, RawRun};
use hive_application::evaluation::EvaluationRunStatus;

const DELIVERY_LIMIT: i32 = 4;

/// Ports `updateRun`'s optimistic transition. The terminal-state assertion Java takes before the
/// `UPDATE` is not re-derived here (every call site already only reaches this after its own
/// `EvaluationRunStateMachine`- or status-list-driven check decided the transition is legal, the
/// same "share one source of truth, don't duplicate it" reasoning `transition_requirement` in the
/// deployment domain already documents) — the `WHERE id = $.. AND generation = $..` match is the
/// real optimistic guard, same conditional-UPDATE-claim pattern used throughout this rewrite.
pub(super) async fn update_run(
    conn: &mut PgConnection,
    current: &RawRun,
    status: EvaluationRunStatus,
    generation: i64,
    outcome: Option<&str>,
    code: Option<&str>,
    terminal: bool,
) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(
        "UPDATE evaluation_runs SET lifecycle_status = $1, generation = $2, outcome_category = $3, outcome_code = $4, \
             started_at = CASE WHEN $1 = 'RUNNING' THEN COALESCE(started_at, CURRENT_TIMESTAMP) ELSE started_at END, \
             completed_at = CASE WHEN $5 THEN CURRENT_TIMESTAMP ELSE completed_at END \
         WHERE id = $6 AND generation = $7",
    )
    .bind(status.as_str())
    .bind(generation)
    .bind(outcome)
    .bind(code)
    .bind(terminal)
    .bind(current.id)
    .bind(current.generation)
    .execute(&mut *conn)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

pub(super) async fn insert_result(
    conn: &mut PgConnection,
    run: Uuid,
    passed: bool,
    outcome: &str,
    code: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO evaluation_results (id, run_id, passed, outcome_category, summary_digest) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (run_id) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(run)
    .bind(passed)
    .bind(outcome)
    .bind(sha256(&format!("{run}|{outcome}|{}", code.unwrap_or(""))))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn update_case(
    conn: &mut PgConnection,
    id: Uuid,
    status: EvaluationRunStatus,
    passed: Option<bool>,
    code: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE evaluation_case_runs SET lifecycle_status = $1, passed = $2, failure_code = $3, completed_at = CURRENT_TIMESTAMP \
         WHERE id = $4 AND lifecycle_status IN ('QUEUED', 'RUNNING')",
    )
    .bind(status.as_str())
    .bind(passed)
    .bind(code)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn start_case(conn: &mut PgConnection, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE evaluation_case_runs SET lifecycle_status = 'RUNNING' WHERE id = $1 AND lifecycle_status = 'QUEUED'")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn reclaim(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE evaluation_outbox_events SET status = 'PENDING', available_at = CURRENT_TIMESTAMP, claimed_at = NULL, claimed_by = NULL \
         WHERE status = 'PROCESSING' AND claimed_at < CURRENT_TIMESTAMP - INTERVAL '30 seconds'",
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Ports `claim()`: no `FOR UPDATE`/`SKIP LOCKED`, mirroring the deployment outbox worker's own
/// established claim pattern (a conditional `UPDATE ... WHERE status = 'PENDING'` is the real claim;
/// 0 rows affected means a lost race, and the caller's at-least-once loop retries next tick).
async fn claim(conn: &mut PgConnection, worker: &str) -> Result<Option<Event>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, run_id, event_type, case_run_id, attempt_count FROM evaluation_outbox_events \
         WHERE status = 'PENDING' AND available_at <= CURRENT_TIMESTAMP ORDER BY available_at, created_at LIMIT 1",
    )
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id: Uuid = row.get(0);
    let attempts: i32 = row.get::<i32, _>(4) + 1;
    let updated = sqlx::query(
        "UPDATE evaluation_outbox_events SET status = 'PROCESSING', claimed_at = CURRENT_TIMESTAMP, claimed_by = $1, attempt_count = $2 \
         WHERE id = $3 AND status = 'PENDING'",
    )
    .bind(worker)
    .bind(attempts)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    if updated.rows_affected() != 1 {
        return Ok(None);
    }
    Ok(Some(Event {
        id,
        run_id: row.get(1),
        event_type: row.get(2),
        case_run_id: row.get(3),
        attempts,
    }))
}

async fn delivered_event(conn: &mut PgConnection, event: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE evaluation_outbox_events SET status = 'DELIVERED', delivered_at = CURRENT_TIMESTAMP, last_error = NULL WHERE id = $1 AND status = 'PROCESSING'")
        .bind(event)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn prepare_work(
    conn: &mut PgConnection,
    event: &Event,
) -> Result<Option<EvaluationWorkItem>, sqlx::Error> {
    let Some(run) = raw_run(conn, event.run_id, false).await? else {
        delivered_event(conn, event.id).await?;
        return Ok(None);
    };
    if !run.status.may_cancel() {
        delivered_event(conn, event.id).await?;
        return Ok(None);
    }
    let definition_version = version(conn, run.definition_version_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    if event.event_type == "START" {
        return Ok(Some(EvaluationWorkItem {
            event_id: event.id,
            run_id: event.run_id,
            case_run_id: None,
            event_type: event.event_type.clone(),
            generation: run.generation,
            attempt: event.attempts,
            canonical_document: definition_version.canonical_document,
            case_ordinal: 0,
            completed_cases: Vec::new(),
            current_lifecycle_status: run.status,
        }));
    }
    if event.event_type == "CASE" {
        let (Some(case_run_id), true) = (
            event.case_run_id,
            run.status == EvaluationRunStatus::Running,
        ) else {
            delivered_event(conn, event.id).await?;
            return Ok(None);
        };
        start_case(conn, case_run_id).await?;
        let ordinal: Option<(i32,)> =
            sqlx::query_as("SELECT ordinal FROM evaluation_case_runs WHERE id = $1")
                .bind(case_run_id)
                .fetch_optional(&mut *conn)
                .await?;
        let Some((ordinal,)) = ordinal else {
            delivered_event(conn, event.id).await?;
            return Ok(None);
        };
        return Ok(Some(EvaluationWorkItem {
            event_id: event.id,
            run_id: event.run_id,
            case_run_id: Some(case_run_id),
            event_type: event.event_type.clone(),
            generation: run.generation,
            attempt: event.attempts,
            canonical_document: definition_version.canonical_document,
            case_ordinal: ordinal,
            completed_cases: Vec::new(),
            current_lifecycle_status: run.status,
        }));
    }
    if event.event_type != "FINALIZE" || run.status != EvaluationRunStatus::Running {
        delivered_event(conn, event.id).await?;
        return Ok(None);
    }
    let rows =
        sqlx::query("SELECT passed FROM evaluation_case_runs WHERE run_id = $1 ORDER BY ordinal")
            .bind(run.id)
            .fetch_all(&mut *conn)
            .await?;
    let completed_cases: Vec<Option<bool>> = rows.iter().map(|row| row.get(0)).collect();
    Ok(Some(EvaluationWorkItem {
        event_id: event.id,
        run_id: event.run_id,
        case_run_id: None,
        event_type: event.event_type.clone(),
        generation: run.generation,
        attempt: event.attempts,
        canonical_document: definition_version.canonical_document,
        case_ordinal: 0,
        completed_cases,
        current_lifecycle_status: run.status,
    }))
}

pub async fn claim_next(
    pool: &PgPool,
    worker_id: &str,
) -> Result<Option<EvaluationWorkItem>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    reclaim(&mut tx).await?;
    let event = claim(&mut tx, worker_id).await?;
    let work = match &event {
        Some(event) => prepare_work(&mut tx, event).await?,
        None => None,
    };
    tx.commit().await?;
    Ok(work)
}

async fn claim_owned(
    conn: &mut PgConnection,
    worker: &str,
    event: &Event,
) -> Result<bool, sqlx::Error> {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM evaluation_outbox_events WHERE id = $1 AND status = 'PROCESSING' AND claimed_by = $2 AND attempt_count = $3)",
    )
    .bind(event.id)
    .bind(worker)
    .bind(event.attempts)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.0)
}

fn event_from_work(work: &EvaluationWorkItem) -> Event {
    Event {
        id: work.event_id,
        run_id: work.run_id,
        event_type: work.event_type.clone(),
        case_run_id: work.case_run_id,
        attempts: work.attempt,
    }
}

pub async fn commit(
    pool: &PgPool,
    worker_id: &str,
    work: &EvaluationWorkItem,
    decision: EvaluationWorkDecision,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let event = event_from_work(work);
    if !claim_owned(&mut tx, worker_id, &event).await? {
        tx.commit().await?;
        return Ok(());
    }
    let Some(run) = raw_run(&mut tx, work.run_id, true).await? else {
        delivered_event(&mut tx, event.id).await?;
        tx.commit().await?;
        return Ok(());
    };
    if run.generation != work.generation {
        delivered_event(&mut tx, event.id).await?;
        tx.commit().await?;
        return Ok(());
    }
    match decision {
        EvaluationWorkDecision::Start { lifecycle_status } => {
            start(&mut tx, &run, lifecycle_status).await?
        }
        EvaluationWorkDecision::Case(value) => execute_case(&mut tx, &event, &run, value).await?,
        EvaluationWorkDecision::Finalize(value) => finalize_run(&mut tx, &run, value).await?,
    }
    delivered_event(&mut tx, event.id).await?;
    tx.commit().await?;
    Ok(())
}

async fn start(
    conn: &mut PgConnection,
    run: &RawRun,
    next: EvaluationRunStatus,
) -> Result<(), sqlx::Error> {
    if next != EvaluationRunStatus::Running || next == run.status {
        return Ok(());
    }
    update_run(conn, run, next, run.generation + 1, None, None, false).await?;
    audit_run(conn, run.id, None, "STARTED", "Evaluation run started.").await?;
    let rows: Vec<(Uuid,)> =
        sqlx::query_as("SELECT id FROM evaluation_case_runs WHERE run_id = $1 ORDER BY ordinal")
            .bind(run.id)
            .fetch_all(&mut *conn)
            .await?;
    for (case_id,) in rows {
        enqueue(conn, run.id, "CASE", Some(case_id)).await?;
    }
    Ok(())
}

async fn execute_case(
    conn: &mut PgConnection,
    event: &Event,
    run: &RawRun,
    decision: EvaluationExecutionDecision,
) -> Result<(), sqlx::Error> {
    let (Some(case_run_id), true) = (
        event.case_run_id,
        run.status == EvaluationRunStatus::Running,
    ) else {
        return Ok(());
    };
    if decision.terminal_run {
        update_case(
            conn,
            case_run_id,
            decision.lifecycle_status,
            decision.passed,
            decision.outcome_code.as_deref(),
        )
        .await?;
        terminal_failure(conn, run, &decision).await?;
        return Ok(());
    }
    update_case(
        conn,
        case_run_id,
        decision.lifecycle_status,
        decision.passed,
        decision.outcome_code.as_deref(),
    )
    .await?;
    let remaining: (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM evaluation_case_runs WHERE run_id = $1 AND lifecycle_status IN ('QUEUED', 'RUNNING'))",
    )
    .bind(run.id)
    .fetch_one(&mut *conn)
    .await?;
    if !remaining.0 {
        enqueue(conn, run.id, "FINALIZE", None).await?;
    }
    Ok(())
}

async fn finalize_run(
    conn: &mut PgConnection,
    run: &RawRun,
    decision: EvaluationFinalizationDecision,
) -> Result<(), sqlx::Error> {
    if run.status != EvaluationRunStatus::Running {
        return Ok(());
    }
    for metric in &decision.metrics {
        sqlx::query(
            "INSERT INTO evaluation_metric_results (id, run_id, metric_code, value, threshold, passed) VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (run_id, metric_code) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(run.id)
        .bind(&metric.code)
        .bind(metric.value)
        .bind(metric.threshold)
        .bind(metric.passed)
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query(
        "INSERT INTO evaluation_artifact_metadata (id, run_id, artifact_kind, content_digest, media_type, byte_length) VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(run.id)
    .bind("LOCAL_SUMMARY")
    .bind(sha256(&format!("{}|summary", run.id)))
    .bind("application/json")
    .bind(0i64)
    .execute(&mut *conn)
    .await?;
    insert_result(
        conn,
        run.id,
        decision.passed,
        &decision.outcome_category,
        Some(decision.summary_digest_material.as_str()),
    )
    .await?;
    update_run(
        conn,
        run,
        decision.lifecycle_status,
        run.generation + 1,
        Some(&decision.outcome_category),
        None,
        true,
    )
    .await?;
    if decision.passed {
        append_evidence(conn, run.id).await?;
    }
    audit_run(conn, run.id, None, "COMPLETED", "Evaluation run completed.").await?;
    Ok(())
}

async fn terminal_failure(
    conn: &mut PgConnection,
    run: &RawRun,
    decision: &EvaluationExecutionDecision,
) -> Result<(), sqlx::Error> {
    insert_result(
        conn,
        run.id,
        false,
        decision.outcome_category.as_deref().unwrap_or(""),
        decision.outcome_code.as_deref(),
    )
    .await?;
    update_run(
        conn,
        run,
        decision.lifecycle_status,
        run.generation + 1,
        decision.outcome_category.as_deref(),
        decision.outcome_code.as_deref(),
        true,
    )
    .await?;
    sqlx::query(
        "UPDATE evaluation_case_runs SET lifecycle_status = 'CANCELED', completed_at = CURRENT_TIMESTAMP WHERE run_id = $1 AND lifecycle_status IN ('QUEUED', 'RUNNING')",
    )
    .bind(run.id)
    .execute(&mut *conn)
    .await?;
    sqlx::query("UPDATE evaluation_outbox_events SET status = 'CANCELED' WHERE run_id = $1 AND status IN ('PENDING', 'PROCESSING')")
        .bind(run.id)
        .execute(&mut *conn)
        .await?;
    audit_run(conn, run.id, None, "FAILED", "Evaluation run failed.").await?;
    Ok(())
}

/// Ports `appendEvidence`: the cross-domain handoff into the already-ported deployment/approval
/// domain on a PASSED terminal run whose target was a `DEPLOYMENT`. Returns the disposition code
/// only for parity with Java's return value; no caller here consumes it (matching `finalizeRun`,
/// which calls this only for its side effect).
async fn append_evidence(conn: &mut PgConnection, run: Uuid) -> Result<String, sqlx::Error> {
    let deployment_id: Option<(Option<Uuid>,)> =
        sqlx::query_as("SELECT deployment_id FROM evaluation_target_snapshots WHERE run_id = $1")
            .bind(run)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((Some(_deployment_id),)) = deployment_id else {
        return Ok("NOT_ELIGIBLE".to_string());
    };
    let candidate = sqlx::query(
        "SELECT run.lifecycle_status, run.outcome_category, run.target_kind, run.target_id, \
             snapshot.deployment_id, snapshot.agent_version_id, snapshot.environment_definition_version_id, \
             snapshot.target_digest, snapshot.plan_digest, snapshot.package_digest, snapshot.binding_digest, \
             deployment.agent_version_id, deployment.environment_definition_version_id, \
             policy.agent_version_id, policy.environment_definition_version_id, \
             policy.target_digest, policy.plan_digest, policy.package_digest, policy.binding_digest, \
             policy.evaluation_requirement_expires_at \
         FROM evaluation_runs run \
           JOIN evaluation_results result ON result.run_id = run.id AND result.passed = TRUE AND result.outcome_category = 'PASSED' \
           JOIN evaluation_target_snapshots snapshot ON snapshot.run_id = run.id \
           JOIN deployments deployment ON deployment.id = snapshot.deployment_id \
           JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id \
         WHERE run.id = $1 \
         FOR UPDATE OF deployment",
    )
    .bind(run)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(candidate) = candidate else {
        return Ok("NOT_ELIGIBLE".to_string());
    };
    let lifecycle_status = rows::run_status(candidate.get(0));
    let outcome_category: Option<String> = candidate.get(1);
    let target_kind: String = candidate.get(2);
    let target_id: Uuid = candidate.get(3);
    let deployment_id: Uuid = candidate.get(4);
    let agent_version_id: Uuid = candidate.get(5);
    let environment_definition_version_id: Uuid = candidate.get(6);
    let target_digest: Option<String> = candidate.get(7);
    let plan_digest: Option<String> = candidate.get(8);
    let package_digest: Option<String> = candidate.get(9);
    let binding_digest: Option<String> = candidate.get(10);
    let deployment_agent_version_id: Uuid = candidate.get(11);
    let deployment_environment_definition_version_id: Uuid = candidate.get(12);
    let policy_agent_version_id: Uuid = candidate.get(13);
    let policy_environment_definition_version_id: Uuid = candidate.get(14);
    let policy_target_digest: Option<String> = candidate.get(15);
    let policy_plan_digest: Option<String> = candidate.get(16);
    let policy_package_digest: Option<String> = candidate.get(17);
    let policy_binding_digest: Option<String> = candidate.get(18);
    let evaluation_requirement_expires_at: chrono::DateTime<chrono::Utc> = candidate.get(19);

    if lifecycle_status != EvaluationRunStatus::Completed
        || outcome_category.as_deref() != Some("PASSED")
        || target_kind != "DEPLOYMENT"
        || target_id != deployment_id
    {
        return Ok("NOT_ELIGIBLE".to_string());
    }
    if agent_version_id != deployment_agent_version_id
        || environment_definition_version_id != deployment_environment_definition_version_id
        || agent_version_id != policy_agent_version_id
        || environment_definition_version_id != policy_environment_definition_version_id
        || target_digest != policy_target_digest
        || plan_digest != policy_plan_digest
        || package_digest != policy_package_digest
        || binding_digest != policy_binding_digest
    {
        return Ok("BINDING_MISMATCH".to_string());
    }
    let pending_requirement: Option<(Uuid,)> = sqlx::query_as(
        "SELECT requirement.id FROM deployment_approval_requirements requirement \
         WHERE requirement.deployment_id = $1 AND requirement.status = 'PENDING' FOR UPDATE",
    )
    .bind(deployment_id)
    .fetch_optional(&mut *conn)
    .await?;
    let waiting = if pending_requirement.is_some() {
        crate::deployment::waiting_for_evaluation(conn, deployment_id).await?
    } else {
        false
    };
    if pending_requirement.is_none() || !waiting {
        return Ok("NOT_PENDING".to_string());
    }
    let already: (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM deployment_evidence_snapshots WHERE deployment_id = $1 AND evidence_kind = 'EVALUATION_PASSED')",
    )
    .bind(deployment_id)
    .fetch_one(&mut *conn)
    .await?;
    if already.0 {
        return Ok("ALREADY_APPENDED".to_string());
    }
    sqlx::query(
        "INSERT INTO deployment_evidence_snapshots (id, deployment_id, evidence_kind, evidence_digest, expires_at, \
             agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest, \
             binding_digest, source_evaluation_run_id) \
         VALUES ($1, $2, 'EVALUATION_PASSED', $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(Uuid::new_v4())
    .bind(deployment_id)
    .bind(sha256(&format!("{}|EVALUATION_PASSED", binding_digest.as_deref().unwrap_or(""))))
    .bind(evaluation_requirement_expires_at)
    .bind(agent_version_id)
    .bind(environment_definition_version_id)
    .bind(&target_digest)
    .bind(&plan_digest)
    .bind(&package_digest)
    .bind(&binding_digest)
    .bind(run)
    .execute(&mut *conn)
    .await?;
    crate::deployment::touch_projection(conn, deployment_id).await?;
    crate::deployment::automatic_approval_handoff(conn, deployment_id).await?;
    Ok("APPENDED".to_string())
}

pub async fn idle(pool: &PgPool, worker_id: &str) -> Result<(), sqlx::Error> {
    heartbeat(pool, worker_id, "READY", None, 0, true).await
}

pub async fn delivered(pool: &PgPool, worker_id: &str) -> Result<(), sqlx::Error> {
    heartbeat(pool, worker_id, "READY", None, 1, true).await
}

pub async fn failed(
    pool: &PgPool,
    worker_id: &str,
    work: &EvaluationWorkItem,
    decision: &EvaluationExecutionDecision,
) -> Result<(), sqlx::Error> {
    let event = event_from_work(work);
    let retry_result = retry_or_dead_letter(pool, worker_id, &event, decision).await;
    heartbeat(
        pool,
        worker_id,
        "DEGRADED",
        decision.outcome_code.as_deref(),
        0,
        false,
    )
    .await?;
    retry_result
}

pub async fn claim_failed(pool: &PgPool, worker_id: &str) -> Result<(), sqlx::Error> {
    heartbeat(pool, worker_id, "DEGRADED", Some("RUNNER_FAILED"), 0, false).await
}

async fn retry_or_dead_letter(
    pool: &PgPool,
    worker: &str,
    event: &Event,
    decision: &EvaluationExecutionDecision,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    if event.attempts < DELIVERY_LIMIT {
        sqlx::query(
            "UPDATE evaluation_outbox_events SET status = 'PENDING', available_at = CURRENT_TIMESTAMP + ($1 * INTERVAL '1 second'), \
                 claimed_at = NULL, claimed_by = NULL, last_error = $2 WHERE id = $3 AND status = 'PROCESSING' AND claimed_by = $4 AND attempt_count = $5",
        )
        .bind(event.attempts)
        .bind(decision.outcome_code.as_deref())
        .bind(event.id)
        .bind(worker)
        .bind(event.attempts)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    let run = raw_run(&mut tx, event.run_id, true).await?;
    let claimed = sqlx::query(
        "UPDATE evaluation_outbox_events SET status = 'DEAD_LETTER', last_error = $1 WHERE id = $2 AND status = 'PROCESSING' AND claimed_by = $3 AND attempt_count = $4",
    )
    .bind(decision.outcome_code.as_deref())
    .bind(event.id)
    .bind(worker)
    .bind(event.attempts)
    .execute(&mut *tx)
    .await?;
    if claimed.rows_affected() == 1 {
        if let Some(run) = &run {
            if run.status.may_cancel() {
                terminal_failure(&mut tx, run, decision).await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Ports `heartbeat()`'s anti-flap upsert: a `READY` heartbeat cannot silently clear an existing
/// `DEGRADED` row unless `recovery` is true (only `idle()`/`delivered()` pass `recovery = true`).
async fn heartbeat(
    pool: &PgPool,
    worker: &str,
    status: &str,
    code: Option<&str>,
    deliveries: i32,
    recovery: bool,
) -> Result<(), sqlx::Error> {
    let queue: (i64, Option<chrono::DateTime<chrono::Utc>>) = sqlx::query_as(
        "SELECT count(*), min(available_at) FROM (SELECT available_at FROM evaluation_outbox_events \
             WHERE status IN ('PENDING', 'PROCESSING') ORDER BY available_at LIMIT 101) bounded",
    )
    .fetch_one(pool)
    .await?;
    let pending_raw = queue.0;
    let oldest = queue.1;
    let truncated = pending_raw > 100;
    let pending = pending_raw.min(100) as i32;
    sqlx::query(
        "INSERT INTO evaluation_worker_heartbeats (worker_id, status, last_failure_code, last_delivery_count, pending_events, oldest_pending_at, pending_truncated) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (worker_id) DO UPDATE SET observed_at = CURRENT_TIMESTAMP, \
           status = CASE WHEN EXCLUDED.status = 'READY' AND evaluation_worker_heartbeats.status = 'DEGRADED' AND NOT $8 THEN 'DEGRADED' ELSE EXCLUDED.status END, \
           last_failure_code = CASE WHEN EXCLUDED.status = 'READY' AND evaluation_worker_heartbeats.status = 'DEGRADED' AND NOT $9 THEN evaluation_worker_heartbeats.last_failure_code ELSE EXCLUDED.last_failure_code END, \
           last_delivery_count = EXCLUDED.last_delivery_count, pending_events = EXCLUDED.pending_events, oldest_pending_at = EXCLUDED.oldest_pending_at, pending_truncated = EXCLUDED.pending_truncated",
    )
    .bind(worker)
    .bind(status)
    .bind(code)
    .bind(deliveries)
    .bind(pending)
    .bind(oldest)
    .bind(truncated)
    .bind(recovery)
    .bind(recovery)
    .execute(pool)
    .await?;
    Ok(())
}
