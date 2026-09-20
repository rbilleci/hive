//! Ports `LocalEvaluationWorkDecider`: applies the deterministic local
//! adapter and immutable document policy without persistence knowledge.

use super::document;
use super::fixture::EvaluationFixturePort;
use super::models::{EvaluationFinalizationDecision, EvaluationWorkDecision, EvaluationWorkItem};
use super::scoring;
use super::state_machine::EvaluationRunStatus;

/// The two decide-time inconsistencies `decide()` can hit. `run_once`'s only caller currently
/// converts this to a display string either way (`anyhow::anyhow!(error)`, preserving the message
/// text for the eventual `store.failed(...)` report), but a typed enum documents the closed set of
/// failure kinds at the function signature instead of leaving it implicit in a bare `String`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecideError {
    #[error("the durable case claim has no frozen definition case")]
    MissingCaseDefinition,
    #[error("the local worker received an unknown event type `{0}`")]
    UnknownEventType(String),
}

/// Java lets a decide-time inconsistency (an unknown event type, or a case ordinal absent from its
/// own frozen document) throw and fall into `LocalEvaluationWorker.runOnce()`'s catch-all, which
/// commits it as a `store.failed(...)` RUNNER_FAILED rather than crashing the worker process. `Err`
/// here — not a panic — lets `run_once` reach that same outcome without `catch_unwind`.
pub fn decide(
    work: &EvaluationWorkItem,
    fixtures: &dyn EvaluationFixturePort,
) -> Result<EvaluationWorkDecision, DecideError> {
    match work.event_type.as_str() {
        "START" => {
            let lifecycle_status = if work.current_lifecycle_status.may_start_run() {
                EvaluationRunStatus::Running
            } else {
                work.current_lifecycle_status
            };
            Ok(EvaluationWorkDecision::Start { lifecycle_status })
        }
        "CASE" => {
            let definition_case = document::cases(&work.canonical_document)
                .into_iter()
                .find(|candidate| candidate.ordinal == work.case_ordinal)
                .ok_or(DecideError::MissingCaseDefinition)?;
            let result = fixtures.execute_with_document(&work.canonical_document, &definition_case);
            Ok(EvaluationWorkDecision::Case(scoring::classify(
                &definition_case,
                &result,
            )))
        }
        "FINALIZE" => {
            let metrics = document::metrics(&work.canonical_document);
            let score = scoring::score(&work.completed_cases, &metrics);
            let passed = score.passed;
            Ok(EvaluationWorkDecision::Finalize(
                EvaluationFinalizationDecision {
                    metrics: score.metrics,
                    passed,
                    outcome_category: if passed { "PASSED" } else { "CASE_FAILED" }.to_string(),
                    lifecycle_status: EvaluationRunStatus::Completed,
                    summary_digest_material: format!("{:.8}", score.exact_match_rate),
                },
            ))
        }
        other => Err(DecideError::UnknownEventType(other.to_string())),
    }
}
