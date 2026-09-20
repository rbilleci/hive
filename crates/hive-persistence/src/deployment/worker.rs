//! Ports `DeploymentOutboxDelivery.deliverNext` and its engine: claim,
//! execute, complete, retry, dead-letter, and lease reclamation. A
//! deterministic worker calls this transaction; it has no provider,
//! credential, or browser authority.

use super::{rows, writes};
use hive_domain::deployment::DeploymentLifecycleStatus;
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

const MAX_WORKER_DELIVERIES: i32 = 3;

#[derive(Clone)]
struct Event {
    id: Uuid,
    deployment_id: Uuid,
    event_type: Option<String>,
    payload: String,
    attempt_count: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerMode {
    Success,
    Failure,
    Retry,
    Poison,
    Invalid,
}

impl WorkerMode {
    fn name(&self) -> &'static str {
        match self {
            WorkerMode::Success => "SUCCESS",
            WorkerMode::Failure => "FAILURE",
            WorkerMode::Retry => "RETRY",
            WorkerMode::Poison => "POISON",
            WorkerMode::Invalid => "INVALID",
        }
    }
}

fn decode_mode(payload: &str) -> WorkerMode {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return WorkerMode::Invalid;
    };
    match value.get("mode").and_then(|value| value.as_str()) {
        Some("SUCCESS") => WorkerMode::Success,
        Some("FAILURE") => WorkerMode::Failure,
        Some("RETRY") => WorkerMode::Retry,
        Some("POISON") => WorkerMode::Poison,
        _ => WorkerMode::Invalid,
    }
}

pub(crate) fn sql_failure_code(error: &sqlx::Error) -> String {
    match error {
        sqlx::Error::Database(db_error) => match db_error.code() {
            Some(code)
                if code.len() == 5
                    && code
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) =>
            {
                format!("SQLSTATE_{code}")
            }
            _ => "DATABASE_FAILURE".to_string(),
        },
        _ => "DATABASE_FAILURE".to_string(),
    }
}

pub async fn deliver_next(pool: &PgPool, worker: &str) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut claimed: Option<Event> = None;
    let result = deliver_next_tx(&mut tx, worker, &mut claimed).await;
    match result {
        Ok(delivered) => {
            tx.commit().await?;
            Ok(delivered)
        }
        Err(error) => {
            // `tx` is still open and uncommitted here: Rust does not drop it until this function
            // returns, so an explicit rollback is required before opening the recovery transaction
            // below. Without it, `recover_failed_delivery`'s claim UPDATE targets the same row `tx`
            // already claimed and blocks forever waiting for a lock `tx` never releases — a
            // deterministic deadlock, not a rare race. Roll back first so the row's lock releases,
            // then claim the still-pending event in a fresh transaction so persistent database
            // faults reach the bounded retry/dead-letter path instead of restarting at attempt one
            // on every poll.
            let _ = tx.rollback().await;
            if let Some(event) = claimed {
                if recover_failed_delivery(pool, worker, &event).await? {
                    return Ok(true);
                }
            }
            Err(error)
        }
    }
}

async fn deliver_next_tx(
    tx: &mut PgConnection,
    worker: &str,
    claimed: &mut Option<Event>,
) -> Result<bool, sqlx::Error> {
    reclaim_expired_leases(tx).await?;
    let Some(event) = claim(tx, worker).await? else {
        return Ok(false);
    };
    *claimed = Some(event.clone());
    let mode = decode_mode(&event.payload);
    if matches!(
        mode,
        WorkerMode::Retry | WorkerMode::Poison | WorkerMode::Invalid
    ) || event.event_type.is_none()
    {
        let detail = if mode == WorkerMode::Invalid {
            "The local worker rejected an unrecognized event mode."
        } else {
            "The local worker isolated a poison event."
        };
        if event.attempt_count >= MAX_WORKER_DELIVERIES {
            dead_letter(tx, &event, detail).await?;
        } else {
            retry(
                tx,
                &event,
                if mode == WorkerMode::Invalid {
                    "The local worker rejected an unrecognized event mode."
                } else {
                    "The local worker will retry this event."
                },
            )
            .await?;
        }
    } else if event.event_type.as_deref() == Some("EXECUTE_DEPLOYMENT") {
        execute(tx, &event, mode, worker).await?;
    } else if event.event_type.as_deref() == Some("COMPLETE_DEPLOYMENT") {
        complete(tx, &event, mode).await?;
    } else {
        dead_letter(
            tx,
            &event,
            "The local worker rejected an unrecognized event type.",
        )
        .await?;
    }
    Ok(true)
}

