//! The outbox delivery engine behind `deliver_next`: claim, execute, complete, retry,
//! dead-letter, and lease reclamation. A deterministic worker calls this transaction; it has no
//! provider, credential, or browser authority.

use super::rows;
use crate::entity::enums::{
    DeploymentAttemptStatus, DeploymentLifecycleStatus as EntityLifecycleStatus,
    DeploymentOutboxStatus, DeploymentRuntimeHealthStatus, LifecycleStatus,
    OutboxDeliveryAuditAction,
};
use crate::entity::{
    deployment_approval_handoff_releases, deployment_approval_requirements, deployment_attempts,
    deployment_outbox_delivery_audit_repairs, deployment_outbox_events, deployment_plan_versions,
    deployment_runtime_health, deployment_worker_heartbeats, deployments, projects,
};
use hive_application::deployment::DeploymentLifecycleStatus;
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::sea_query::{Expr, ExprTrait, IntoTableRef, LockType, OnConflict, Query};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, NotSet,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

/// `CAST('<n> seconds' AS interval)`, the spelling `evaluation::worker` established, because
/// sea-query has no interval `Value`.
fn seconds(count: i64) -> Expr {
    Expr::value(format!("{count} seconds")).cast_as("interval")
}

/// `CAST('<n> milliseconds' AS interval)`.
fn millis(count: i64) -> Expr {
    Expr::value(format!("{count} milliseconds")).cast_as("interval")
}

const MAX_WORKER_DELIVERIES: i32 = 3;

