use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Datelike, Timelike, Utc};
use hive_persistence::worker_health::{deployment_worker_health, evaluation_worker_health};
use serde::Serialize;

/// The timestamp form the `/health` responses carry: `Z` for the zero UTC offset, never
/// `+00:00`; seconds omitted on a whole minute; and a fraction of 3, 6 or 9 digits, whichever is
/// the shortest that loses nothing. The tests below pin every one of those cases.
fn health_timestamp_string(value: DateTime<Utc>) -> String {
    let mut out = String::with_capacity(30);

    out.push_str(&format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        value.year(),
        value.month(),
        value.day(),
        value.hour(),
        value.minute()
    ));

    let second = value.second();
    let nanos = value.nanosecond();

    if second > 0 || nanos > 0 {
        out.push_str(&format!(":{second:02}"));
        if nanos > 0 {
            out.push('.');
            if nanos.is_multiple_of(1_000_000) {
                out.push_str(&format!("{:03}", nanos / 1_000_000));
            } else if nanos.is_multiple_of(1_000) {
                out.push_str(&format!("{:06}", nanos / 1_000));
            } else {
                out.push_str(&format!("{nanos:09}"));
            }
        }
    }

    out.push('Z');
    out
}

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

#[cfg(test)]
mod health_timestamp_tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, nano: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s)
            .single()
            .unwrap()
            .with_nanosecond(nano)
            .unwrap()
    }

    #[test]
    fn whole_minute_has_no_seconds_or_fraction() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 0, 0)),
            "2026-09-17T08:05Z"
        );
    }

    #[test]
    fn nonzero_seconds_with_no_fraction() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 30, 0)),
            "2026-09-17T08:05:30Z"
        );
    }

    #[test]
    fn millisecond_precision_prints_three_digits() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 30, 123_000_000)),
            "2026-09-17T08:05:30.123Z"
        );
    }

    #[test]
    fn microsecond_precision_prints_six_digits() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 30, 123_456_000)),
            "2026-09-17T08:05:30.123456Z"
        );
    }

    #[test]
    fn nanosecond_precision_prints_nine_digits() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 30, 123_456_789)),
            "2026-09-17T08:05:30.123456789Z"
        );
    }

    #[test]
    fn zero_second_with_nanos_still_prints_seconds() {
        assert_eq!(
            health_timestamp_string(at(2026, 9, 17, 8, 5, 0, 5_000_000)),
            "2026-09-17T08:05:00.005Z"
        );
    }

    #[test]
    fn single_digit_fields_are_zero_padded() {
        assert_eq!(
            health_timestamp_string(at(2026, 1, 2, 3, 4, 5, 0)),
            "2026-01-02T03:04:05Z"
        );
    }
}
