//! Closed-set text columns as SeaORM active enums, so the ORM types them. One enum per concept; an
//! enum is shared between columns only where the concept and the value set are both the same.
//! Values match each column's `CHECK (... IN (...))`, which `tests/entity_coverage.rs` verifies
//! against the live schema. These columns are `TEXT`, not native database enums (Aurora DSQL has
//! none), so Seaography exposes them as `String` with string filters
//! (`docs/idiomatic-seaography-plan.md`, A1).

use sea_orm::entity::prelude::*;

/// Lifecycle of an organization, project, evaluation definition or reusable resource.
///
/// Columns: `organizations.lifecycle_status`, `projects.lifecycle_status`, `evaluation_definitions.lifecycle_status`, `reusable_resources.lifecycle_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "lifecycle_status")]
pub enum LifecycleStatus {
    #[sea_orm(string_value = "ACTIVE")]
    Active,
    #[sea_orm(string_value = "ARCHIVED")]
    Archived,
}

/// Columns: `agents.lifecycle_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "agent_lifecycle_status"
)]
pub enum AgentLifecycleStatus {
    #[sea_orm(string_value = "ACTIVE")]
    Active,
    #[sea_orm(string_value = "DEPRECATED")]
    Deprecated,
    #[sea_orm(string_value = "ARCHIVED")]
    Archived,
}

/// Lifecycle of a project settings connection or tool connection.
///
/// Columns: `project_settings_connections.lifecycle_status`, `project_tool_connections.lifecycle_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "connection_lifecycle_status"
)]
pub enum ConnectionLifecycleStatus {
    #[sea_orm(string_value = "ACTIVE")]
    Active,
    #[sea_orm(string_value = "DISABLED")]
    Disabled,
    #[sea_orm(string_value = "ARCHIVED")]
    Archived,
}

/// The logical environment class; `environment` columns hold the same value (a deployment plan's
/// `environment` is written from its environment definition's `logical_environment_class`).
///
/// Columns: `catalog_environments.environment`, `deployment_plan_versions.environment`, `deployments.environment`, `project_settings_connections.environment`, `project_tool_connections.environment`, `deployment_policy_snapshots.logical_environment_class`, `environment_definition_versions.logical_environment_class`, `evaluation_target_projections.logical_environment_class`, `evaluation_target_snapshots.logical_environment_class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "logical_environment_class"
)]
pub enum LogicalEnvironmentClass {
    #[sea_orm(string_value = "DEVELOPMENT")]
    Development,
    #[sea_orm(string_value = "STAGING")]
    Staging,
    #[sea_orm(string_value = "PRODUCTION")]
    Production,
}

/// Columns: `administration_audit_events.scope_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "administration_scope_type"
)]
pub enum AdministrationScopeType {
    #[sea_orm(string_value = "ORGANIZATION")]
    Organization,
    #[sea_orm(string_value = "PROJECT")]
    Project,
}

/// Columns: `agent_authoring_audit_events.action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "agent_authoring_audit_action"
)]
pub enum AgentAuthoringAuditAction {
    #[sea_orm(string_value = "CREATED")]
    Created,
    #[sea_orm(string_value = "SAVED")]
    Saved,
    #[sea_orm(string_value = "VALIDATED")]
    Validated,
    #[sea_orm(string_value = "PUBLISHED")]
    Published,
}

/// Columns: `agent_draft_audit_events.action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "agent_draft_audit_action"
)]
pub enum AgentDraftAuditAction {
    #[sea_orm(string_value = "UPDATED")]
    Updated,
    #[sea_orm(string_value = "VALIDATED")]
    Validated,
}

/// Columns: `agent_draft_editor_roles.role_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "agent_draft_editor_role_code"
)]
pub enum AgentDraftEditorRoleCode {
    #[sea_orm(string_value = "PROJECT_ADMIN")]
    ProjectAdmin,
    #[sea_orm(string_value = "AGENT_DEVELOPER")]
    AgentDeveloper,
}