async fn recover_failed_delivery(
    pool: &PgPool,
    worker: &str,
    claimed: &Event,
) -> Result<bool, sqlx::Error> {
    let Some(event) = claim_failed_delivery(pool, worker, claimed).await? else {
        return Ok(true);
    };
    let dead_lettered = event.attempt_count >= MAX_WORKER_DELIVERIES;
    if let Err(error) = persist_failed_delivery_state(pool, &event, dead_lettered).await {
        // The durable claim already consumed this attempt. A lease recovery makes a later poll
        // resume from that count instead of returning this event to an unbounded attempt-one loop.
        tracing::warn!(code = %sql_failure_code(&error), "local worker recovery state write failed");
        return Ok(true);
    }
    Ok(true)
}

async fn claim_failed_delivery(
    pool: &PgPool,
    worker: &str,
    claimed: &Event,
) -> Result<Option<Event>, sqlx::Error> {
    let mut recovery = pool.begin().await?;
    let row = sqlx::query("UPDATE deployment_outbox_events SET status = 'PROCESSING', claimed_at = CURRENT_TIMESTAMP, claimed_by = $1, attempt_count = attempt_count + 1 WHERE id = $2 AND status = 'PENDING' RETURNING attempt_count")
        .bind(worker)
        .bind(claimed.id)
        .fetch_optional(&mut *recovery)
        .await?;
    let event = row.map(|row| Event {
        id: claimed.id,
        deployment_id: claimed.deployment_id,
        event_type: claimed.event_type.clone(),
        payload: claimed.payload.clone(),
        attempt_count: row.get(0),
    });
    recovery.commit().await?;
    Ok(event)
}

async fn persist_failed_delivery_state(
    pool: &PgPool,
    event: &Event,
    dead_lettered: bool,
) -> Result<(), sqlx::Error> {
    let mut recovery = pool.begin().await?;
    let detail = "The local worker retried a database delivery failure.";
    if dead_lettered {
        dead_letter_state(&mut recovery, event, detail).await?;
    } else {
        retry(&mut recovery, event, detail).await?;
    }
    enqueue_failed_delivery_audit(&mut recovery, event, dead_lettered).await?;
    recovery.commit().await?;
    Ok(())
}

async fn enqueue_failed_delivery_audit(
    conn: &mut PgConnection,
    event: &Event,
    dead_lettered: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO deployment_outbox_delivery_audit_repairs (outbox_event_id, delivery_attempt, deployment_id, action) VALUES ($1, $2, $3, $4) ON CONFLICT (outbox_event_id, delivery_attempt, action) DO NOTHING")
        .bind(event.id)
        .bind(event.attempt_count)
        .bind(event.deployment_id)
        .bind(if dead_lettered { "OUTBOX_DEAD_LETTERED" } else { "OUTBOX_DELIVERY_RETRIED" })
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Records recovery audit obligations in their own transaction so an audit outage cannot erase
/// delivery state. Called from `record_worker_heartbeat` on every ready tick before reporting
/// readiness, matching Java's `recordWorkerHeartbeat`.
async fn repair_pending_delivery_audits(pool: &PgPool) -> bool {
    match repair_pending_delivery_audits_inner(pool).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(code = %sql_failure_code(&error), "local worker recovery audit failed");
            false
        }
    }
}