#[derive(Clone)]
struct Event {
    id: Uuid,
    deployment_id: Uuid,
    event_type: Option<String>,
    payload: serde_json::Value,
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

fn decode_mode(payload: &serde_json::Value) -> WorkerMode {
    match payload.get("mode").and_then(|value| value.as_str()) {
        Some("SUCCESS") => WorkerMode::Success,
        Some("FAILURE") => WorkerMode::Failure,
        Some("RETRY") => WorkerMode::Retry,
        Some("POISON") => WorkerMode::Poison,
        _ => WorkerMode::Invalid,
    }
}

/// Extracts a Postgres SQLSTATE from a `sea_orm::DbErr`, the same way
/// `crate::retry::is_serialization_failure_db` does, for a human-readable heartbeat failure code.
pub(crate) fn db_failure_code(error: &DbErr) -> String {
    use sea_orm::RuntimeErr;
    let (DbErr::Exec(RuntimeErr::SqlxError(inner)) | DbErr::Query(RuntimeErr::SqlxError(inner))) =
        error
    else {
        return "DATABASE_FAILURE".to_string();
    };
    match inner.as_ref() {
        sea_orm::sqlx::Error::Database(database_error) => match database_error.code() {
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

pub async fn deliver_next(db: &DatabaseConnection, worker: &str) -> Result<bool, DbErr> {
    let txn = db.begin().await?;
    let mut claimed: Option<Event> = None;
    let result = deliver_next_tx(&txn, worker, &mut claimed).await;
    match result {
        Ok(delivered) => {
            txn.commit().await?;
            Ok(delivered)
        }
        Err(error) => {
            // `txn` is still open and uncommitted here: Rust does not drop it until this function
            // returns, so an explicit rollback is required before opening the recovery transaction
            // below. Without it, `recover_failed_delivery`'s claim UPDATE targets the same row `txn`
            // already claimed and blocks forever waiting for a lock `txn` never releases — a
            // deterministic deadlock, not a rare race. Roll back first so the row's lock releases,
            // then claim the still-pending event in a fresh transaction so persistent database
            // faults reach the bounded retry/dead-letter path instead of restarting at attempt one
            // on every poll.
            let _ = txn.rollback().await;
            if let Some(event) = claimed {
                if recover_failed_delivery(db, worker, &event).await? {
                    return Ok(true);
                }
            }
            Err(error)
        }
    }
}

async fn deliver_next_tx(
    txn: &impl ConnectionTrait,
    worker: &str,
    claimed: &mut Option<Event>,
) -> Result<bool, DbErr> {
    reclaim_expired_leases(txn).await?;
    let Some(event) = claim(txn, worker).await? else {
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
            dead_letter(txn, &event, detail).await?;
        } else {
            retry(
                txn,
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
        execute(txn, &event, mode, worker).await?;
    } else if event.event_type.as_deref() == Some("COMPLETE_DEPLOYMENT") {
        complete(txn, &event, mode).await?;
    } else {
        dead_letter(
            txn,
            &event,
            "The local worker rejected an unrecognized event type.",
        )
        .await?;
    }
    Ok(true)
}

async fn recover_failed_delivery(
    db: &DatabaseConnection,
    worker: &str,
    claimed: &Event,
) -> Result<bool, DbErr> {
    let Some(event) = claim_failed_delivery(db, worker, claimed).await? else {
        return Ok(true);
    };
    let dead_lettered = event.attempt_count >= MAX_WORKER_DELIVERIES;
    if let Err(error) = persist_failed_delivery_state(db, &event, dead_lettered).await {
        // The durable claim already consumed this attempt. A lease recovery makes a later poll
        // resume from that count instead of returning this event to an unbounded attempt-one loop.
        tracing::warn!(code = %db_failure_code(&error), "local worker recovery state write failed");
        return Ok(true);
    }
    Ok(true)
}

async fn claim_failed_delivery(
    db: &DatabaseConnection,
    worker: &str,
    claimed: &Event,
) -> Result<Option<Event>, DbErr> {
    use deployment_outbox_events::Column;
    let recovery = db.begin().await?;
    let updated = deployment_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(DeploymentOutboxStatus::Processing.to_value()),
        )
        .col_expr(Column::ClaimedAt, Expr::current_timestamp())
        .col_expr(Column::ClaimedBy, Expr::value(worker))
        .col_expr(Column::AttemptCount, Expr::col(Column::AttemptCount).add(1))
        .filter(Column::Id.eq(claimed.id))
        .filter(Column::Status.eq(DeploymentOutboxStatus::Pending))
        .exec_with_returning(&recovery)
        .await?;
    let event = updated.into_iter().next().map(|row| Event {
        id: claimed.id,
        deployment_id: claimed.deployment_id,
        event_type: claimed.event_type.clone(),
        payload: claimed.payload.clone(),
        attempt_count: row.attempt_count,
    });
    recovery.commit().await?;
    Ok(event)
}

async fn persist_failed_delivery_state(
    db: &DatabaseConnection,
    event: &Event,
    dead_lettered: bool,
) -> Result<(), DbErr> {
    let recovery = db.begin().await?;
    let detail = "The local worker retried a database delivery failure.";
    if dead_lettered {
        dead_letter_state(&recovery, event, detail).await?;
    } else {
        retry(&recovery, event, detail).await?;
    }
    enqueue_failed_delivery_audit(&recovery, event, dead_lettered).await?;
    recovery.commit().await?;
    Ok(())
}

async fn enqueue_failed_delivery_audit(
    db: &impl ConnectionTrait,
    event: &Event,
    dead_lettered: bool,
) -> Result<(), DbErr> {
    use deployment_outbox_delivery_audit_repairs::Column;
    deployment_outbox_delivery_audit_repairs::Entity::insert(
        deployment_outbox_delivery_audit_repairs::ActiveModel {
            outbox_event_id: Set(event.id),
            delivery_attempt: Set(event.attempt_count),
            deployment_id: Set(event.deployment_id),
            action: Set(if dead_lettered {
                OutboxDeliveryAuditAction::OutboxDeadLettered
            } else {
                OutboxDeliveryAuditAction::OutboxDeliveryRetried
            }),
            created_at: NotSet,
            recorded_at: NotSet,
        },
    )
    .on_conflict(
        OnConflict::columns([
            Column::OutboxEventId,
            Column::DeliveryAttempt,
            Column::Action,
        ])
        .do_nothing()
        .to_owned(),
    )
    .try_insert()
    .exec_without_returning(db)
    .await?;
    Ok(())
}

/// Records recovery audit obligations in their own transaction so an audit outage cannot erase
/// delivery state. `record_worker_heartbeat` calls it on every ready tick, before it reports
/// readiness.
async fn repair_pending_delivery_audits(db: &DatabaseConnection) -> bool {
    match repair_pending_delivery_audits_inner(db).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(code = %db_failure_code(&error), "local worker recovery audit failed");
            false
        }
    }
}

async fn repair_pending_delivery_audits_inner(db: &DatabaseConnection) -> Result<(), DbErr> {
    let repair = db.begin().await?;
    // No FOR UPDATE: DSQL accepts the syntax but never blocks or skips concurrently visible rows
    // under its optimistic concurrency control. The conditional UPDATE below is the real ownership
    // check; a lost race surfaces here as a genuine SQLSTATE 40001, caught by the caller.
    use deployment_outbox_delivery_audit_repairs::Column;
    let pending = deployment_outbox_delivery_audit_repairs::Entity::find()
        .filter(Column::RecordedAt.is_null())
        .order_by_asc(Column::CreatedAt)
        .order_by_asc(Column::OutboxEventId)
        .order_by_asc(Column::DeliveryAttempt)
        .limit(50)
        .all(&repair)
        .await?;
    for row in pending {
        let event_id = row.outbox_event_id;
        let attempt = row.delivery_attempt;
        rows::audit(
            &repair,
            row.deployment_id,
            None,
            &row.action.to_value(),
            serde_json::json!({"eventId": event_id.to_string(), "deliveryAttempt": attempt}),
        )
        .await?;
        let updated = deployment_outbox_delivery_audit_repairs::Entity::update_many()
            .col_expr(Column::RecordedAt, Expr::current_timestamp())
            .filter(Column::OutboxEventId.eq(event_id))
            .filter(Column::DeliveryAttempt.eq(attempt))
            .filter(Column::Action.eq(row.action))
            .filter(Column::RecordedAt.is_null())
            .exec(&repair)
            .await?;
        if updated.rows_affected != 1 {
            return Err(DbErr::RecordNotFound(format!(
                "no unrecorded delivery-audit-repair row for event {event_id} attempt {attempt}"
            )));
        }
    }
    repair.commit().await?;
    Ok(())
}

async fn approval_handoff_pending(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    // `NOT EXISTS (any requirement) OR EXISTS (a PENDING requirement)`: the deployment has at most
    // one requirement row (`deployment_id` is unique), so one read answers both.
    let requirement = deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .one(db)
        .await?;
    Ok(match requirement {
        None => true,
        Some(requirement) => {
            requirement.status == crate::entity::enums::ApprovalRequirementStatus::Pending
        }
    })
}

async fn execute(
    db: &impl ConnectionTrait,
    event: &Event,
    mode: WorkerMode,
    worker: &str,
) -> Result<(), DbErr> {
    // The recovery handoff takes its authority lock before it locks the deployment. A retained event
    // with no requirement therefore cannot invert the decision lock order.
    if approval_handoff_pending(db, event.deployment_id).await? {
        crate::deployment::approval::automatic_approval_handoff(db, event.deployment_id).await?;
    }
    let deployment = deployment_locked(db, event.deployment_id).await?;
    let Some(deployment) = deployment else {
        return delivered(db, event).await;
    };
    if !deployment.lifecycle_status.awaits_execution() {
        return delivered(db, event).await;
    }
    let mut project_query = projects::Entity::find_by_id(deployment.project_id)
        .filter(projects::Column::LifecycleStatus.eq(LifecycleStatus::Active))
        .select_only()
        .column(projects::Column::Id);
    QuerySelect::query(&mut project_query)
        .lock_with_tables(LockType::Share, [projects::Entity.into_table_ref()]);
    let project_active = project_query.into_tuple::<Uuid>().one(db).await?.is_some();
    if !project_active {
        crate::deployment::approval::block_approval_execution(db, deployment.id, None).await?;
        return delivered(db, event).await;
    }
    if crate::deployment::approval::approved_approval_handoff(db, deployment.id).await?
        && !crate::deployment::approval::compatible_approval_worker(db, worker).await?
    {
        return crate::deployment::approval::defer_incompatible_approval_handoff(db, event.id)
            .await;
    }
    if !crate::deployment::approval::approval_execution_eligible(db, deployment.id).await? {
        if approval_handoff_pending(db, deployment.id).await? {
            return crate::deployment::approval::defer_pending_approval_handoff(db, event.id).await;
        }
        crate::deployment::approval::block_approval_execution(db, deployment.id, None).await?;
        return delivered(db, event).await;
    }
    if crate::deployment::approval::approved_approval_handoff(db, deployment.id).await?
        && !crate::deployment::approval::compatible_approval_worker(db, worker).await?
    {
        return crate::deployment::approval::defer_incompatible_approval_handoff(db, event.id)
            .await;
    }
    let attempt = Uuid::new_v4();
    insert_attempt(db, attempt, event.deployment_id, None).await?;
    set_lifecycle(db, event.deployment_id, EntityLifecycleStatus::InProgress).await?;
    set_runtime_health(
        db,
        event.deployment_id,
        DeploymentRuntimeHealthStatus::Starting,
        "Local execution is progressing.",
    )
    .await?;
    rows::stage(
        db,
        attempt,
        "REQUESTED",
        "SUCCEEDED",
        "The immutable local plan entered execution.",
    )
    .await?;
    rows::stage(
        db,
        attempt,
        "PACKAGING",
        "SUCCEEDED",
        "The deterministic local package was verified.",
    )
    .await?;
    rows::stage(
        db,
        attempt,
        "EXECUTING",
        "STARTED",
        "The local execution adapter started the attempt.",
    )
    .await?;
    rows::audit(
        db,
        event.deployment_id,
        None,
        "EXECUTION_STARTED",
        serde_json::json!({"attemptId": attempt.to_string()}),
    )
    .await?;
    rows::enqueue(db, event.deployment_id, "COMPLETE_DEPLOYMENT", mode.name()).await?;
    delivered(db, event).await
}

async fn complete(db: &impl ConnectionTrait, event: &Event, mode: WorkerMode) -> Result<(), DbErr> {
    let Some(deployment) = deployment_locked(db, event.deployment_id).await? else {
        return delivered(db, event).await;
    };
    let Some(attempt) = &deployment.current_attempt else {
        return delivered(db, event).await;
    };
    if deployment.lifecycle_status != DeploymentLifecycleStatus::InProgress {
        return delivered(db, event).await;
    }
    let failure = mode == WorkerMode::Failure;
    let attempt_id = attempt.id;
    deployment_attempts::Entity::update_many()
        .col_expr(
            deployment_attempts::Column::Status,
            Expr::value(
                if failure {
                    DeploymentAttemptStatus::Failed
                } else {
                    DeploymentAttemptStatus::Succeeded
                }
                .to_value(),
            ),
        )
        .col_expr(
            deployment_attempts::Column::CompletedAt,
            Expr::current_timestamp(),
        )
        .col_expr(
            deployment_attempts::Column::Generation,
            Expr::col(deployment_attempts::Column::Generation).add(1),
        )
        .col_expr(
            deployment_attempts::Column::FailureCode,
            Expr::value(failure.then_some("LOCAL_EXECUTION_FAILED")),
        )
        .col_expr(
            deployment_attempts::Column::FailureSummary,
            Expr::value(failure.then_some(
                "The local execution fixture reported a failure. Review the stage timeline.",
            )),
        )
        .filter(deployment_attempts::Column::Id.eq(attempt_id))
        .filter(deployment_attempts::Column::Status.eq(DeploymentAttemptStatus::Running))
        .exec(db)
        .await?;
    rows::stage(
        db,
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
    set_lifecycle(
        db,
        event.deployment_id,
        if failure {
            EntityLifecycleStatus::Failed
        } else {
            EntityLifecycleStatus::Active
        },
    )
    .await?;
    set_runtime_health(
        db,
        event.deployment_id,
        if failure {
            DeploymentRuntimeHealthStatus::Unhealthy
        } else {
            DeploymentRuntimeHealthStatus::Healthy
        },
        if failure {
            "The local execution did not produce a healthy runtime."
        } else {
            "The deterministic local runtime is healthy."
        },
    )
    .await?;
    rows::audit(
        db,
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
    delivered(db, event).await
}

async fn retry(db: &impl ConnectionTrait, event: &Event, detail: &str) -> Result<(), DbErr> {
    let delay_seconds = 1i64
        .checked_shl((event.attempt_count - 1).clamp(0, 5) as u32)
        .unwrap_or(30);
    let delay_seconds = Ord::min(delay_seconds, 30);
    let jitter_millis = (event.id.as_u64_pair().1 as i64).rem_euclid(250);
    use deployment_outbox_events::Column;
    let updated = deployment_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(DeploymentOutboxStatus::Pending.to_value()),
        )
        .col_expr(
            Column::AvailableAt,
            Expr::current_timestamp()
                .add(seconds(delay_seconds))
                .add(millis(jitter_millis)),
        )
        .col_expr(Column::ClaimedAt, Expr::value(None::<DateTimeWithTimeZone>))
        .col_expr(Column::ClaimedBy, Expr::value(None::<String>))
        .col_expr(Column::LastError, Expr::value(detail))
        .filter(Column::Id.eq(event.id))
        .filter(Column::Status.eq(DeploymentOutboxStatus::Processing))
        .exec(db)
        .await?;
    if updated.rows_affected != 1 {
        return Err(DbErr::RecordNotFound(format!(
            "no processing outbox event with id {}",
            event.id
        )));
    }
    Ok(())
}

async fn dead_letter(db: &impl ConnectionTrait, event: &Event, detail: &str) -> Result<(), DbErr> {
    dead_letter_state(db, event, detail).await?;
    rows::audit(
        db,
        event.deployment_id,
        None,
        "OUTBOX_DEAD_LETTERED",
        serde_json::json!({"eventId": event.id.to_string()}),
    )
    .await
}

/// Persists the terminal delivery state without coupling it to later audit availability.
async fn dead_letter_state(
    db: &impl ConnectionTrait,
    event: &Event,
    detail: &str,
) -> Result<(), DbErr> {
    use deployment_outbox_events::Column;
    let updated = deployment_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(DeploymentOutboxStatus::DeadLetter.to_value()),
        )
        .col_expr(Column::LastError, Expr::value(detail))
        .col_expr(Column::ClaimedAt, Expr::current_timestamp())
        .filter(Column::Id.eq(event.id))
        .filter(Column::Status.eq(DeploymentOutboxStatus::Processing))
        .exec(db)
        .await?;
    if updated.rows_affected != 1 {
        return Err(DbErr::RecordNotFound(format!(
            "no processing outbox event with id {}",
            event.id
        )));
    }
    let Some(deployment) = rows::deployments(db, &[event.deployment_id], true)
        .await?
        .into_iter()
        .next()
    else {
        return Ok(());
    };
    if deployment.lifecycle_status.is_terminal() {
        return Ok(());
    }
    let running = rows::terminalize_running_attempts(
        db,
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
        insert_attempt(db, attempt, event.deployment_id, Some(detail)).await?;
        rows::stage(db, attempt, "FAILED", "FAILED", detail).await?;
    }
    set_lifecycle(db, event.deployment_id, EntityLifecycleStatus::Failed).await?;
    set_runtime_health(
        db,
        event.deployment_id,
        DeploymentRuntimeHealthStatus::Unhealthy,
        "The local worker isolated an execution event.",
    )
    .await?;
    // A poisoned event can dead-letter a deployment straight from AWAITING_APPROVAL/REQUESTED, the
    // one terminalization site that reaches FAILED without already having resolved (satisfied,
    // rejected, or invalidated) its approval requirement first.
    crate::deployment::approval::invalidate_pending_requirement(
        db,
        event.deployment_id,
        "TERMINAL_LIFECYCLE",
        None,
    )
    .await?;
    deployment_approval_handoff_releases::Entity::delete_many()
        .filter(deployment_approval_handoff_releases::Column::DeploymentId.eq(event.deployment_id))
        .exec(db)
        .await?;
    Ok(())
}

async fn reclaim_expired_leases(db: &impl ConnectionTrait) -> Result<(), DbErr> {
    use deployment_outbox_events::Column;
    let rows_found = deployment_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(DeploymentOutboxStatus::Pending.to_value()),
        )
        .col_expr(Column::AvailableAt, Expr::current_timestamp())
        .col_expr(Column::ClaimedAt, Expr::value(None::<DateTimeWithTimeZone>))
        .col_expr(Column::ClaimedBy, Expr::value(None::<String>))
        .col_expr(
            Column::LastError,
            Expr::value("A local worker lease expired before delivery completed."),
        )
        .filter(Column::Status.eq(DeploymentOutboxStatus::Processing))
        .filter(Expr::col(Column::ClaimedAt).lt(Expr::current_timestamp().sub(seconds(30))))
        .exec_with_returning(db)
        .await?;
    for row in rows_found {
        rows::audit(
            db,
            row.deployment_id,
            None,
            "OUTBOX_LEASE_RECLAIMED",
            serde_json::json!({"recovery": "lease-expired"}),
        )
        .await?;
    }
    Ok(())
}

async fn claim(db: &impl ConnectionTrait, worker: &str) -> Result<Option<Event>, DbErr> {
    // No FOR UPDATE: see `repair_pending_delivery_audits` for why. The conditional UPDATE below is
    // the real claim — it already returns `None` on a lost race, and `deliver_next`'s existing
    // rollback-and-reclaim-in-a-fresh-transaction fallback already covers the case where the race is
    // instead only detected at commit, as SQLSTATE 40001.
    use deployment_outbox_events::Column;
    let Some(row) = deployment_outbox_events::Entity::find()
        .filter(Column::Status.eq(DeploymentOutboxStatus::Pending))
        .filter(Expr::col(Column::AvailableAt).lte(Expr::current_timestamp()))
        .order_by_asc(Column::AvailableAt)
        .order_by_asc(Column::CreatedAt)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let event = Event {
        id: row.id,
        deployment_id: row.deployment_id,
        event_type: Some(row.event_type.to_value()),
        payload: row.payload,
        attempt_count: row.attempt_count + 1,
    };
    let updated = deployment_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(DeploymentOutboxStatus::Processing.to_value()),
        )
        .col_expr(Column::ClaimedAt, Expr::current_timestamp())
        .col_expr(Column::ClaimedBy, Expr::value(worker))
        .col_expr(Column::AttemptCount, Expr::col(Column::AttemptCount).add(1))
        .filter(Column::Id.eq(event.id))
        .filter(Column::Status.eq(DeploymentOutboxStatus::Pending))
        .exec(db)
        .await?;
    if updated.rows_affected != 1 {
        return Ok(None);
    }
    Ok(Some(event))
}