/// Validation state of an agent draft or an evaluation definition draft.
///
/// Columns: `agent_drafts.validation_status`, `agent_operational_summaries.draft_validation_status`, `evaluation_definition_drafts.validation_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "draft_validation_status"
)]
pub enum DraftValidationStatus {
    #[sea_orm(string_value = "VALID")]
    Valid,
    #[sea_orm(string_value = "INVALID")]
    Invalid,
    #[sea_orm(string_value = "NOT_VALIDATED")]
    NotValidated,
}

/// Columns: `agent_operational_summaries.deployment_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "agent_deployment_status"
)]
pub enum AgentDeploymentStatus {
    #[sea_orm(string_value = "ACTIVE")]
    Active,
    #[sea_orm(string_value = "DEGRADED")]
    Degraded,
    #[sea_orm(string_value = "FAILED")]
    Failed,
    #[sea_orm(string_value = "NOT_DEPLOYED")]
    NotDeployed,
}

/// Columns: `agent_operational_summaries.evaluation_outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "agent_evaluation_outcome"
)]
pub enum AgentEvaluationOutcome {
    #[sea_orm(string_value = "PASSED")]
    Passed,
    #[sea_orm(string_value = "FAILED")]
    Failed,
    #[sea_orm(string_value = "INCONCLUSIVE")]
    Inconclusive,
    #[sea_orm(string_value = "NO_EVALUATION")]
    NoEvaluation,
}

/// Columns: `agent_operational_summaries.runtime_health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "agent_runtime_health"
)]
pub enum AgentRuntimeHealth {
    #[sea_orm(string_value = "HEALTHY")]
    Healthy,
    #[sea_orm(string_value = "DEGRADED")]
    Degraded,
    #[sea_orm(string_value = "UNHEALTHY")]
    Unhealthy,
    #[sea_orm(string_value = "UNKNOWN")]
    Unknown,
}

/// Columns: `catalog_definitions.definition_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "catalog_definition_kind"
)]
pub enum CatalogDefinitionKind {
    #[sea_orm(string_value = "model")]
    Model,
    #[sea_orm(string_value = "tool")]
    Tool,
}

/// Columns: `console_role_assignments.role_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "console_role_code")]
pub enum ConsoleRoleCode {
    #[sea_orm(string_value = "ORGANIZATION_MEMBER")]
    OrganizationMember,
    #[sea_orm(string_value = "PROJECT_ADMIN")]
    ProjectAdmin,
    #[sea_orm(string_value = "AGENT_DEVELOPER")]
    AgentDeveloper,
}

/// Columns: `organization_membership_roles.role_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "organization_role_code"
)]
pub enum OrganizationRoleCode {
    #[sea_orm(string_value = "ORGANIZATION_MEMBER")]
    OrganizationMember,
    #[sea_orm(string_value = "ORGANIZATION_ADMIN")]
    OrganizationAdmin,
    #[sea_orm(string_value = "AUDITOR")]
    Auditor,
}

/// Columns: `project_membership_roles.role_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "project_role_code")]
pub enum ProjectRoleCode {
    #[sea_orm(string_value = "PROJECT_ADMIN")]
    ProjectAdmin,
    #[sea_orm(string_value = "AGENT_DEVELOPER")]
    AgentDeveloper,
    #[sea_orm(string_value = "OPERATOR")]
    Operator,
    #[sea_orm(string_value = "DEPLOYMENT_APPROVER")]
    DeploymentApprover,
    #[sea_orm(string_value = "AUDITOR")]
    Auditor,
}

/// The column's CHECK is `role_code = 'PLATFORM_ADMIN'`: a closed set of one.
///
/// Columns: `platform_role_assignments.role_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "platform_role_code")]
pub enum PlatformRoleCode {
    #[sea_orm(string_value = "PLATFORM_ADMIN")]
    PlatformAdmin,
}

