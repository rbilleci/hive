//! Ports the plain data records under `dev.hive.application.deployment`:
//! the `Deployment` aggregate and its nested facts, the cursor-paginated
//! connections, the approval-requirement projection, and the mutation-result
//! envelopes. Java's nested records (e.g. `Deployment.Environment`) become
//! separate top-level structs here since Rust has no nested-type namespacing
//! within a struct; names are disambiguated where two Java nested types would
//! otherwise collide (`DeploymentPreview.Environment` -> `PreviewEnvironment`,
//! `DeploymentEnvironmentConnection.Environment` -> `EnvironmentVersion`).

use chrono::{DateTime, Utc};
use hive_domain::deployment::{ApprovalRequirementStatus, DeploymentLifecycleStatus};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentEnvironment {
    pub id: Uuid,
    pub stable_definition_id: String,
    pub version: String,
    pub display_name: String,
    pub logical_environment_class: String,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub content_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentPlanReview {
    pub active_agent_version_number: Option<i64>,
    pub change_summary: String,
    pub added_dependency_versions: Vec<String>,
    pub removed_dependency_versions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentPlan {
    pub agent_version_id: Uuid,
    pub agent_content_digest: String,
    pub environment_definition_version_id: Uuid,
    pub target_digest: String,
    pub plan_digest: String,
    pub package_digest: String,
    pub package_reference: Option<String>,
    pub compiler_version: String,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    /// `None` when this `Deployment` was loaded with `include_canonical_plan: false` (every list/
    /// summary read path), not when the plan document itself is empty — `rows::deployments` binds
    /// a SQL `NULL::text` in that mode rather than paying for the full JSON fetch.
    pub canonical_plan: Option<String>,
    pub review: DeploymentPlanReview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentEvidence {
    pub kind: String,
    /// `None` when `state` is `MISSING`: the approval snapshot's required-evidence-kind listing
    /// surfaces evidence a project's policy requires but no snapshot has recorded yet, via a
    /// `LEFT JOIN` that leaves every evidence column NULL for that row.
    pub digest: Option<String>,
    pub binding_digest: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentPolicy {
    pub policy_digest: String,
    pub policy_revision: i64,
    pub logical_environment_class: String,
    pub risk: String,
    pub binding_digest: String,
    pub required_evidence: Vec<String>,
    pub required_approvers: i32,
    pub evaluation_requirement_expires_at: Option<DateTime<Utc>>,
    pub evidence: Vec<DeploymentEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentAttempt {
    pub id: Uuid,
    pub number: i64,
    pub status: String,
    pub generation: i64,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub failure_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentRuntimeHealth {
    pub status: String,
    pub summary: String,
    pub observed_at: Option<DateTime<Utc>>,
    pub generation: i64,
}

/// Browser-safe prior active target selected from retained deployment and observed-health facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentRollbackTarget {
    pub deployment_id: Uuid,
    pub agent_version_id: Uuid,
    pub agent_version_number: i64,
    pub target_digest: String,
    pub runtime_health: DeploymentRuntimeHealth,
}

/// Browser-safe projection of one local deployment request and its frozen execution inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployment {
    pub id: Uuid,
    pub project_id: Uuid,
    pub agent_id: Uuid,
    pub agent_display_name: String,
    pub agent_version_id: Uuid,
    pub agent_version_number: i64,
    pub environment: DeploymentEnvironment,
    pub strategy: String,
    pub lifecycle_status: DeploymentLifecycleStatus,
    pub revision: i64,
    pub projection_revision: i64,
    pub requested_by: Uuid,
    pub requested_at: DateTime<Utc>,
    pub plan: DeploymentPlan,
    pub policy: DeploymentPolicy,
    pub current_attempt: Option<DeploymentAttempt>,
    pub runtime_health: DeploymentRuntimeHealth,
    pub rollback_target: Option<DeploymentRollbackTarget>,
}

/// Canonical deployment-history filters. The repository derives tenant visibility before applying them.
#[derive(Debug, Clone, Default)]
pub struct DeploymentFilter {
    pub project_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    pub agent_version_id: Option<Uuid>,
    pub environment_definition_version_id: Option<Uuid>,
    pub lifecycle_status: Option<DeploymentLifecycleStatus>,
    pub strategy: Option<String>,
}

/// Bounded keyset page for tenant-scoped deployment history.
#[derive(Debug, Clone)]
pub struct DeploymentConnection {
    pub nodes: Vec<Deployment>,
    pub cursors: Vec<String>,
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
}

/// Immutable, sanitized unified deployment-history record. An audit event can have no execution attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentTimelineEvent {
    pub id: Uuid,
    pub attempt_id: Option<Uuid>,
    pub attempt_number: i64,
    pub sequence: i64,
    pub stage: String,
    pub status: String,
    pub message: Option<String>,
    pub source: String,
    pub occurred_at: DateTime<Utc>,
}

/// Bounded keyset page for one deployment's unified history.
#[derive(Debug, Clone)]
pub struct DeploymentTimelineConnection {
    pub nodes: Vec<DeploymentTimelineEvent>,
    pub cursors: Vec<String>,
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
}

/// One authorized deployment detail read with its bounded timeline from one repository session.
#[derive(Debug, Clone)]
pub struct DeploymentDetailProjection {
    pub deployment: Deployment,
    pub timeline: DeploymentTimelineConnection,
}

/// Typed, tenant-safe refusal returned by the purpose-built deployment commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentProblemKind {
    NotFound,
    Forbidden,
    InvalidInput,
    ReasonRequired,
    ConfirmationRequired,
    ConfirmationMismatch,
    RevisionConflict,
    LifecycleConflict,
    IdempotencyConflict,
    RateLimited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentProblem {
    pub kind: DeploymentProblemKind,
    pub resource_id: Option<Uuid>,
    pub expected_revision: i64,
    pub actual_revision: i64,
}

impl DeploymentProblem {
    fn simple(kind: DeploymentProblemKind) -> Self {
        Self {
            kind,
            resource_id: None,
            expected_revision: 0,
            actual_revision: 0,
        }
    }

    pub fn not_found() -> Self {
        Self::simple(DeploymentProblemKind::NotFound)
    }

    pub fn forbidden() -> Self {
        Self::simple(DeploymentProblemKind::Forbidden)
    }

    pub fn invalid() -> Self {
        Self::simple(DeploymentProblemKind::InvalidInput)
    }

    pub fn reason_required() -> Self {
        Self::simple(DeploymentProblemKind::ReasonRequired)
    }

    pub fn confirmation_required() -> Self {
        Self::simple(DeploymentProblemKind::ConfirmationRequired)
    }

    pub fn confirmation_mismatch() -> Self {
        Self::simple(DeploymentProblemKind::ConfirmationMismatch)
    }

    pub fn lifecycle() -> Self {
        Self::simple(DeploymentProblemKind::LifecycleConflict)
    }

    pub fn idempotency() -> Self {
        Self::simple(DeploymentProblemKind::IdempotencyConflict)
    }

    pub fn rate_limited() -> Self {
        Self::simple(DeploymentProblemKind::RateLimited)
    }

    pub fn conflict(id: Uuid, expected: i64, actual: i64) -> Self {
        Self {
            kind: DeploymentProblemKind::RevisionConflict,
            resource_id: Some(id),
            expected_revision: expected,
            actual_revision: actual,
        }
    }
}

/// A deployment command returns either the current deployment view or one typed refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentOutcome {
    Accepted,
    Replayed,
    Refused,
}

#[derive(Debug, Clone)]
pub struct DeploymentMutationResult {
    pub deployment: Option<Deployment>,
    pub problem: Option<DeploymentProblem>,
    pub outcome: DeploymentOutcome,
}

impl DeploymentMutationResult {
    pub fn success(deployment: Deployment) -> Self {
        Self {
            deployment: Some(deployment),
            problem: None,
            outcome: DeploymentOutcome::Accepted,
        }
    }

    pub fn replayed(deployment: Deployment) -> Self {
        Self {
            deployment: Some(deployment),
            problem: None,
            outcome: DeploymentOutcome::Replayed,
        }
    }

    pub fn refused(problem: DeploymentProblem) -> Self {
        Self {
            deployment: None,
            problem: Some(problem),
            outcome: DeploymentOutcome::Refused,
        }
    }
}

/// Server-derived preview; submitting repeats the resolution inside its transaction before facts are frozen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewEnvironment {
    pub id: Uuid,
    pub stable_definition_id: String,
    pub version: String,
    pub display_name: String,
    pub logical_environment_class: String,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub content_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewCurrentTarget {
    pub alias_name: String,
    pub deployment_id: Uuid,
    pub agent_version_id: Uuid,
    pub agent_version_number: i64,
    pub target_digest: String,
    pub requested_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentPreview {
    pub environment: PreviewEnvironment,
    pub strategy: String,
    pub risk: String,
    pub policy_digest: String,
    pub policy_revision: i64,
    pub required_evidence: Vec<String>,
    pub required_approvers: i32,
    pub plan_digest: String,
    pub package_digest: String,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub agent_content_digest: String,
    pub target_digest: String,
    pub binding_digest: String,
    pub current_target: Option<PreviewCurrentTarget>,
    pub requirement_expires_at: DateTime<Utc>,
    pub warnings: Vec<String>,
    pub compatibility: String,
}

/// Immutable environment-definition versions selectable for one immutable agent version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentVersion {
    pub id: String,
    pub stable_definition_id: String,
    pub version: String,
    pub display_name: String,
    pub logical_environment_class: String,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub content_digest: String,
}

#[derive(Debug, Clone)]
pub struct DeploymentEnvironmentConnection {
    pub nodes: Vec<EnvironmentVersion>,
    pub cursors: Vec<String>,
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
}

/// Tenant-scoped immutable inputs that the application compiler turns into a deployment request.
#[derive(Debug, Clone)]
pub struct DeploymentCompilationContext {
    pub version: super::compiler::VersionSource,
    pub environment: super::compiler::EnvironmentDefinition,
    pub policy: super::compiler::PolicySource,
    pub current_target: Option<super::compiler::ActiveTarget>,
}

/// Tenant-scoped current inputs that the application compiler uses for one recovery cycle.
#[derive(Debug, Clone)]
pub struct DeploymentRecoveryCompilationContext {
    pub version: super::compiler::VersionSource,
    pub environment: super::compiler::EnvironmentDefinition,
    pub policy: super::compiler::PolicySource,
    pub current_target: Option<super::compiler::ActiveTarget>,
    pub strategy: String,
}

/// Immutable principal identity projected from the authoritative principal directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPrincipal {
    pub id: Uuid,
    pub subject: String,
}

/// One append-only human decision recorded against a frozen deployment approval requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalDecision {
    pub id: Uuid,
    pub requirement_id: Uuid,
    pub actor_principal_id: Uuid,
    pub value: String,
    pub comment: Option<String>,
    pub rejection_reason: Option<String>,
    pub eligibility_checked_at: DateTime<Utc>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRule {
    pub required_evidence: Vec<String>,
    pub required_distinct_approver_count: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalTarget {
    pub agent_version_id: Uuid,
    pub agent_version_digest: String,
    pub environment_definition_version_id: Uuid,
    pub environment_definition_digest: String,
    pub target_digest: String,
    pub deployment_plan_digest: String,
    pub artifact_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalSnapshot {
    pub policy_digest: String,
    pub policy_revision: i64,
    pub environment_class: String,
    pub risk: String,
    pub rule: ApprovalRule,
    pub target: ApprovalTarget,
    pub evidence: Vec<DeploymentEvidence>,
    pub expires_at: DateTime<Utc>,
}

/// A bounded first decision page materialized with its authorized approval inbox page.
#[derive(Debug, Clone)]
pub struct ApprovalDecisionPreview {
    pub nodes: Vec<ApprovalDecision>,
    pub cursors: Vec<String>,
}

/// Frozen approval inputs plus the current terminal-or-pending evaluation state for one deployment cycle.
#[derive(Debug, Clone)]
pub struct ApprovalRequirement {
    pub id: Uuid,
    pub deployment_id: Uuid,
    pub project_id: Uuid,
    pub revision: i64,
    pub status: ApprovalRequirementStatus,
    pub expires_at: Option<DateTime<Utc>>,
    pub satisfied_at: Option<DateTime<Utc>>,
    pub rejected_at: Option<DateTime<Utc>>,
    pub invalidated_at: Option<DateTime<Utc>>,
    pub requester_id: Uuid,
    pub requester: Option<ApprovalPrincipal>,
    pub required_distinct_approver_count: i32,
    pub qualifying_approval_count: i32,
    pub satisfied_participants: Vec<Uuid>,
    pub satisfied_participant_details: Vec<ApprovalPrincipal>,
    pub approval_snapshot: ApprovalSnapshot,
    pub decision_preview: Option<ApprovalDecisionPreview>,
}

/// One tenant-scoped inbox row. The boolean is a rendering hint; the command reauthorizes independently.
#[derive(Debug, Clone)]
pub struct ApprovalInboxItem {
    pub requirement: ApprovalRequirement,
    pub deployment: Deployment,
    pub eligible: bool,
    pub decision_available: bool,
}

#[derive(Debug, Clone)]
pub struct ApprovalDecisionConnection {
    pub nodes: Vec<ApprovalDecision>,
    pub cursors: Vec<String>,
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
}

#[derive(Debug, Clone)]
pub struct ApprovalInboxConnection {
    pub nodes: Vec<ApprovalInboxItem>,
    pub cursors: Vec<String>,
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
}

/// Application-facing refusal projection that keeps transport callers independent from domain planning types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalDecisionProblem {
    pub code: String,
    pub resource_id: Option<Uuid>,
    pub expected_revision: i64,
    pub actual_revision: i64,
}

impl ApprovalDecisionProblem {
    pub fn unavailable() -> Self {
        Self {
            code: "NOT_FOUND".to_string(),
            resource_id: None,
            expected_revision: 0,
            actual_revision: 0,
        }
    }

    pub fn of(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            resource_id: None,
            expected_revision: 0,
            actual_revision: 0,
        }
    }
}

impl From<hive_domain::deployment::ApprovalDecisionProblem> for ApprovalDecisionProblem {
    fn from(value: hive_domain::deployment::ApprovalDecisionProblem) -> Self {
        Self {
            code: value.code,
            resource_id: value.resource_id,
            expected_revision: value.expected_revision,
            actual_revision: value.actual_revision,
        }
    }
}

/// Result of appending one immutable approval or rejection fact.
#[derive(Debug, Clone)]
pub struct ApprovalDecisionMutationResult {
    pub decision: Option<ApprovalDecision>,
    pub requirement: Option<ApprovalRequirement>,
    pub deployment: Option<Deployment>,
    pub problem: Option<ApprovalDecisionProblem>,
}

impl ApprovalDecisionMutationResult {
    pub fn success(
        decision: ApprovalDecision,
        requirement: ApprovalRequirement,
        deployment: Deployment,
    ) -> Self {
        Self {
            decision: Some(decision),
            requirement: Some(requirement),
            deployment: Some(deployment),
            problem: None,
        }
    }

    pub fn refused(problem: ApprovalDecisionProblem) -> Self {
        Self {
            decision: None,
            requirement: None,
            deployment: None,
            problem: Some(problem),
        }
    }
}