async fn delivered(db: &impl ConnectionTrait, event: &Event) -> Result<(), DbErr> {
    use deployment_outbox_events::Column;
    deployment_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(DeploymentOutboxStatus::Delivered.to_value()),
        )
        .col_expr(Column::DeliveredAt, Expr::current_timestamp())
        .col_expr(Column::LastError, Expr::value(None::<String>))
        .filter(Column::Id.eq(event.id))
        .filter(Column::Status.eq(DeploymentOutboxStatus::Processing))
        .exec(db)
        .await?;
    Ok(())
}

async fn deployment_locked(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Option<hive_application::deployment::Deployment>, DbErr> {
    if deployments::Entity::find_by_id(id)
        .lock_exclusive()
        .select_only()
        .column(deployments::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    Ok(rows::deployments(db, &[id], true).await?.into_iter().next())
}

/// A worker emits one durable heartbeat after each local batch or contained failure. This writes
/// the heartbeat and nothing else; the maintenance-triggering branch lives in
/// `record_worker_heartbeat` below. The `deployment-worker` subcommand calls this directly.
pub async fn write_worker_heartbeat(
    db: &DatabaseConnection,
    worker: &str,
    delivered: i32,
    ready: bool,
    failure_code: Option<&str>,
) -> Result<(), DbErr> {
    use deployment_worker_heartbeats::Column;
    let worker = worker.trim();
    // The queued events, bounded: the read takes at most 51 rows and counts them and their
    // earliest `available_at` in Rust. This is a heartbeat metric read outside any transaction.
    let queued = deployment_outbox_events::Entity::find()
        .filter(deployment_outbox_events::Column::Status.is_in([
            DeploymentOutboxStatus::Pending,
            DeploymentOutboxStatus::Processing,
        ]))
        .order_by_asc(deployment_outbox_events::Column::AvailableAt)
        .order_by_asc(deployment_outbox_events::Column::CreatedAt)
        .limit(51)
        .select_only()
        .column(deployment_outbox_events::Column::AvailableAt)
        .into_tuple::<DateTimeWithTimeZone>()
        .all(db)
        .await?;
    let pending = i32::try_from(queued.len()).unwrap_or(i32::MAX);
    let oldest = queued.iter().min().copied();
    deployment_worker_heartbeats::Entity::insert(deployment_worker_heartbeats::ActiveModel {
        worker_id: Set(worker.to_string()),
        observed_at: NotSet,
        last_batch_deliveries: Set(Ord::max(delivered, 0)),
        pending_events: Set(pending),
        oldest_pending_at: Set(oldest),
        state: Set(if ready {
            crate::entity::enums::WorkerHeartbeatState::Ready
        } else {
            crate::entity::enums::WorkerHeartbeatState::Degraded
        }),
        failure_code: Set(failure_code.map(str::to_string)),
        approval_execution_compatible: Set(Some(true)),
    })
    .on_conflict(
        OnConflict::column(Column::WorkerId)
            .update_columns([
                Column::ObservedAt,
                Column::LastBatchDeliveries,
                Column::PendingEvents,
                Column::OldestPendingAt,
                Column::State,
                Column::FailureCode,
                Column::ApprovalExecutionCompatible,
            ])
            .to_owned(),
    )
    .exec_without_returning(db)
    .await?;
    let stale = Query::select()
        .column(Column::WorkerId)
        .from(deployment_worker_heartbeats::Entity)
        .and_where(Expr::col(Column::WorkerId).ne(worker))
        .and_where(Expr::col(Column::ObservedAt).lt(Expr::current_timestamp().sub(seconds(15))))
        .order_by(Column::ObservedAt, sea_orm::Order::Asc)
        .limit(50)
        .to_owned();
    deployment_worker_heartbeats::Entity::delete_many()
        .filter(Column::WorkerId.in_subquery(stale))
        .exec(db)
        .await?;
    Ok(())
}

/// `INSERT INTO deployment_attempts ... SELECT <plan id>, COALESCE(MAX(attempt_number) + 1, 1) ...
/// FROM deployment_plan_versions WHERE deployment_id = $1 AND version_number = 1`, as two reads and
/// an insert by key. Both call sites hold this deployment's row `FOR UPDATE` in the same
/// transaction (`deployment_locked`/`rows::deployments(.., true)`), so no other writer can add an
/// attempt between the maximum and the insert; the table's `(deployment_id, attempt_number)` unique
/// key is the backstop.
async fn insert_attempt(
    db: &impl ConnectionTrait,
    attempt_id: Uuid,
    deployment_id: Uuid,
    failure_summary: Option<&str>,
) -> Result<(), DbErr> {
    let Some(plan) = deployment_plan_versions::Entity::find()
        .filter(deployment_plan_versions::Column::DeploymentId.eq(deployment_id))
        .filter(deployment_plan_versions::Column::VersionNumber.eq(1_i64))
        .one(db)
        .await?
    else {
        return Ok(());
    };
    let next_number = deployment_attempts::Entity::find()
        .filter(deployment_attempts::Column::DeploymentId.eq(deployment_id))
        .order_by_desc(deployment_attempts::Column::AttemptNumber)
        .one(db)
        .await?
        .map_or(1, |attempt| attempt.attempt_number + 1);
    let failed = failure_summary.is_some();
    deployment_attempts::Entity::insert(deployment_attempts::ActiveModel {
        id: Set(attempt_id),
        deployment_id: Set(deployment_id),
        deployment_plan_version_id: Set(plan.id),
        attempt_number: Set(next_number),
        status: Set(if failed {
            DeploymentAttemptStatus::Failed
        } else {
            DeploymentAttemptStatus::Running
        }),
        generation: Set(1),
        started_at: Set(Some(chrono::Utc::now().fixed_offset())),
        completed_at: Set(failed.then(|| chrono::Utc::now().fixed_offset())),
        failure_code: Set(failed.then(|| "LOCAL_OUTBOX_POISON".to_string())),
        failure_summary: Set(failure_summary.map(str::to_string)),
        created_at: NotSet,
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

/// `UPDATE deployments SET lifecycle_status = $1, revision = revision + 1, updated_at =
/// CURRENT_TIMESTAMP WHERE id = $2`.
async fn set_lifecycle(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    status: EntityLifecycleStatus,
) -> Result<(), DbErr> {
    deployments::Entity::update_many()
        .col_expr(
            deployments::Column::LifecycleStatus,
            Expr::value(status.to_value()),
        )
        .col_expr(
            deployments::Column::Revision,
            Expr::col(deployments::Column::Revision).add(1),
        )
        .col_expr(deployments::Column::UpdatedAt, Expr::current_timestamp())
        .filter(deployments::Column::Id.eq(deployment_id))
        .exec(db)
        .await?;
    Ok(())
}

/// `UPDATE deployment_runtime_health SET status = $1, summary = $2, observed_at =
/// CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $3`.
async fn set_runtime_health(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    status: DeploymentRuntimeHealthStatus,
    summary: &str,
) -> Result<(), DbErr> {
    deployment_runtime_health::Entity::update_many()
        .col_expr(
            deployment_runtime_health::Column::Status,
            Expr::value(status.to_value()),
        )
        .col_expr(
            deployment_runtime_health::Column::Summary,
            Expr::value(summary),
        )
        .col_expr(
            deployment_runtime_health::Column::ObservedAt,
            Expr::current_timestamp(),
        )
        .col_expr(
            deployment_runtime_health::Column::Generation,
            Expr::col(deployment_runtime_health::Column::Generation).add(1),
        )
        .filter(deployment_runtime_health::Column::DeploymentId.eq(deployment_id))
        .exec(db)
        .await?;
    Ok(())
}

/// The rate gate `approvalMaintenanceDue()` enforces: independent of, and in addition to, the
/// `serve`-subcommand's separate 1-second scheduled task — this one gates the opportunistic
/// maintenance pass `record_worker_heartbeat` runs on ready heartbeats.
fn approval_maintenance_due(next_at: &std::sync::atomic::AtomicI64) -> bool {
    use std::sync::atomic::Ordering;
    let now = chrono::Utc::now().timestamp_millis();
    let scheduled = next_at.load(Ordering::SeqCst);
    now >= scheduled
        && next_at
            .compare_exchange(scheduled, now + 1_000, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
}

/// The durable heartbeat, plus the sticky maintenance-failure check and the rate-gated approval
/// maintenance branch. It also runs `repair_pending_delivery_audits`, which drains the outbox
/// worker's own audit-repair queue (`deployment_outbox_delivery_audit_repairs`) rather than
/// reconciling approvals; both happen from this one entry point.
pub async fn record_worker_heartbeat(
    db: &DatabaseConnection,
    worker: &str,
    delivered: i32,
    ready: bool,
    failure_code: Option<&str>,
    next_approval_maintenance_at: &std::sync::atomic::AtomicI64,
) -> Result<(), DbErr> {
    if worker.trim().is_empty() {
        return Ok(());
    }
    let maintenance_failed = ready
        && failure_code.is_none()
        && super::approval::approval_maintenance_failed(db, worker).await?;
    let recovery_failed =
        ready && failure_code.is_none() && !repair_pending_delivery_audits(db).await;
    let reported_failure = if maintenance_failed {
        Some("APPROVAL_MAINTENANCE_FAILED")
    } else if recovery_failed {
        Some("OUTBOX_RECOVERY_AUDIT_FAILED")
    } else {
        failure_code
    };
    write_worker_heartbeat(
        db,
        worker,
        delivered,
        reported_failure.is_none() && ready,
        reported_failure,
    )
    .await?;

    if ready && approval_maintenance_due(next_approval_maintenance_at) {
        let healthy = match super::approval::reconcile_expired_approval_requirements(db).await {
            Ok(health) => health.healthy,
            Err(_) => false,
        };
        let succeeded = healthy && super::approval::release_compatible_approval_handoffs(db).await;
        write_worker_heartbeat(
            db,
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

#[cfg(test)]
mod payload_tests {
    use super::{db_failure_code, decode_mode, WorkerMode};
    use sea_orm::DbErr;
    use serde_json::json;

    /// The payload arrives from a row this worker wrote, but a drifted or hand-edited one must
    /// land on `INVALID` rather than on any executing mode. Asserted through `name`, the
    /// vocabulary the outbox row and the heartbeat both carry.
    #[test]
    fn only_the_four_named_modes_decode() {
        for text in ["SUCCESS", "FAILURE", "RETRY", "POISON"] {
            assert_eq!(decode_mode(&json!({ "mode": text })).name(), text);
        }
    }

    #[test]
    fn a_payload_with_no_recognizable_mode_is_invalid() {
        for payload in [
            json!({}),
            json!({ "mode": null }),
            json!({ "mode": "success" }),
            json!({ "mode": 1 }),
            json!({ "mode": ["SUCCESS"] }),
            json!("SUCCESS"),
            json!(null),
        ] {
            assert_eq!(decode_mode(&payload).name(), "INVALID", "{payload}");
            assert!(decode_mode(&payload) == WorkerMode::Invalid, "{payload}");
        }
    }

    /// The heartbeat code has to survive a `CHECK` on an uppercase code pattern, so anything that
    /// is not a five-character SQLSTATE becomes the generic code rather than leaking the driver's
    /// own message.
    #[test]
    fn an_error_that_carries_no_sqlstate_reports_the_generic_code() {
        for error in [
            DbErr::Custom("boom".to_string()),
            DbErr::Type("boom".to_string()),
            DbErr::RecordNotFound("boom".to_string()),
            DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout),
        ] {
            assert_eq!(db_failure_code(&error), "DATABASE_FAILURE", "{error:?}");
        }
    }
}