/// Columns: `deployment_approval_decisions.decision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "approval_decision")]
pub enum ApprovalDecision {
    #[sea_orm(string_value = "APPROVE")]
    Approve,
    #[sea_orm(string_value = "REJECT")]
    Reject,
}

/// Columns: `deployment_approval_requirements.invalidation_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "approval_invalidation_code"
)]
pub enum ApprovalInvalidationCode {
    #[sea_orm(string_value = "APPROVAL_EVIDENCE_MISSING")]
    ApprovalEvidenceMissing,
    #[sea_orm(string_value = "APPROVAL_EVIDENCE_EXPIRED")]
    ApprovalEvidenceExpired,
    #[sea_orm(string_value = "APPROVAL_EVIDENCE_MISMATCH")]
    ApprovalEvidenceMismatch,
    #[sea_orm(string_value = "APPROVAL_REQUIREMENT_EXPIRED")]
    ApprovalRequirementExpired,
    #[sea_orm(string_value = "TERMINAL_LIFECYCLE")]
    TerminalLifecycle,
    #[sea_orm(string_value = "PROJECT_ARCHIVED")]
    ProjectArchived,
}

/// Columns: `deployment_approval_requirements.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "approval_requirement_status"
)]
pub enum ApprovalRequirementStatus {
    #[sea_orm(string_value = "PENDING")]
    Pending,
    #[sea_orm(string_value = "SATISFIED")]
    Satisfied,
    #[sea_orm(string_value = "REJECTED")]
    Rejected,
    #[sea_orm(string_value = "EXPIRED")]
    Expired,
    #[sea_orm(string_value = "INVALIDATED")]
    Invalidated,
}

/// Columns: `deployment_attempts.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "deployment_attempt_status"
)]
pub enum DeploymentAttemptStatus {
    #[sea_orm(string_value = "QUEUED")]
    Queued,
    #[sea_orm(string_value = "RUNNING")]
    Running,
    #[sea_orm(string_value = "SUCCEEDED")]
    Succeeded,
    #[sea_orm(string_value = "FAILED")]
    Failed,
    #[sea_orm(string_value = "CANCELED")]
    Canceled,
}

/// Columns: `deployment_audit_events.action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "deployment_audit_action"
)]
pub enum DeploymentAuditAction {
    #[sea_orm(string_value = "REQUESTED")]
    Requested,
    #[sea_orm(string_value = "CANCELED")]
    Canceled,
    #[sea_orm(string_value = "EXECUTION_STARTED")]
    ExecutionStarted,
    #[sea_orm(string_value = "EXECUTION_SUCCEEDED")]
    ExecutionSucceeded,
    #[sea_orm(string_value = "EXECUTION_FAILED")]
    ExecutionFailed,
    #[sea_orm(string_value = "OUTBOX_DEAD_LETTERED")]
    OutboxDeadLettered,
    #[sea_orm(string_value = "OUTBOX_LEASE_RECLAIMED")]
    OutboxLeaseReclaimed,
    #[sea_orm(string_value = "OUTBOX_DELIVERY_RETRIED")]
    OutboxDeliveryRetried,
    #[sea_orm(string_value = "APPROVAL_RECORDED")]
    ApprovalRecorded,
    #[sea_orm(string_value = "APPROVAL_REPLAYED")]
    ApprovalReplayed,
    #[sea_orm(string_value = "APPROVAL_SATISFIED")]
    ApprovalSatisfied,
    #[sea_orm(string_value = "APPROVAL_REJECTED")]
    ApprovalRejected,
    #[sea_orm(string_value = "APPROVAL_EXPIRED")]
    ApprovalExpired,
    #[sea_orm(string_value = "APPROVAL_INVALIDATED")]
    ApprovalInvalidated,
    #[sea_orm(string_value = "APPROVAL_EXECUTION_BLOCKED")]
    ApprovalExecutionBlocked,
    #[sea_orm(string_value = "RETRY_RECORDED")]
    RetryRecorded,
    #[sea_orm(string_value = "PROMOTION_RECORDED")]
    PromotionRecorded,
    #[sea_orm(string_value = "ROLLBACK_RECORDED")]
    RollbackRecorded,
}

