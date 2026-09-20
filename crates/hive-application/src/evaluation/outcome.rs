//! Ports `EvaluationOutcomeSummary`: redacted, domain-owned terminal summaries
//! from retained run facts.

use chrono::{DateTime, Utc};

pub fn duration_millis(
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
) -> Option<i64> {
    let (started_at, completed_at) = (started_at?, completed_at?);
    if completed_at < started_at {
        return None;
    }
    Some((completed_at - started_at).num_milliseconds())
}

pub fn failure_summary(
    lifecycle_status: super::state_machine::EvaluationRunStatus,
    outcome_category: Option<&str>,
) -> Option<String> {
    if !lifecycle_status.is_terminal() || outcome_category == Some("PASSED") {
        return None;
    }
    let summary = match outcome_category? {
        "CASE_FAILED" => "One or more evaluation cases did not match the expected result.",
        "TARGET_FAILED" => "The frozen target could not complete the evaluation.",
        "RUNNER_FAILED" => "The local evaluation worker could not complete the evaluation.",
        "CANCELED" => "An authorized operator canceled the evaluation.",
        _ => return None,
    };
    Some(summary.to_string())
}
