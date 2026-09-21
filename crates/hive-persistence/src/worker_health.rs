//! Ports `PostgresDeploymentRepository.workerHealth` and
//! `PostgresEvaluationRepository.workerHealth` verbatim. Self-contained queries
//! against `deployment_worker_heartbeats` / `deployment_approval_handoff_releases`
//! and `evaluation_outbox_events` / `evaluation_worker_heartbeats`; they live here, outside
//! their owning repository modules, because `GET /health/*` is their only caller.

use crate::entity::enums::{EvaluationOutboxStatus, WorkerHeartbeatState};
use crate::entity::{
    deployment_approval_handoff_releases, deployment_worker_heartbeats, evaluation_outbox_events,
    evaluation_worker_heartbeats,
};
use chrono::{DateTime, Utc};
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{
    ActiveEnum, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect,
};

#[derive(Debug, Clone)]
pub struct DeploymentWorkerHealth {
    pub status: &'static str,
    pub worker_id: Option<String>,
    pub observed_at: Option<DateTime<Utc>>,
    pub pending_events: i32,
    pub oldest_pending_at: Option<DateTime<Utc>>,
    pub pending_approval_handoffs: i32,
    pub oldest_approval_handoff_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub detail: String,
}

impl DeploymentWorkerHealth {
    fn unavailable(detail: &str) -> Self {
        Self {
            status: "UNAVAILABLE",
            worker_id: None,
            observed_at: None,
            pending_events: 0,
            oldest_pending_at: None,
            pending_approval_handoffs: 0,
            oldest_approval_handoff_at: None,
            failure_code: None,
            detail: detail.to_string(),
        }
    }
}

/// The heartbeat and the handoff backlog the deleted two-CTE statement joined. Read as two entity
/// queries: the heartbeat row its `ORDER BY approval_execution_compatible DESC, observed_at DESC
/// LIMIT 1` selected, and the same bounded 51-row page of handoff releases whose `COUNT(*)` and
/// `MIN(created_at)` are taken in Rust. Both are `/health` reads outside any transaction that held
/// no lock, and `EXTRACT(EPOCH FROM CURRENT_TIMESTAMP - observed_at) * 1000` is the same
/// subtraction on the service clock.
struct DeploymentWorkerSnapshot {
    heartbeat: Option<deployment_worker_heartbeats::Model>,
    pending_handoffs: i32,
    oldest_handoff_at: Option<DateTime<Utc>>,
}

async fn deployment_worker_snapshot(
    db: &DatabaseConnection,
) -> Result<DeploymentWorkerSnapshot, DbErr> {
    let heartbeat = deployment_worker_heartbeats::Entity::find()
        .order_by_desc(deployment_worker_heartbeats::Column::ApprovalExecutionCompatible)
        .order_by_desc(deployment_worker_heartbeats::Column::ObservedAt)
        .one(db)
        .await?;
    let bounded = deployment_approval_handoff_releases::Entity::find()
        .order_by_asc(deployment_approval_handoff_releases::Column::CreatedAt)
        .limit(51)
        .select_only()
        .column(deployment_approval_handoff_releases::Column::CreatedAt)
        .into_tuple::<sea_orm::prelude::DateTimeWithTimeZone>()
        .all(db)
        .await?;
    Ok(DeploymentWorkerSnapshot {
        heartbeat,
        pending_handoffs: i32::try_from(bounded.len()).unwrap_or(i32::MAX),
        oldest_handoff_at: bounded.iter().min().map(|value| value.to_utc()),
    })
}