/// Columns: `deployment_evidence_invalidations.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "evidence_invalidation_kind"
)]
pub enum EvidenceInvalidationKind {
    #[sea_orm(string_value = "REVOKED")]
    Revoked,
    #[sea_orm(string_value = "FAILED")]
    Failed,
}

/// Columns: `deployment_evidence_snapshots.evidence_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "deployment_evidence_kind"
)]
pub enum DeploymentEvidenceKind {
    #[sea_orm(string_value = "PLAN_VALIDATED")]
    PlanValidated,
    #[sea_orm(string_value = "CHANGE_SUMMARY_READY")]
    ChangeSummaryReady,
    #[sea_orm(string_value = "EVALUATION_PASSED")]
    EvaluationPassed,
}

/// Columns: `deployment_outbox_delivery_audit_repairs.action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "outbox_delivery_audit_action"
)]
pub enum OutboxDeliveryAuditAction {
    #[sea_orm(string_value = "OUTBOX_DELIVERY_RETRIED")]
    OutboxDeliveryRetried,
    #[sea_orm(string_value = "OUTBOX_DEAD_LETTERED")]
    OutboxDeadLettered,
}

/// Columns: `deployment_outbox_events.event_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "deployment_outbox_event_type"
)]
pub enum DeploymentOutboxEventType {
    #[sea_orm(string_value = "EXECUTE_DEPLOYMENT")]
    ExecuteDeployment,
    #[sea_orm(string_value = "COMPLETE_DEPLOYMENT")]
    CompleteDeployment,
}

/// Columns: `deployment_outbox_events.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "deployment_outbox_status"
)]
pub enum DeploymentOutboxStatus {
    #[sea_orm(string_value = "PENDING")]
    Pending,
    #[sea_orm(string_value = "PROCESSING")]
    Processing,
    #[sea_orm(string_value = "DELIVERED")]
    Delivered,
    #[sea_orm(string_value = "DEAD_LETTER")]
    DeadLetter,
}

/// Columns: `deployment_policy_snapshots.risk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "deployment_risk")]
pub enum DeploymentRisk {
    #[sea_orm(string_value = "LOW")]
    Low,
    #[sea_orm(string_value = "MEDIUM")]
    Medium,
    #[sea_orm(string_value = "HIGH")]
    High,
}

/// Columns: `deployment_recovery_action_receipts.action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "deployment_recovery_action"
)]
pub enum DeploymentRecoveryAction {
    #[sea_orm(string_value = "RETRY")]
    Retry,
    #[sea_orm(string_value = "PROMOTE")]
    Promote,
    #[sea_orm(string_value = "ROLLBACK")]
    Rollback,
}

/// Columns: `deployment_runtime_health.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "deployment_runtime_health_status"
)]
pub enum DeploymentRuntimeHealthStatus {
    #[sea_orm(string_value = "NOT_OBSERVED")]
    NotObserved,
    #[sea_orm(string_value = "STARTING")]
    Starting,
    #[sea_orm(string_value = "HEALTHY")]
    Healthy,
    #[sea_orm(string_value = "UNHEALTHY")]
    Unhealthy,
    #[sea_orm(string_value = "CANCELED")]
    Canceled,
}

/// Columns: `deployment_stage_events.stage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "deployment_stage")]
pub enum DeploymentStage {
    #[sea_orm(string_value = "REQUESTED")]
    Requested,
    #[sea_orm(string_value = "PACKAGING")]
    Packaging,
    #[sea_orm(string_value = "EXECUTING")]
    Executing,
    #[sea_orm(string_value = "COMPLETED")]
    Completed,
    #[sea_orm(string_value = "CANCELED")]
    Canceled,
    #[sea_orm(string_value = "FAILED")]
    Failed,
}

