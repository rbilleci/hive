use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use hive_domain::health_timestamp_string;
use hive_persistence::worker_health::{deployment_worker_health, evaluation_worker_health};
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HealthResponse {
    status: &'static str,
    approval_maintenance: &'static str,
    approval_maintenance_attempted: i64,
    approval_maintenance_reconciled: i64,
    approval_maintenance_failed: i64,
    approval_upgrade_maintenance: &'static str,
    graphql_requests: u64,
    graphql_failures: u64,
    graphql_unavailable: u64,
    graphql_last_duration_nanos: u64,
    graphql_max_duration_nanos: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval_maintenance_failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval_upgrade_maintenance_failure_code: Option<String>,
}

pub async fn health(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let approval = state.approval_maintenance.status();
    let upgrade = state.approval_maintenance.upgrade();
    let telemetry = &state.telemetry;

    let body = HealthResponse {
        status: if approval.healthy { "ok" } else { "degraded" },
        approval_maintenance: if approval.healthy { "ok" } else { "failed" },
        approval_maintenance_attempted: approval.attempted,
        approval_maintenance_reconciled: approval.reconciled,
        approval_maintenance_failed: approval.failed,
        approval_upgrade_maintenance: if upgrade.healthy { "ok" } else { "failed" },
        graphql_requests: telemetry.completed(),
        graphql_failures: telemetry.failed(),
        graphql_unavailable: telemetry.unavailable(),
        graphql_last_duration_nanos: telemetry.last_duration_nanos(),
        graphql_max_duration_nanos: telemetry.max_duration_nanos(),
        approval_maintenance_failure_code: (!approval.healthy)
            .then_some(approval.failure_code)
            .flatten(),
        approval_upgrade_maintenance_failure_code: (!upgrade.healthy)
            .then_some(upgrade.failure_code)
            .flatten(),
    };

    let status = if approval.healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeploymentWorkerStatusResponse {
    status: &'static str,
    worker_id: Option<String>,
    observed_at: Option<String>,
    pending_events: i32,
    oldest_pending_at: Option<String>,
    detail: String,
    pending_approval_handoffs: i32,
    oldest_approval_handoff_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_code: Option<String>,
}

pub async fn deployment_worker_status(
    State(state): State<AppState>,
) -> (StatusCode, Json<DeploymentWorkerStatusResponse>) {
    let health = deployment_worker_health(&state.db, state.deployment_worker_stale_millis).await;

    let status = if health.status == "READY" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = DeploymentWorkerStatusResponse {
        status: health.status,
        worker_id: health.worker_id,
        observed_at: health.observed_at.map(health_timestamp_string),
        pending_events: health.pending_events,
        oldest_pending_at: health.oldest_pending_at.map(health_timestamp_string),
        detail: health.detail,
        pending_approval_handoffs: health.pending_approval_handoffs,
        oldest_approval_handoff_at: health
            .oldest_approval_handoff_at
            .map(health_timestamp_string),
        failure_code: health.failure_code,
    };
    (status, Json(body))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EvaluationWorkerStatusResponse {
    status: &'static str,
    pending_events: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_code: Option<String>,
}

pub async fn evaluation_worker_status(
    State(state): State<AppState>,
) -> (StatusCode, Json<EvaluationWorkerStatusResponse>) {
    let health = evaluation_worker_health(&state.db).await;

    let status = if health.status == "READY" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = EvaluationWorkerStatusResponse {
        status: health.status,
        pending_events: health.pending_events,
        failure_code: health.failure_code,
    };
    (status, Json(body))
}