/// Mirrors `PostgresDeploymentRepository.workerHealth(long staleMillis)`.
pub async fn deployment_worker_health(
    db: &DatabaseConnection,
    stale_millis: i64,
) -> DeploymentWorkerHealth {
    let threshold = Ord::max(stale_millis, 1);

    let Ok(snapshot) = deployment_worker_snapshot(db).await else {
        return DeploymentWorkerHealth::unavailable("Worker health storage is unavailable.");
    };

    let compatible = snapshot
        .heartbeat
        .as_ref()
        .and_then(|row| row.approval_execution_compatible)
        .unwrap_or(false);
    let pending_handoffs = snapshot.pending_handoffs;
    let handoff_backlog = pending_handoffs >= 50;
    let heartbeat_present = snapshot.heartbeat.is_some();
    let age = snapshot.heartbeat.as_ref().map_or(i64::MAX, |row| {
        Ord::max(
            (Utc::now() - row.observed_at.to_utc()).num_milliseconds(),
            0,
        )
    });
    let ready = heartbeat_present
        && snapshot
            .heartbeat
            .as_ref()
            .is_some_and(|row| row.state == WorkerHeartbeatState::Ready)
        && compatible
        && age <= threshold
        && !handoff_backlog;
    let failure_code: Option<String> = snapshot
        .heartbeat
        .as_ref()
        .and_then(|row| row.failure_code.clone());

    let detail = if handoff_backlog {
        "Approval handoff release backlog requires another compatible-worker maintenance pass."
            .to_string()
    } else if ready {
        "The M14-compatible worker heartbeat is current.".to_string()
    } else if !heartbeat_present {
        "No worker heartbeat is recorded.".to_string()
    } else if failure_code.is_some() {
        "The local worker reported a sanitized delivery failure.".to_string()
    } else if compatible {
        "The M14-compatible worker heartbeat is stale or degraded. Restart the local deployment worker.".to_string()
    } else {
        "An M13-compatible worker cannot execute approved handoffs. Start an M14-compatible worker."
            .to_string()
    };

    let status = if ready {
        "READY"
    } else if handoff_backlog || (compatible && age <= threshold) {
        "DEGRADED"
    } else {
        "STALE"
    };

    DeploymentWorkerHealth {
        status,
        worker_id: snapshot.heartbeat.as_ref().map(|row| row.worker_id.clone()),
        observed_at: snapshot
            .heartbeat
            .as_ref()
            .map(|row| row.observed_at.to_utc()),
        pending_events: snapshot
            .heartbeat
            .as_ref()
            .map_or(0, |row| row.pending_events),
        oldest_pending_at: snapshot
            .heartbeat
            .as_ref()
            .and_then(|row| row.oldest_pending_at.map(|value| value.to_utc())),
        pending_approval_handoffs: pending_handoffs,
        oldest_approval_handoff_at: snapshot.oldest_handoff_at,
        failure_code,
        detail,
    }
}

#[derive(Debug, Clone)]
pub struct EvaluationWorkerHealth {
    pub status: &'static str,
    pub pending_events: i32,
    pub failure_code: Option<String>,
}

/// Mirrors `PostgresEvaluationRepository.workerHealth()`. The deleted statement's
/// `count(*)`/`bool_or(...)` over one filtered scan is the same count plus one bounded existence
/// read, and `observed_at >= CURRENT_TIMESTAMP - INTERVAL \'30 seconds\'` is decided in Rust from
/// the row's own instant. Both are `/health` reads that took no lock.
pub async fn evaluation_worker_health(db: &DatabaseConnection) -> EvaluationWorkerHealth {
    let unavailable = || EvaluationWorkerHealth {
        status: "UNAVAILABLE",
        pending_events: 0,
        failure_code: Some("DATABASE_UNAVAILABLE".to_string()),
    };
    let stale_claim_before =
        Expr::current_timestamp().sub(Expr::value("30 seconds").cast_as("interval"));
    let stranded_condition = sea_orm::Condition::all()
        .add(evaluation_outbox_events::Column::Status.eq(EvaluationOutboxStatus::Processing))
        .add(Expr::col(evaluation_outbox_events::Column::ClaimedAt).lt(stale_claim_before.clone()));
    let backlog_condition = sea_orm::Condition::any()
        .add(evaluation_outbox_events::Column::Status.eq(EvaluationOutboxStatus::Pending))
        .add(stranded_condition.clone());
    let Ok(pending) = evaluation_outbox_events::Entity::find()
        .filter(backlog_condition)
        .count(db)
        .await
    else {
        return unavailable();
    };
    let pending = i32::try_from(pending).unwrap_or(i32::MAX);
    let Ok(stranded_row) = evaluation_outbox_events::Entity::find()
        .filter(stranded_condition)
        .select_only()
        .column(evaluation_outbox_events::Column::Id)
        .into_tuple::<uuid::Uuid>()
        .one(db)
        .await
    else {
        return unavailable();
    };
    let stranded = stranded_row.is_some();

    let heartbeat = evaluation_worker_heartbeats::Entity::find()
        .order_by_desc(evaluation_worker_heartbeats::Column::ObservedAt)
        .one(db)
        .await
        .ok()
        .flatten();

    let Some(heartbeat) = heartbeat else {
        return EvaluationWorkerHealth {
            status: "STALE",
            pending_events: pending,
            failure_code: Some("WORKER_STALE".to_string()),
        };
    };

    let fresh = heartbeat.observed_at.to_utc() >= Utc::now() - chrono::Duration::seconds(30);
    if !fresh {
        return EvaluationWorkerHealth {
            status: "STALE",
            pending_events: pending,
            failure_code: Some("WORKER_STALE".to_string()),
        };
    }
    if stranded {
        return EvaluationWorkerHealth {
            status: "STALE",
            pending_events: pending,
            failure_code: Some("LEASE_STALE".to_string()),
        };
    }

    if heartbeat.status.to_value() == "DEGRADED" {
        return EvaluationWorkerHealth {
            status: "DEGRADED",
            pending_events: pending,
            failure_code: Some(
                heartbeat
                    .last_failure_code
                    .unwrap_or_else(|| "RUNNER_FAILED".to_string()),
            ),
        };
    }

    EvaluationWorkerHealth {
        status: "READY",
        pending_events: pending,
        failure_code: None,
    }
}