/// Columns: `deployment_stage_events.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "deployment_stage_status"
)]
pub enum DeploymentStageStatus {
    #[sea_orm(string_value = "STARTED")]
    Started,
    #[sea_orm(string_value = "SUCCEEDED")]
    Succeeded,
    #[sea_orm(string_value = "FAILED")]
    Failed,
    #[sea_orm(string_value = "CANCELED")]
    Canceled,
}

/// Columns: `deployments.lifecycle_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "deployment_lifecycle_status"
)]
pub enum DeploymentLifecycleStatus {
    #[sea_orm(string_value = "REQUESTED")]
    Requested,
    #[sea_orm(string_value = "AWAITING_APPROVAL")]
    AwaitingApproval,
    #[sea_orm(string_value = "APPROVED")]
    Approved,
    #[sea_orm(string_value = "IN_PROGRESS")]
    InProgress,
    #[sea_orm(string_value = "ACTIVE")]
    Active,
    #[sea_orm(string_value = "FAILED")]
    Failed,
    #[sea_orm(string_value = "CANCELED")]
    Canceled,
    #[sea_orm(string_value = "ROLLED_BACK")]
    RolledBack,
}

/// Columns: `deployments.strategy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "deployment_strategy"
)]
pub enum DeploymentStrategy {
    #[sea_orm(string_value = "REPLACE")]
    Replace,
    #[sea_orm(string_value = "ROLLING")]
    Rolling,
    #[sea_orm(string_value = "CANARY")]
    Canary,
    #[sea_orm(string_value = "BLUE_GREEN")]
    BlueGreen,
}

/// Self-reported state of a deployment or evaluation worker.
///
/// Columns: `deployment_worker_heartbeats.state`, `evaluation_worker_heartbeats.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "worker_heartbeat_state"
)]
pub enum WorkerHeartbeatState {
    #[sea_orm(string_value = "READY")]
    Ready,
    #[sea_orm(string_value = "DEGRADED")]
    Degraded,
}

/// Lifecycle of an evaluation run and of each case run inside it.
///
/// Columns: `evaluation_runs.lifecycle_status`, `evaluation_case_runs.lifecycle_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "evaluation_lifecycle_status"
)]
pub enum EvaluationLifecycleStatus {
    #[sea_orm(string_value = "QUEUED")]
    Queued,
    #[sea_orm(string_value = "RUNNING")]
    Running,
    #[sea_orm(string_value = "COMPLETED")]
    Completed,
    #[sea_orm(string_value = "FAILED")]
    Failed,
    #[sea_orm(string_value = "CANCELED")]
    Canceled,
}

/// Columns: `evaluation_command_receipts.action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "evaluation_command_action"
)]
pub enum EvaluationCommandAction {
    #[sea_orm(string_value = "CREATE")]
    Create,
    #[sea_orm(string_value = "UPDATE")]
    Update,
    #[sea_orm(string_value = "VALIDATE")]
    Validate,
    #[sea_orm(string_value = "DUPLICATE")]
    Duplicate,
    #[sea_orm(string_value = "PUBLISH")]
    Publish,
    #[sea_orm(string_value = "RUN")]
    Run,
    #[sea_orm(string_value = "CANCEL")]
    Cancel,
    #[sea_orm(string_value = "RERUN")]
    Rerun,
}

/// Columns: `evaluation_outbox_events.event_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "evaluation_outbox_event_type"
)]
pub enum EvaluationOutboxEventType {
    #[sea_orm(string_value = "START")]
    Start,
    #[sea_orm(string_value = "CASE")]
    Case,
    #[sea_orm(string_value = "FINALIZE")]
    Finalize,
}

