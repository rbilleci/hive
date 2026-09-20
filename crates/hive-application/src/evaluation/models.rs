//! What the evaluation *commands* and the local worker exchange: the work items and decisions the
//! worker commits, and the mutation-result/problem types the repository trait returns. A command
//! answers with the persistence layer's own rows, so the result is generic over them; every
//! evaluation read is a generated Seaography entity query.

use uuid::Uuid;

use super::scoring::Metric;
use super::state_machine::EvaluationRunStatus;

/// Ports `EvaluationExecutionDecision`: a domain-owned terminal case decision a durable work store
/// may commit after rechecking its claim.
#[derive(Debug, Clone)]
pub struct EvaluationExecutionDecision {
    pub lifecycle_status: EvaluationRunStatus,
    pub passed: Option<bool>,
    pub outcome_category: Option<String>,
    pub outcome_code: Option<String>,
    pub terminal_run: bool,
}

impl EvaluationExecutionDecision {
    pub fn success() -> Self {
        Self {
            lifecycle_status: EvaluationRunStatus::Completed,
            passed: Some(true),
            outcome_category: None,
            outcome_code: None,
            terminal_run: false,
        }
    }

    pub fn case_failure() -> Self {
        Self {
            lifecycle_status: EvaluationRunStatus::Failed,
            passed: Some(false),
            outcome_category: Some("CASE_FAILED".to_string()),
            outcome_code: Some("EXACT_MATCH_FAILED".to_string()),
            terminal_run: false,
        }
    }

    pub fn target_failure(code: &str) -> Self {
        Self {
            lifecycle_status: EvaluationRunStatus::Failed,
            passed: Some(false),
            outcome_category: Some("TARGET_FAILED".to_string()),
            outcome_code: Some(code.to_string()),
            terminal_run: true,
        }
    }

    pub fn runner_failure(code: &str) -> Self {
        Self {
            lifecycle_status: EvaluationRunStatus::Failed,
            passed: Some(false),
            outcome_category: Some("RUNNER_FAILED".to_string()),
            outcome_code: Some(code.to_string()),
            terminal_run: true,
        }
    }
}

/// Ports `EvaluationFinalizationDecision`.
#[derive(Debug, Clone)]
pub struct EvaluationFinalizationDecision {
    pub metrics: Vec<Metric>,
    pub passed: bool,
    pub outcome_category: String,
    pub lifecycle_status: EvaluationRunStatus,
    pub summary_digest_material: String,
}

/// Ports `EvaluationWorkDecision`: a pure local-work result persistence may commit after
/// validating its durable claim.
#[derive(Debug, Clone)]
pub enum EvaluationWorkDecision {
    Start {
        lifecycle_status: EvaluationRunStatus,
    },
    Case(EvaluationExecutionDecision),
    Finalize(EvaluationFinalizationDecision),
}

/// Ports `EvaluationWorkItem`: immutable work claim data valid only with its durable claim
/// identity and generation.
#[derive(Debug, Clone)]
pub struct EvaluationWorkItem {
    pub event_id: Uuid,
    pub run_id: Uuid,
    pub case_run_id: Option<Uuid>,
    pub event_type: String,
    pub generation: i64,
    pub attempt: i32,
    pub canonical_document: String,
    pub case_ordinal: i32,
    pub completed_cases: Vec<Option<bool>>,
    pub current_lifecycle_status: EvaluationRunStatus,
}

/// Ports `EvaluationProblem`: a transport-neutral refusal that keeps hidden resources
/// indistinguishable from missing resources.
#[derive(Debug, Clone)]
pub struct EvaluationProblem {
    pub kind: EvaluationProblemKind,
    pub resource_id: Option<Uuid>,
    pub expected_revision: i64,
    pub actual_revision: i64,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationProblemKind {
    NotFound,
    Forbidden,
    Validation,
    RevisionConflict,
    LifecycleConflict,
    IdempotencyConflict,
    TargetIncompatible,
    Unavailable,
}

impl EvaluationProblem {
    pub fn not_found() -> Self {
        Self {
            kind: EvaluationProblemKind::NotFound,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
            message: "The requested evaluation resource is unavailable.".to_string(),
        }
    }

    pub fn forbidden() -> Self {
        Self {
            kind: EvaluationProblemKind::Forbidden,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
            message: "The current authority cannot perform this evaluation action.".to_string(),
        }
    }

    pub fn validation() -> Self {
        Self {
            kind: EvaluationProblemKind::Validation,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
            message: "The evaluation definition contains validation errors.".to_string(),
        }
    }

    pub fn conflict(id: Uuid, expected: i64, actual: i64) -> Self {
        Self {
            kind: EvaluationProblemKind::RevisionConflict,
            resource_id: Some(id),
            expected_revision: expected,
            actual_revision: actual,
            message: "The evaluation draft changed before this request.".to_string(),
        }
    }

    pub fn lifecycle() -> Self {
        Self {
            kind: EvaluationProblemKind::LifecycleConflict,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
            message: "The evaluation run is not eligible for this transition.".to_string(),
        }
    }

    /// Java constructs this inline in `transaction()`'s `IdempotencyException` catch block rather
    /// than via a named factory on `EvaluationProblem` — the message text below is copied verbatim
    /// from that call site, not invented.
    pub fn idempotency() -> Self {
        Self {
            kind: EvaluationProblemKind::IdempotencyConflict,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
            message: "The idempotency key belongs to a different request.".to_string(),
        }
    }

    pub fn target() -> Self {
        Self {
            kind: EvaluationProblemKind::TargetIncompatible,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
            message: "The immutable target does not satisfy the published definition contract."
                .to_string(),
        }
    }

    pub fn unavailable() -> Self {
        Self {
            kind: EvaluationProblemKind::Unavailable,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
            message: "The local evaluation service is unavailable.".to_string(),
        }
    }
}

/// Ports `EvaluationMutationResult`: the row a command left behind, or exactly one typed refusal.
/// `D`, `V` and `R` are the persistence layer's stored definition, version and run rows.
#[derive(Debug, Clone)]
pub struct EvaluationMutationResult<D, V, R> {
    pub definition: Option<D>,
    pub version: Option<V>,
    pub run: Option<R>,
    pub problem: Option<EvaluationProblem>,
}

impl<D, V, R> EvaluationMutationResult<D, V, R> {
    pub fn definition(value: D) -> Self {
        Self {
            definition: Some(value),
            version: None,
            run: None,
            problem: None,
        }
    }

    pub fn version(definition: D, version: V) -> Self {
        Self {
            definition: Some(definition),
            version: Some(version),
            run: None,
            problem: None,
        }
    }

    pub fn run(value: R) -> Self {
        Self {
            definition: None,
            version: None,
            run: Some(value),
            problem: None,
        }
    }

    pub fn refused(value: EvaluationProblem) -> Self {
        Self {
            definition: None,
            version: None,
            run: None,
            problem: Some(value),
        }
    }
}

/// Ports `EvaluationRepository.WorkerHealth`.
#[derive(Debug, Clone)]
pub struct WorkerHealth {
    pub status: String,
    pub pending_events: i32,
    pub failure_code: Option<String>,
}