async fn repair_pending_delivery_audits_inner(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut repair = pool.begin().await?;
    // No FOR UPDATE: DSQL accepts the syntax but never blocks or skips concurrently visible rows
    // under its optimistic concurrency control. The conditional UPDATE below is the real ownership
    // check; a lost race surfaces here as a genuine SQLSTATE 40001, caught by the caller.
    let pending = sqlx::query("SELECT outbox_event_id, delivery_attempt, deployment_id, action FROM deployment_outbox_delivery_audit_repairs WHERE recorded_at IS NULL ORDER BY created_at ASC, outbox_event_id ASC, delivery_attempt ASC LIMIT 50")
        .fetch_all(&mut *repair)
        .await?;
    for row in pending {
        let event_id: Uuid = row.get("outbox_event_id");
        let attempt: i32 = row.get("delivery_attempt");
        let deployment_id: Uuid = row.get("deployment_id");
        let action: String = row.get("action");
        writes::audit(
            &mut repair,
            deployment_id,
            None,
            &action,
            serde_json::json!({"eventId": event_id.to_string(), "deliveryAttempt": attempt}),
        )
        .await?;
        let updated = sqlx::query("UPDATE deployment_outbox_delivery_audit_repairs SET recorded_at = CURRENT_TIMESTAMP WHERE outbox_event_id = $1 AND delivery_attempt = $2 AND action = $3 AND recorded_at IS NULL")
            .bind(event_id)
            .bind(attempt)
            .bind(&action)
            .execute(&mut *repair)
            .await?;
        if updated.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound);
        }
    }
    repair.commit().await?;
    Ok(())
}

async fn approval_handoff_pending(
    conn: &mut PgConnection,
    deployment_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let row: (bool,) = sqlx::query_as(
        "SELECT NOT EXISTS (SELECT 1 FROM deployment_approval_requirements requirement WHERE requirement.deployment_id = $1) \
           OR EXISTS (SELECT 1 FROM deployment_approval_requirements requirement WHERE requirement.deployment_id = $1 AND requirement.status = 'PENDING')",
    )
    .bind(deployment_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.0)
}

async fn execute(
    conn: &mut PgConnection,
    event: &Event,
    mode: WorkerMode,
    worker: &str,
) -> Result<(), sqlx::Error> {
    // The recovery handoff takes its authority lock before it locks the deployment. A retained event
    // with no requirement therefore cannot invert the decision lock order.
    if approval_handoff_pending(conn, event.deployment_id).await? {
        crate::deployment::approval::automatic_approval_handoff(conn, event.deployment_id).await?;
    }
    let deployment = deployment_locked(conn, event.deployment_id).await?;
    let Some(deployment) = deployment else {
        return delivered(conn, event).await;
    };
    if !deployment.lifecycle_status.awaits_execution() {
        return delivered(conn, event).await;
    }
    let project_active = sqlx::query(
        "SELECT 1 FROM projects WHERE id = $1 AND lifecycle_status = 'ACTIVE' FOR SHARE",
    )
    .bind(deployment.project_id)
    .fetch_optional(&mut *conn)
    .await?
    .is_some();
    if !project_active {
        crate::deployment::approval::block_approval_execution(conn, deployment.id, None).await?;
        return delivered(conn, event).await;
    }
    if crate::deployment::approval::approved_approval_handoff(conn, deployment.id).await?
        && !crate::deployment::approval::compatible_approval_worker(conn, worker).await?
    {
        return crate::deployment::approval::defer_incompatible_approval_handoff(conn, event.id)
            .await;
    }
    if !crate::deployment::approval::approval_execution_eligible(conn, deployment.id).await? {
        if approval_handoff_pending(conn, deployment.id).await? {
            return crate::deployment::approval::defer_pending_approval_handoff(conn, event.id)
                .await;
        }
        crate::deployment::approval::block_approval_execution(conn, deployment.id, None).await?;
        return delivered(conn, event).await;
    }
    if crate::deployment::approval::approved_approval_handoff(conn, deployment.id).await?
        && !crate::deployment::approval::compatible_approval_worker(conn, worker).await?
    {
        return crate::deployment::approval::defer_incompatible_approval_handoff(conn, event.id)
            .await;
    }
    let attempt = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO deployment_attempts (id, deployment_id, deployment_plan_version_id, attempt_number, status, generation, started_at) \
         SELECT $1, $2, id, COALESCE((SELECT MAX(attempt_number) + 1 FROM deployment_attempts WHERE deployment_id = $3), 1), 'RUNNING', 1, CURRENT_TIMESTAMP \
         FROM deployment_plan_versions WHERE deployment_id = $4 AND version_number = 1",
    )
    .bind(attempt)
    .bind(event.deployment_id)
    .bind(event.deployment_id)
    .bind(event.deployment_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query("UPDATE deployments SET lifecycle_status = 'IN_PROGRESS', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1").bind(event.deployment_id).execute(&mut *conn).await?;
    sqlx::query("UPDATE deployment_runtime_health SET status = 'STARTING', summary = 'Local execution is progressing.', observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1")
        .bind(event.deployment_id)
        .execute(&mut *conn)
        .await?;
    writes::stage(
        conn,
        attempt,
        "REQUESTED",
        "SUCCEEDED",
        "The immutable local plan entered execution.",
    )
    .await?;
    writes::stage(
        conn,
        attempt,
        "PACKAGING",
        "SUCCEEDED",
        "The deterministic local package was verified.",
    )
    .await?;
    writes::stage(
        conn,
        attempt,
        "EXECUTING",
        "STARTED",
        "The local execution adapter started the attempt.",
    )
    .await?;
    writes::audit(
        conn,
        event.deployment_id,
        None,
        "EXECUTION_STARTED",
        serde_json::json!({"attemptId": attempt.to_string()}),
    )
    .await?;
    writes::enqueue(
        conn,
        event.deployment_id,
        "COMPLETE_DEPLOYMENT",
        mode.name(),
    )
    .await?;
    delivered(conn, event).await
}

