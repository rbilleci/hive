//! Ports the evaluation records the *commands* and the local worker still exchange:
//! definitions, drafts, versions, runs and the mutation-result/problem types the repository
//! trait returns. The read projections and their connections went with the hand-built queries:
//! every evaluation read is a generated Seaography entity query now.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::document::EvaluationDiagnostic;
use super::scoring::Metric;
use super::state_machine::EvaluationRunStatus;

#[derive(Debug, Clone)]
pub struct EvaluationDefinitionDraft {
    pub definition_id: Uuid,
    pub canonical_document: String,
    pub revision: i64,
    pub validation_status: String,
    pub diagnostics: Vec<EvaluationDiagnostic>,
    pub based_on_version_id: Option<Uuid>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct EvaluationDefinitionVersion {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub number: i64,
    pub canonical_document: String,
    pub content_digest: String,
    pub based_on_version_id: Option<Uuid>,
    pub published_by: Uuid,
    pub published_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct EvaluationDefinition {
    pub id: Uuid,
    pub project_id: Uuid,
    pub slug: String,
    pub lifecycle_status: String,
    pub draft: EvaluationDefinitionDraft,
    pub latest_version: Option<EvaluationDefinitionVersion>,
    pub can_author: bool,
    pub can_publish: bool,
    pub created_at: DateTime<Utc>,
}

/// `deployment_id`/`target_digest`/`plan_digest`/`package_digest`/`binding_digest` are `None` for
/// an `AGENT_VERSION` target: only a `DEPLOYMENT` target sources them from a policy snapshot (see
/// `PostgresEvaluationRepository.target()`'s two branches).
#[derive(Debug, Clone)]
pub struct EvaluationTargetSnapshot {
    pub agent_version_id: Uuid,
    pub deployment_id: Option<Uuid>,
    pub environment_definition_version_id: Uuid,
    pub logical_environment_class: String,
    pub agent_content_digest: String,
    pub target_digest: Option<String>,
    pub plan_digest: Option<String>,
    pub package_digest: Option<String>,
    pub binding_digest: Option<String>,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub environment_content_digest: String,
}

/// The run itself never carries its nested `cases`/`metrics`/`artifacts`/`audit` collections
/// (those are separate, independently paginated GraphQL connection fields, mirroring
/// `EvaluationRun`'s Java shape where the resolver — not this record — loads them); `target` is
/// `Option` because `runRows()`'s SQL is a `LEFT JOIN` against `evaluation_target_snapshots`, even
/// though in steady state `insertTarget` always populates it in the same transaction as the run.
#[derive(Debug, Clone)]
pub struct EvaluationRun {
    pub id: Uuid,
    pub project_id: Uuid,
    pub definition_version_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub environment_definition_version_id: Uuid,
    pub source_run_id: Option<Uuid>,
    pub lifecycle_status: EvaluationRunStatus,
    pub generation: i64,
    pub outcome_category: Option<String>,
    pub outcome_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub target: Option<EvaluationTargetSnapshot>,
    pub deployment_evidence_disposition: String,
}

impl EvaluationRun {
    pub fn duration_millis(&self) -> Option<i64> {
        super::outcome::duration_millis(self.started_at, self.completed_at)
    }

    pub fn failure_summary(&self) -> Option<String> {
        super::outcome::failure_summary(self.lifecycle_status, self.outcome_category.as_deref())
    }
}

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

/// Ports `EvaluationMutationResult`: a successful definition or run result, or exactly one typed
/// refusal.
#[derive(Debug, Clone)]
pub struct EvaluationMutationResult {
    pub definition: Option<EvaluationDefinition>,
    pub version: Option<EvaluationDefinitionVersion>,
    pub run: Option<EvaluationRun>,
    pub problem: Option<EvaluationProblem>,
}

impl EvaluationMutationResult {
    pub fn definition(value: EvaluationDefinition) -> Self {
        Self {
            definition: Some(value),
            version: None,
            run: None,
            problem: None,
        }
    }

    pub fn version(definition: EvaluationDefinition, version: EvaluationDefinitionVersion) -> Self {
        Self {
            definition: Some(definition),
            version: Some(version),
            run: None,
            problem: None,
        }
    }

    pub fn run(value: EvaluationRun) -> Self {
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
