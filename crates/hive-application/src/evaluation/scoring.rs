//! Ports `EvaluationScoringPolicy`: local exact-match case decisions and
//! aggregate metrics computed from frozen facts.

use super::document::{CaseDefinition, MetricDefinition};
use super::fixture::EvaluationFixtureResult;
use super::models::EvaluationExecutionDecision;

pub fn classify(
    definition: &CaseDefinition,
    result: &EvaluationFixtureResult,
) -> EvaluationExecutionDecision {
    match result {
        EvaluationFixtureResult::Output(value) => {
            if definition.expected_output == *value {
                EvaluationExecutionDecision::success()
            } else {
                EvaluationExecutionDecision::case_failure()
            }
        }
        EvaluationFixtureResult::TargetFailure(code) => {
            EvaluationExecutionDecision::target_failure(code)
        }
        EvaluationFixtureResult::RunnerFailure(code) => {
            EvaluationExecutionDecision::runner_failure(code)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Metric {
    pub code: String,
    pub value: f64,
    pub threshold: f64,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub exact_match_rate: f64,
    pub metrics: Vec<Metric>,
    pub passed: bool,
}

/// Ports the `BigDecimal`-scaled rate (8-scale, `HALF_UP`) as a plain `f64` rounded the same way;
/// `f64` carries plenty of precision for a 0..=1 rate over at most 100 cases (see `MAX_CASES`).
pub fn score(completed_cases: &[Option<bool>], metrics: &[MetricDefinition]) -> Score {
    if completed_cases.is_empty() || completed_cases.iter().any(Option::is_none) {
        return Score {
            exact_match_rate: 0.0,
            metrics: Vec::new(),
            passed: false,
        };
    }
    let matched = completed_cases
        .iter()
        .filter(|value| **value == Some(true))
        .count();
    let rate = round_half_up(matched as f64 / completed_cases.len() as f64, 8);
    let results: Vec<Metric> = metrics
        .iter()
        .map(|metric| Metric {
            code: metric.code.clone(),
            value: rate,
            threshold: metric.threshold,
            passed: rate >= metric.threshold,
        })
        .collect();
    Score {
        exact_match_rate: rate,
        passed: matched == completed_cases.len() && results.iter().all(|metric| metric.passed),
        metrics: results,
    }
}

fn round_half_up(value: f64, scale: i32) -> f64 {
    let factor = 10f64.powi(scale);
    (value * factor).round() / factor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::document::MetricDefinition;
    use crate::evaluation::state_machine::EvaluationRunStatus;

    fn metric(code: &str, threshold: f64) -> MetricDefinition {
        MetricDefinition {
            code: code.to_string(),
            threshold,
        }
    }

    #[test]
    fn classify_matches_exact_output() {
        let case = crate::evaluation::document::CaseDefinition {
            ordinal: 1,
            key: "a".to_string(),
            prompt: "p".to_string(),
            expected_output: "ready".to_string(),
            fixture_output: Some("ready".to_string()),
            target_failure_code: None,
        };
        let decision = classify(&case, &EvaluationFixtureResult::Output("ready".to_string()));
        assert_eq!(decision.lifecycle_status, EvaluationRunStatus::Completed);
        assert_eq!(decision.passed, Some(true));
    }

    #[test]
    fn classify_fails_on_mismatched_output() {
        let case = crate::evaluation::document::CaseDefinition {
            ordinal: 1,
            key: "a".to_string(),
            prompt: "p".to_string(),
            expected_output: "ready".to_string(),
            fixture_output: Some("not ready".to_string()),
            target_failure_code: None,
        };
        let decision = classify(
            &case,
            &EvaluationFixtureResult::Output("not ready".to_string()),
        );
        assert_eq!(decision.lifecycle_status, EvaluationRunStatus::Failed);
        assert_eq!(decision.outcome_category.as_deref(), Some("CASE_FAILED"));
        assert!(!decision.terminal_run);
    }

    #[test]
    fn classify_marks_target_and_runner_failures_terminal() {
        let case = crate::evaluation::document::CaseDefinition {
            ordinal: 1,
            key: "a".to_string(),
            prompt: "p".to_string(),
            expected_output: "ready".to_string(),
            fixture_output: None,
            target_failure_code: Some("TARGET_DOWN".to_string()),
        };
        let decision = classify(
            &case,
            &EvaluationFixtureResult::TargetFailure("TARGET_DOWN".to_string()),
        );
        assert!(decision.terminal_run);
        assert_eq!(decision.outcome_category.as_deref(), Some("TARGET_FAILED"));
    }

    #[test]
    fn score_computes_exact_match_rate_and_per_metric_pass() {
        // `Score.passed` (the overall run outcome) requires every case to match, not just the
        // metric's own threshold: Java's `matched == completedCases.size() && ...allMatch(passed)`.
        // A metric can pass its threshold (0.75 >= 0.5) while the run overall still does not.
        let completed = vec![Some(true), Some(true), Some(false), Some(true)];
        let metrics = vec![metric("EXACT_MATCH_RATE", 0.5)];
        let score = score(&completed, &metrics);
        assert_eq!(score.exact_match_rate, 0.75);
        assert!(!score.passed);
        assert_eq!(score.metrics.len(), 1);
        assert!(score.metrics[0].passed);
    }

    #[test]
    fn score_passes_only_when_every_case_matches() {
        let completed = vec![Some(true), Some(true), Some(true)];
        let metrics = vec![metric("EXACT_MATCH_RATE", 1.0)];
        let score = score(&completed, &metrics);
        assert_eq!(score.exact_match_rate, 1.0);
        assert!(score.passed);
    }

    #[test]
    fn score_fails_when_any_case_is_incomplete() {
        let completed = vec![Some(true), None];
        let metrics = vec![metric("EXACT_MATCH_RATE", 0.0)];
        let score = score(&completed, &metrics);
        assert_eq!(score.exact_match_rate, 0.0);
        assert!(!score.passed);
        assert!(score.metrics.is_empty());
    }

    #[test]
    fn score_fails_on_an_empty_case_list() {
        let score = score(&[], &[metric("EXACT_MATCH_RATE", 0.0)]);
        assert!(!score.passed);
        assert_eq!(score.exact_match_rate, 0.0);
    }

    #[test]
    fn score_requires_every_metric_to_pass() {
        let completed = vec![Some(true), Some(false)];
        let metrics = vec![metric("EXACT_MATCH_RATE", 0.9)];
        let score = score(&completed, &metrics);
        assert_eq!(score.exact_match_rate, 0.5);
        assert!(!score.passed);
        assert!(!score.metrics[0].passed);
    }
}
