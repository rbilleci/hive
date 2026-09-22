//! Redacted, domain-owned terminal summaries from retained run facts.

use chrono::{DateTime, Utc};

/// Why a terminal run ended: `evaluation_runs.outcome_category` and
/// `evaluation_results.outcome_category`. A decision names a variant, so persistence stores it
/// without parsing a string back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvaluationOutcomeCategory {
    Passed,
    CaseFailed,
    TargetFailed,
    RunnerFailed,
    Canceled,
}

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
    outcome_category: Option<EvaluationOutcomeCategory>,
) -> Option<String> {
    if !lifecycle_status.is_terminal() {
        return None;
    }
    let summary = match outcome_category? {
        EvaluationOutcomeCategory::Passed => return None,
        EvaluationOutcomeCategory::CaseFailed => {
            "One or more evaluation cases did not match the expected result."
        }
        EvaluationOutcomeCategory::TargetFailed => {
            "The frozen target could not complete the evaluation."
        }
        EvaluationOutcomeCategory::RunnerFailed => {
            "The local evaluation worker could not complete the evaluation."
        }
        EvaluationOutcomeCategory::Canceled => "An authorized operator canceled the evaluation.",
    };
    Some(summary.to_string())
}
