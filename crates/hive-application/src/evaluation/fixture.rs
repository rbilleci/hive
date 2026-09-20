//! Ports `EvaluationFixtureResult` (closed local-adapter outcomes) and
//! `EvaluationFixturePort`/`LocalPromptCaseFixtureAdapter`: the only
//! executable local evaluation adapter, a deterministic fixture-backed test
//! double with no network call, model client, or async behavior — it reads
//! outcomes directly out of the frozen JSON document's `fixture` object.

use super::document::CaseDefinition;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationFixtureResult {
    Output(String),
    TargetFailure(String),
    RunnerFailure(String),
}

pub trait EvaluationFixturePort: Send + Sync {
    fn execute(&self, definition: &CaseDefinition) -> EvaluationFixtureResult {
        if let Some(code) = &definition.target_failure_code {
            return EvaluationFixtureResult::TargetFailure(code.clone());
        }
        match &definition.fixture_output {
            Some(output) => EvaluationFixtureResult::Output(output.clone()),
            None => EvaluationFixtureResult::RunnerFailure("MALFORMED_FIXTURE".to_string()),
        }
    }

    fn execute_with_document(
        &self,
        canonical_document: &str,
        definition: &CaseDefinition,
    ) -> EvaluationFixtureResult {
        if super::document::runner_failure_fixture(canonical_document) {
            return EvaluationFixtureResult::RunnerFailure("RUNNER_FIXTURE".to_string());
        }
        self.execute(definition)
    }
}

pub struct LocalPromptCaseFixtureAdapter;

impl EvaluationFixturePort for LocalPromptCaseFixtureAdapter {}