async fn complete(
    conn: &mut PgConnection,
    event: &Event,
    mode: WorkerMode,
) -> Result<(), sqlx::Error> {
    let Some(deployment) = deployment_locked(conn, event.deployment_id).await? else {
        return delivered(conn, event).await;
    };
    let Some(attempt) = &deployment.current_attempt else {
        return delivered(conn, event).await;
    };
    if deployment.lifecycle_status != DeploymentLifecycleStatus::InProgress {
        return delivered(conn, event).await;
    }
    let failure = mode == WorkerMode::Failure;
    let attempt_id = attempt.id;
    sqlx::query("UPDATE deployment_attempts SET status = $1, completed_at = CURRENT_TIMESTAMP, generation = generation + 1, failure_code = $2, failure_summary = $3 WHERE id = $4 AND status = 'RUNNING'")
        .bind(if failure { "FAILED" } else { "SUCCEEDED" })
        .bind(if failure { Some("LOCAL_EXECUTION_FAILED") } else { None })
        .bind(if failure { Some("The local execution fixture reported a failure. Review the stage timeline.") } else { None })
        .bind(attempt_id)
        .execute(&mut *conn)
        .await?;
    writes::stage(
        conn,
        attempt_id,
        if failure { "FAILED" } else { "COMPLETED" },
        if failure { "FAILED" } else { "SUCCEEDED" },
        if failure {
            "The local fixture ended with a sanitized failure."
        } else {
            "The local execution completed successfully."
        },
    )
    .await?;
    sqlx::query("UPDATE deployments SET lifecycle_status = $1, revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
        .bind(if failure { "FAILED" } else { "ACTIVE" })
        .bind(event.deployment_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("UPDATE deployment_runtime_health SET status = $1, summary = $2, observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $3")
        .bind(if failure { "UNHEALTHY" } else { "HEALTHY" })
        .bind(if failure { "The local execution did not produce a healthy runtime." } else { "The deterministic local runtime is healthy." })
        .bind(event.deployment_id)
        .execute(&mut *conn)
        .await?;
    writes::audit(
        conn,
        event.deployment_id,
        None,
        if failure {
            "EXECUTION_FAILED"
        } else {
            "EXECUTION_SUCCEEDED"
        },
        serde_json::json!({"attemptId": attempt_id.to_string(), "mode": mode.name()}),
    )
    .await?;
    delivered(conn, event).await
}

async fn retry(conn: &mut PgConnection, event: &Event, detail: &str) -> Result<(), sqlx::Error> {
    let delay_seconds = 1i64
        .checked_shl((event.attempt_count - 1).clamp(0, 5) as u32)
        .unwrap_or(30)
        .min(30);
    let jitter_millis = (event.id.as_u64_pair().1 as i64).rem_euclid(250);
    let updated = sqlx::query(
        "UPDATE deployment_outbox_events SET status = 'PENDING', available_at = CURRENT_TIMESTAMP + ($1 * INTERVAL '1 second') \
             + ($2 * INTERVAL '1 millisecond'), claimed_at = NULL, claimed_by = NULL, last_error = $3 WHERE id = $4 AND status = 'PROCESSING'",
    )
    .bind(delay_seconds)
    .bind(jitter_millis)
    .bind(detail)
    .bind(event.id)
    .execute(&mut *conn)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

async fn dead_letter(
    conn: &mut PgConnection,
    event: &Event,
    detail: &str,
) -> Result<(), sqlx::Error> {
    dead_letter_state(conn, event, detail).await?;
    writes::audit(
        conn,
        event.deployment_id,
        None,
        "OUTBOX_DEAD_LETTERED",
        serde_json::json!({"eventId": event.id.to_string()}),
    )
    .await
}

/// Persists the terminal delivery state without coupling it to later audit availability.
async fn dead_letter_state(
    conn: &mut PgConnection,
    event: &Event,
    detail: &str,
) -> Result<(), sqlx::Error> {
    let updated = sqlx::query("UPDATE deployment_outbox_events SET status = 'DEAD_LETTER', last_error = $1, claimed_at = CURRENT_TIMESTAMP WHERE id = $2 AND status = 'PROCESSING'")
        .bind(detail)
        .bind(event.id)
        .execute(&mut *conn)
        .await?;
    if updated.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    let Some(deployment) = rows::deployments(conn, &[event.deployment_id], true)
        .await?
        .into_iter()
        .next()
    else {
        return Ok(());
    };
    if deployment.lifecycle_status.is_terminal() {
        return Ok(());
    }
    let running = writes::terminalize_running_attempts(
        conn,
        event.deployment_id,
        "FAILED",
        "LOCAL_OUTBOX_POISON",
        detail,
        "FAILED",
        "FAILED",
    )
    .await?;
    if running.is_empty() {
        let attempt = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO deployment_attempts (id, deployment_id, deployment_plan_version_id, attempt_number, status, generation, started_at, completed_at, \
                 failure_code, failure_summary) \
             SELECT $1, $2, id, COALESCE((SELECT MAX(attempt_number) + 1 FROM deployment_attempts WHERE deployment_id = $3), 1), 'FAILED', 1, \
               CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, 'LOCAL_OUTBOX_POISON', $4 FROM deployment_plan_versions WHERE deployment_id = $5 AND version_number = 1",
        )
        .bind(attempt)
        .bind(event.deployment_id)
        .bind(event.deployment_id)
        .bind(detail)
        .bind(event.deployment_id)
        .execute(&mut *conn)
        .await?;
        writes::stage(conn, attempt, "FAILED", "FAILED", detail).await?;
    }
    sqlx::query("UPDATE deployments SET lifecycle_status = 'FAILED', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1").bind(event.deployment_id).execute(&mut *conn).await?;
    sqlx::query("UPDATE deployment_runtime_health SET status = 'UNHEALTHY', summary = 'The local worker isolated an execution event.', observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1")
        .bind(event.deployment_id)
        .execute(&mut *conn)
        .await?;
    // A poisoned event can dead-letter a deployment straight from AWAITING_APPROVAL/REQUESTED, the
    // one terminalization site that reaches FAILED without already having resolved (satisfied,
    // rejected, or invalidated) its approval requirement first.
    crate::deployment::approval::invalidate_pending_requirement(
        conn,
        event.deployment_id,
        "TERMINAL_LIFECYCLE",
        None,
    )
    .await?;
    sqlx::query("DELETE FROM deployment_approval_handoff_releases WHERE deployment_id = $1")
        .bind(event.deployment_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn reclaim_expired_leases(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    let rows_found = sqlx::query(
        "UPDATE deployment_outbox_events SET status = 'PENDING', available_at = CURRENT_TIMESTAMP, claimed_at = NULL, claimed_by = NULL, \
             last_error = 'A local worker lease expired before delivery completed.' \
         WHERE status = 'PROCESSING' AND claimed_at < CURRENT_TIMESTAMP - INTERVAL '30 seconds' RETURNING deployment_id",
    )
    .fetch_all(&mut *conn)
    .await?;
    for row in rows_found {
        let deployment_id: Uuid = row.get(0);
        writes::audit(
            conn,
            deployment_id,
            None,
            "OUTBOX_LEASE_RECLAIMED",
            serde_json::json!({"recovery": "lease-expired"}),
        )
        .await?;
    }
    Ok(())
}

async fn claim(conn: &mut PgConnection, worker: &str) -> Result<Option<Event>, sqlx::Error> {
    // No FOR UPDATE: see `repair_pending_delivery_audits` for why. The conditional UPDATE below is
    // the real claim — it already returns `None` on a lost race, and `deliver_next`'s existing
    // rollback-and-reclaim-in-a-fresh-transaction fallback already covers the case where the race is
    // instead only detected at commit, as SQLSTATE 40001.
    let row = sqlx::query("SELECT id, deployment_id, event_type, payload::text, attempt_count + 1 FROM deployment_outbox_events WHERE status = 'PENDING' AND available_at <= CURRENT_TIMESTAMP ORDER BY available_at, created_at LIMIT 1")
        .fetch_optional(&mut *conn)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let event = Event {
        id: row.get(0),
        deployment_id: row.get(1),
        event_type: row.get(2),
        payload: row.get(3),
        attempt_count: row.get(4),
    };
    let updated = sqlx::query("UPDATE deployment_outbox_events SET status = 'PROCESSING', claimed_at = CURRENT_TIMESTAMP, claimed_by = $1, attempt_count = attempt_count + 1 WHERE id = $2 AND status = 'PENDING'")
        .bind(worker)
        .bind(event.id)
        .execute(&mut *conn)
        .await?;
    if updated.rows_affected() != 1 {
        return Ok(None);
    }
    Ok(Some(event))
}

async fn delivered(conn: &mut PgConnection, event: &Event) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE deployment_outbox_events SET status = 'DELIVERED', delivered_at = CURRENT_TIMESTAMP, last_error = NULL WHERE id = $1 AND status = 'PROCESSING'")
        .bind(event.id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn deployment_locked(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<hive_application::deployment::Deployment>, sqlx::Error> {
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

/// A worker emits one durable heartbeat after each local batch or contained failure. Ports
/// `writeWorkerHeartbeat` only — `recordWorkerHeartbeat`'s maintenance-triggering branch
/// (`if (ready && approvalMaintenanceDue())`) is RTP-APPROVAL's job, not ported here (see this
/// module's parent doc comment). The `deployment-worker` subcommand calls this directly.
pub async fn write_worker_heartbeat(
    pool: &PgPool,
    worker: &str,
    delivered: i32,
    ready: bool,
    failure_code: Option<&str>,
) -> Result<(), sqlx::Error> {
    let worker = worker.trim();
    sqlx::query(
        "INSERT INTO deployment_worker_heartbeats (worker_id, observed_at, last_batch_deliveries, pending_events, oldest_pending_at, state, failure_code, \
             approval_execution_compatible) \
         SELECT $1, CURRENT_TIMESTAMP, $2, pending.count, pending.oldest, $3, $4, TRUE \
         FROM ( \
           SELECT COUNT(*)::integer AS count, MIN(available_at) AS oldest FROM ( \
             SELECT available_at FROM deployment_outbox_events \
             WHERE status IN ('PENDING', 'PROCESSING') ORDER BY available_at, created_at LIMIT 51 \
           ) bounded \
         ) pending \
         ON CONFLICT (worker_id) DO UPDATE SET observed_at = EXCLUDED.observed_at, \
           last_batch_deliveries = EXCLUDED.last_batch_deliveries, pending_events = EXCLUDED.pending_events, \
           oldest_pending_at = EXCLUDED.oldest_pending_at, state = EXCLUDED.state, failure_code = EXCLUDED.failure_code, \
           approval_execution_compatible = EXCLUDED.approval_execution_compatible",
    )
    .bind(worker)
    .bind(delivered.max(0))
    .bind(if ready { "READY" } else { "DEGRADED" })
    .bind(failure_code)
    .execute(pool)
    .await?;
    sqlx::query(
        "DELETE FROM deployment_worker_heartbeats WHERE worker_id IN ( \
           SELECT worker_id FROM deployment_worker_heartbeats \
           WHERE worker_id <> $1 AND observed_at < CURRENT_TIMESTAMP - INTERVAL '15 seconds' \
           ORDER BY observed_at ASC LIMIT 50 \
         )",
    )
    .bind(worker)
    .execute(pool)
    .await?;
    Ok(())
}

/// The rate gate `approvalMaintenanceDue()` enforces: independent of, and in addition to, the
/// `serve`-subcommand's separate 1-second scheduled task (`RTD-MAINTENANCE-PARITY`) — this one
/// gates the opportunistic maintenance pass `record_worker_heartbeat` runs on ready heartbeats.
fn approval_maintenance_due(next_at: &std::sync::atomic::AtomicI64) -> bool {
    use std::sync::atomic::Ordering;
    let now = chrono::Utc::now().timestamp_millis();
    let scheduled = next_at.load(Ordering::SeqCst);
    now >= scheduled
        && next_at
            .compare_exchange(scheduled, now + 1_000, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
}

/// Ports `recordWorkerHeartbeat`. `repairPendingDeliveryAudits` repairs the outbox worker's own
/// audit-repair queue (`deployment_outbox_delivery_audit_repairs`), a Group G concern, not approval
/// reconciliation, but is included here since it long predates RTP-APPROVAL. The sticky
/// `approvalMaintenanceFailed` check and the rate-gated `approvalMaintenanceDue()` maintenance
/// branch are RTP-APPROVAL additions.
pub async fn record_worker_heartbeat(
    pool: &PgPool,
    worker: &str,
    delivered: i32,
    ready: bool,
    failure_code: Option<&str>,
    next_approval_maintenance_at: &std::sync::atomic::AtomicI64,
) -> Result<(), sqlx::Error> {
    if worker.trim().is_empty() {
        return Ok(());
    }
    let maintenance_failed = ready
        && failure_code.is_none()
        && super::approval::approval_maintenance_failed(pool, worker).await?;
    let recovery_failed =
        ready && failure_code.is_none() && !repair_pending_delivery_audits(pool).await;
    let reported_failure = if maintenance_failed {
        Some("APPROVAL_MAINTENANCE_FAILED")
    } else if recovery_failed {
        Some("OUTBOX_RECOVERY_AUDIT_FAILED")
    } else {
        failure_code
    };
    write_worker_heartbeat(
        pool,
        worker,
        delivered,
        reported_failure.is_none() && ready,
        reported_failure,
    )
    .await?;

    if ready && approval_maintenance_due(next_approval_maintenance_at) {
        let healthy = match super::approval::reconcile_expired_approval_requirements(pool).await {
            Ok(health) => health.healthy,
            Err(_) => false,
        };
        let succeeded =
            healthy && super::approval::release_compatible_approval_handoffs(pool).await;
        write_worker_heartbeat(
            pool,
            worker,
            delivered,
            succeeded,
            if succeeded {
                None
            } else {
                Some("APPROVAL_MAINTENANCE_FAILED")
            },
        )
        .await?;
    }
    Ok(())
}