/// Columns: `evaluation_outbox_events.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "evaluation_outbox_status"
)]
pub enum EvaluationOutboxStatus {
    #[sea_orm(string_value = "PENDING")]
    Pending,
    #[sea_orm(string_value = "PROCESSING")]
    Processing,
    #[sea_orm(string_value = "DELIVERED")]
    Delivered,
    #[sea_orm(string_value = "DEAD_LETTER")]
    DeadLetter,
    #[sea_orm(string_value = "CANCELED")]
    Canceled,
}

/// Outcome of an evaluation run, also recorded on its result row.
///
/// Columns: `evaluation_runs.outcome_category`, `evaluation_results.outcome_category`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "evaluation_outcome_category"
)]
pub enum EvaluationOutcomeCategory {
    #[sea_orm(string_value = "PASSED")]
    Passed,
    #[sea_orm(string_value = "CASE_FAILED")]
    CaseFailed,
    #[sea_orm(string_value = "TARGET_FAILED")]
    TargetFailed,
    #[sea_orm(string_value = "RUNNER_FAILED")]
    RunnerFailed,
    #[sea_orm(string_value = "CANCELED")]
    Canceled,
}

/// Columns: `evaluation_runs.target_kind`, `evaluation_target_projections.target_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "evaluation_target_kind"
)]
pub enum EvaluationTargetKind {
    #[sea_orm(string_value = "AGENT_VERSION")]
    AgentVersion,
    #[sea_orm(string_value = "DEPLOYMENT")]
    Deployment,
}

/// Columns: `frozen_spend_import_batches.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "spend_import_batch_state"
)]
pub enum SpendImportBatchState {
    #[sea_orm(string_value = "COMPLETE")]
    Complete,
    #[sea_orm(string_value = "FAILED")]
    Failed,
    #[sea_orm(string_value = "INCOMPLETE")]
    Incomplete,
}

/// Columns: `principal_display_preferences.color_scheme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "color_scheme")]
pub enum ColorScheme {
    #[sea_orm(string_value = "SYSTEM")]
    System,
    #[sea_orm(string_value = "LIGHT")]
    Light,
    #[sea_orm(string_value = "DARK")]
    Dark,
}

/// Columns: `principal_display_preferences.density`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "display_density")]
pub enum DisplayDensity {
    #[sea_orm(string_value = "COMFORTABLE")]
    Comfortable,
    #[sea_orm(string_value = "COMPACT")]
    Compact,
}

/// Columns: `principal_display_preferences.sidebar_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "sidebar_state")]
pub enum SidebarState {
    #[sea_orm(string_value = "EXPANDED")]
    Expanded,
    #[sea_orm(string_value = "COLLAPSED")]
    Collapsed,
}

/// Columns: `project_dashboard_metrics.cost_availability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "cost_availability")]
pub enum CostAvailability {
    #[sea_orm(string_value = "AVAILABLE")]
    Available,
    #[sea_orm(string_value = "UNAVAILABLE")]
    Unavailable,
    #[sea_orm(string_value = "UNKNOWN")]
    Unknown,
}

/// Columns: `project_settings_connections.credential_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "credential_status")]
pub enum CredentialStatus {
    #[sea_orm(string_value = "UNBOUND")]
    Unbound,
    #[sea_orm(string_value = "REDACTED_BOUND")]
    RedactedBound,
}

/// Columns: `reusable_resource_drafts.validation_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "reusable_resource_validation_status"
)]
pub enum ReusableResourceValidationStatus {
    #[sea_orm(string_value = "UNVALIDATED")]
    Unvalidated,
    #[sea_orm(string_value = "VALID")]
    Valid,
    #[sea_orm(string_value = "INVALID")]
    Invalid,
}

/// Columns: `reusable_resources.resource_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "reusable_resource_kind"
)]
pub enum ReusableResourceKind {
    #[sea_orm(string_value = "PROMPT")]
    Prompt,
    #[sea_orm(string_value = "POLICY")]
    Policy,
    #[sea_orm(string_value = "MODEL_PROFILE")]
    ModelProfile,
}
