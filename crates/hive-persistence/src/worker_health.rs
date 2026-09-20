//! Ports `PostgresDeploymentRepository.workerHealth` and
//! `PostgresEvaluationRepository.workerHealth` verbatim. Self-contained queries
//! against `deployment_worker_heartbeats` / `deployment_approval_handoff_releases`
//! and `evaluation_outbox_events` / `evaluation_worker_heartbeats`; ported ahead of
//! the rest of their owning repositories (RTP-DEPLOYMENT, RTP-EVALUATION) because
//! `GET /health/*` needs them from RTP-BOOTSTRAP onward.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

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

/// Mirrors `PostgresDeploymentRepository.workerHealth(long staleMillis)`.
pub async fn deployment_worker_health(
    db: &DatabaseConnection,
    stale_millis: i64,
) -> DeploymentWorkerHealth {
    let threshold = stale_millis.max(1);

    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "WITH latest AS (\
            SELECT worker_id, observed_at, last_batch_deliveries, pending_events, oldest_pending_at, state, failure_code, approval_execution_compatible \
            FROM deployment_worker_heartbeats ORDER BY approval_execution_compatible DESC, observed_at DESC LIMIT 1 \
         ), handoffs AS (\
            SELECT COUNT(*)::integer AS count, MIN(created_at) AS oldest FROM (\
                SELECT created_at FROM deployment_approval_handoff_releases ORDER BY created_at ASC LIMIT 51 \
            ) bounded \
         ) \
         SELECT latest.worker_id, latest.observed_at, latest.pending_events, latest.oldest_pending_at, \
            latest.state, latest.failure_code, latest.approval_execution_compatible, \
            (EXTRACT(EPOCH FROM CURRENT_TIMESTAMP - latest.observed_at) * 1000)::double precision AS age_millis, \
            handoffs.count AS pending_handoffs, handoffs.oldest AS oldest_handoff_at \
         FROM handoffs LEFT JOIN latest ON TRUE",
        [],
    );
    let row = match db.query_one_raw(statement).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return DeploymentWorkerHealth::unavailable(
                "Worker health storage did not return a snapshot.",
            )
        }
        Err(_) => {
            return DeploymentWorkerHealth::unavailable("Worker health storage is unavailable.")
        }
    };

    let state: Option<String> = match row.try_get_by("state") {
        Ok(value) => value,
        Err(_) => {
            return DeploymentWorkerHealth::unavailable("Worker health storage is unavailable.")
        }
    };
    let compatible: Option<bool> = row
        .try_get_by("approval_execution_compatible")
        .unwrap_or(None);
    let compatible = compatible.unwrap_or(false);
    let pending_handoffs: i32 = row.try_get_by("pending_handoffs").unwrap_or(0);
    let handoff_backlog = pending_handoffs >= 50;
    let heartbeat_present = state.is_some();
    let age_millis: Option<f64> = row.try_get_by("age_millis").unwrap_or(None);
    let age = age_millis
        .map(|value| value.round().max(0.0) as i64)
        .unwrap_or(i64::MAX);
    let ready = heartbeat_present
        && state.as_deref() == Some("READY")
        && compatible
        && age <= threshold
        && !handoff_backlog;
    let failure_code: Option<String> = row.try_get_by("failure_code").unwrap_or(None);

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
        worker_id: row.try_get_by("worker_id").unwrap_or(None),
        observed_at: row.try_get_by("observed_at").unwrap_or(None),
        pending_events: row.try_get_by("pending_events").unwrap_or(0),
        oldest_pending_at: row.try_get_by("oldest_pending_at").unwrap_or(None),
        pending_approval_handoffs: pending_handoffs,
        oldest_approval_handoff_at: row.try_get_by("oldest_handoff_at").unwrap_or(None),
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

/// Mirrors `PostgresEvaluationRepository.workerHealth()`.
pub async fn evaluation_worker_health(db: &DatabaseConnection) -> EvaluationWorkerHealth {
    let backlog_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT count(*) AS count, bool_or(status = 'PROCESSING' AND claimed_at < CURRENT_TIMESTAMP - INTERVAL '30 seconds') AS stranded \
         FROM evaluation_outbox_events \
         WHERE status = 'PENDING' OR (status = 'PROCESSING' AND claimed_at < CURRENT_TIMESTAMP - INTERVAL '30 seconds')",
        [],
    );
    let backlog = match db.query_one_raw(backlog_statement).await {
        Ok(Some(row)) => row,
        _ => {
            return EvaluationWorkerHealth {
                status: "UNAVAILABLE",
                pending_events: 0,
                failure_code: Some("DATABASE_UNAVAILABLE".to_string()),
            }
        }
    };

    let pending: i32 = backlog
        .try_get_by::<i64, _>("count")
        .map(|value| value as i32)
        .unwrap_or(0);
    let stranded: bool = backlog
        .try_get_by("stranded")
        .unwrap_or(None)
        .unwrap_or(false);

    let heartbeat_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT status, last_failure_code, observed_at >= CURRENT_TIMESTAMP - INTERVAL '30 seconds' AS fresh \
         FROM evaluation_worker_heartbeats ORDER BY observed_at DESC LIMIT 1",
        [],
    );
    let heartbeat = db.query_one_raw(heartbeat_statement).await.ok().flatten();

    let Some(heartbeat) = heartbeat else {
        return EvaluationWorkerHealth {
            status: "STALE",
            pending_events: pending,
            failure_code: Some("WORKER_STALE".to_string()),
        };
    };

    let fresh: bool = heartbeat.try_get_by("fresh").unwrap_or(false);
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

    let status: String = heartbeat.try_get_by("status").unwrap_or_default();
    if status == "DEGRADED" {
        let last_failure_code: Option<String> =
            heartbeat.try_get_by("last_failure_code").unwrap_or(None);
        return EvaluationWorkerHealth {
            status: "DEGRADED",
            pending_events: pending,
            failure_code: Some(last_failure_code.unwrap_or_else(|| "RUNNER_FAILED".to_string())),
        };
    }

    EvaluationWorkerHealth {
        status: "READY",
        pending_events: pending,
        failure_code: None,
    }
}
