//! Ports `DeploymentGraphql`'s full field set: the 5 deployment queries
//! (`deployments`, `deploymentTimeline`, `deploymentProjection`,
//! `deploymentEnvironmentDefinitionVersions`, `deploymentPreview`), the 5
//! deployment mutations (`deployAgentVersion`, `cancelDeployment`,
//! `retryDeployment`, `promoteDeployment`, `rollbackDeployment`), and
//! (RTP-APPROVAL) the approval inbox/decision surface: `approvalInbox`,
//! `approvalRequirement` (Java's resolver method is named `approvalDetail`,
//! but the GraphQL field itself is `approvalRequirement`), the nested
//! `ApprovalRequirement.decisions` field, and `decideDeploymentApproval`.
//! `DeploymentService::decide_approval` already carries
//! `DecideDeploymentApprovalService`'s validation (the `REVIEW_CODES`
//! allowlist and decision/comment/rejectionReason cross-field check), so this
//! file only wires the GraphQL layer on top of already-ported repository and
//! service methods.
//!
//! Every "long"-typed Java field (`policyRevision`, `revision`, `generation`,
//! `sequence`, ...) is ported as `i32`, matching the cast-from-i64 convention
//! this codebase already uses for every other revision/version-number field
//! (e.g. `AgentVersion.number`), rather than introducing a custom GraphQL
//! `Long` scalar only for this domain.

use crate::schema::{Principal, RequestCorrelationId, RequestPrincipal};
use async_graphql::{Context, Enum, InputObject, Interface, Object, SimpleObject};
use hive_application::deployment::{
    ApprovalDecision as AppApprovalDecision, ApprovalDecisionConnection as AppDecisionConnection,
    ApprovalDecisionMutationResult as AppDecisionMutationResult,
    ApprovalDecisionProblem as AppDecisionProblem, ApprovalInboxConnection as AppInboxConnection,
    ApprovalInboxItem as AppInboxItem, ApprovalPrincipal as AppApprovalPrincipal,
    ApprovalRequirement as AppApprovalRequirement, ApprovalRule as AppApprovalRule,
    ApprovalSnapshot as AppApprovalSnapshot, ApprovalTarget as AppApprovalTarget,
    Deployment as AppDeployment, DeploymentAttempt as AppDeploymentAttempt,
    DeploymentConnection as AppDeploymentConnection,
    DeploymentDetailProjection as AppDeploymentDetailProjection,
    DeploymentEnvironment as AppDeploymentEnvironment,
    DeploymentEnvironmentConnection as AppDeploymentEnvironmentConnection,
    DeploymentEvidence as AppDeploymentEvidence, DeploymentFilter as AppDeploymentFilter,
    DeploymentMutationResult as AppMutationResult, DeploymentPlan as AppDeploymentPlan,
    DeploymentPlanReview as AppDeploymentPlanReview, DeploymentPolicy as AppDeploymentPolicy,
    DeploymentPreview as AppDeploymentPreview, DeploymentProblem as AppProblem,
    DeploymentProblemKind as AppProblemKind,
    DeploymentRollbackTarget as AppDeploymentRollbackTarget,
    DeploymentRuntimeHealth as AppDeploymentRuntimeHealth, DeploymentService,
    DeploymentTimelineConnection as AppDeploymentTimelineConnection,
    DeploymentTimelineEvent as AppDeploymentTimelineEvent,
    EnvironmentVersion as AppEnvironmentVersion, PreviewCurrentTarget as AppPreviewCurrentTarget,
};
use hive_persistence::deployment::PgDeploymentRepository;
use uuid::Uuid;

fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    hive_domain::java_offset_date_time_string(value)
}

fn optional_timestamp(value: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    value.map(hive_domain::java_offset_date_time_string)
}

// --- enums (Java declares these via GraphQLEnumType, not plain strings) ---

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum DeploymentStrategy {
    Replace,
    Rolling,
    BlueGreen,
    Canary,
}

impl DeploymentStrategy {
    fn parse(value: &str) -> Self {
        match value {
            "REPLACE" => Self::Replace,
            "ROLLING" => Self::Rolling,
            "BLUE_GREEN" => Self::BlueGreen,
            "CANARY" => Self::Canary,
            other => panic!("unrecognized deployment strategy `{other}`"),
        }
    }

    fn value(self) -> &'static str {
        match self {
            Self::Replace => "REPLACE",
            Self::Rolling => "ROLLING",
            Self::BlueGreen => "BLUE_GREEN",
            Self::Canary => "CANARY",
        }
    }
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum DeploymentLifecycleStatus {
    Requested,
    AwaitingApproval,
    Approved,
    InProgress,
    Active,
    Failed,
    Canceled,
    RolledBack,
}

impl From<hive_domain::deployment::DeploymentLifecycleStatus> for DeploymentLifecycleStatus {
    fn from(value: hive_domain::deployment::DeploymentLifecycleStatus) -> Self {
        use hive_domain::deployment::DeploymentLifecycleStatus as Domain;
        match value {
            Domain::Requested => Self::Requested,
            Domain::AwaitingApproval => Self::AwaitingApproval,
            Domain::Approved => Self::Approved,
            Domain::InProgress => Self::InProgress,
            Domain::Active => Self::Active,
            Domain::Failed => Self::Failed,
            Domain::Canceled => Self::Canceled,
            Domain::RolledBack => Self::RolledBack,
        }
    }
}

impl From<DeploymentLifecycleStatus> for hive_domain::deployment::DeploymentLifecycleStatus {
    fn from(value: DeploymentLifecycleStatus) -> Self {
        match value {
            DeploymentLifecycleStatus::Requested => Self::Requested,
            DeploymentLifecycleStatus::AwaitingApproval => Self::AwaitingApproval,
            DeploymentLifecycleStatus::Approved => Self::Approved,
            DeploymentLifecycleStatus::InProgress => Self::InProgress,
            DeploymentLifecycleStatus::Active => Self::Active,
            DeploymentLifecycleStatus::Failed => Self::Failed,
            DeploymentLifecycleStatus::Canceled => Self::Canceled,
            DeploymentLifecycleStatus::RolledBack => Self::RolledBack,
        }
    }
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum DeploymentAttemptStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
}

impl DeploymentAttemptStatus {
    fn parse(value: &str) -> Self {
        match value {
            "QUEUED" => Self::Queued,
            "RUNNING" => Self::Running,
            "SUCCEEDED" => Self::Succeeded,
            "FAILED" => Self::Failed,
            "CANCELED" => Self::Canceled,
            other => panic!("unrecognized deployment attempt status `{other}`"),
        }
    }
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum DeploymentRuntimeHealthStatus {
    NotObserved,
    Starting,
    Healthy,
    Unhealthy,
    Canceled,
}

impl DeploymentRuntimeHealthStatus {
    fn parse(value: &str) -> Self {
        match value {
            "NOT_OBSERVED" => Self::NotObserved,
            "STARTING" => Self::Starting,
            "HEALTHY" => Self::Healthy,
            "UNHEALTHY" => Self::Unhealthy,
            "CANCELED" => Self::Canceled,
            other => panic!("unrecognized deployment runtime health status `{other}`"),
        }
    }
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum LogicalEnvironmentClass {
    Development,
    Staging,
    Production,
}

impl LogicalEnvironmentClass {
    fn parse(value: &str) -> Self {
        match value {
            "DEVELOPMENT" => Self::Development,
            "STAGING" => Self::Staging,
            "PRODUCTION" => Self::Production,
            other => panic!("unrecognized logical environment class `{other}`"),
        }
    }
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum DeploymentRiskLevel {
    Low,
    Medium,
    High,
}

impl DeploymentRiskLevel {
    fn parse(value: &str) -> Self {
        match value {
            "LOW" => Self::Low,
            "MEDIUM" => Self::Medium,
            "HIGH" => Self::High,
            other => panic!("unrecognized deployment risk level `{other}`"),
        }
    }
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum ApprovalEvidenceKind {
    PlanValidated,
    ChangeSummaryReady,
    EvaluationPassed,
}

impl ApprovalEvidenceKind {
    fn parse(value: &str) -> Self {
        match value {
            "PLAN_VALIDATED" => Self::PlanValidated,
            "CHANGE_SUMMARY_READY" => Self::ChangeSummaryReady,
            "EVALUATION_PASSED" => Self::EvaluationPassed,
            other => panic!("unrecognized approval evidence kind `{other}`"),
        }
    }
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum ApprovalEvidenceState {
    Valid,
    Missing,
    Expired,
    Revoked,
    Failed,
    Mismatch,
}

impl ApprovalEvidenceState {
    fn parse(value: &str) -> Self {
        match value {
            "VALID" => Self::Valid,
            "MISSING" => Self::Missing,
            "EXPIRED" => Self::Expired,
            "REVOKED" => Self::Revoked,
            "FAILED" => Self::Failed,
            "MISMATCH" => Self::Mismatch,
            other => panic!("unrecognized approval evidence state `{other}`"),
        }
    }
}

// --- object types ---

#[derive(SimpleObject, Clone)]
pub struct DeploymentEnvironmentDefinitionVersion {
    pub id: async_graphql::ID,
    pub stable_definition_id: String,
    pub version: String,
    pub display_name: String,
    pub logical_environment_class: LogicalEnvironmentClass,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub content_digest: String,
}

impl From<&AppDeploymentEnvironment> for DeploymentEnvironmentDefinitionVersion {
    fn from(value: &AppDeploymentEnvironment) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            stable_definition_id: value.stable_definition_id.clone(),
            version: value.version.clone(),
            display_name: value.display_name.clone(),
            logical_environment_class: LogicalEnvironmentClass::parse(
                &value.logical_environment_class,
            ),
            catalog_release_id: value.catalog_release_id.clone(),
            catalog_release_digest: value.catalog_release_digest.clone(),
            content_digest: value.content_digest.clone(),
        }
    }
}

impl From<&AppEnvironmentVersion> for DeploymentEnvironmentDefinitionVersion {
    fn from(value: &AppEnvironmentVersion) -> Self {
        Self {
            id: async_graphql::ID(value.id.clone()),
            stable_definition_id: value.stable_definition_id.clone(),
            version: value.version.clone(),
            display_name: value.display_name.clone(),
            logical_environment_class: LogicalEnvironmentClass::parse(
                &value.logical_environment_class,
            ),
            catalog_release_id: value.catalog_release_id.clone(),
            catalog_release_digest: value.catalog_release_digest.clone(),
            content_digest: value.content_digest.clone(),
        }
    }
}

#[derive(SimpleObject)]
pub struct DeploymentEvidenceSnapshot {
    pub kind: ApprovalEvidenceKind,
    pub digest: Option<String>,
    pub binding_digest: Option<String>,
    pub expires_at: Option<String>,
    pub state: ApprovalEvidenceState,
}

impl From<&AppDeploymentEvidence> for DeploymentEvidenceSnapshot {
    fn from(value: &AppDeploymentEvidence) -> Self {
        Self {
            kind: ApprovalEvidenceKind::parse(&value.kind),
            digest: value.digest.clone(),
            binding_digest: value.binding_digest.clone(),
            expires_at: optional_timestamp(value.expires_at),
            state: ApprovalEvidenceState::parse(&value.state),
        }
    }
}

#[derive(SimpleObject)]
pub struct DeploymentPolicySnapshot {
    pub policy_digest: String,
    pub policy_revision: i32,
    pub logical_environment_class: LogicalEnvironmentClass,
    pub risk: DeploymentRiskLevel,
    pub binding_digest: String,
    pub required_evidence: Vec<ApprovalEvidenceKind>,
    pub required_approvers: i32,
    pub evaluation_requirement_expires_at: Option<String>,
    pub evidence: Vec<DeploymentEvidenceSnapshot>,
}

impl From<&AppDeploymentPolicy> for DeploymentPolicySnapshot {
    fn from(value: &AppDeploymentPolicy) -> Self {
        Self {
            policy_digest: value.policy_digest.clone(),
            policy_revision: value.policy_revision as i32,
            logical_environment_class: LogicalEnvironmentClass::parse(
                &value.logical_environment_class,
            ),
            risk: DeploymentRiskLevel::parse(&value.risk),
            binding_digest: value.binding_digest.clone(),
            required_evidence: value
                .required_evidence
                .iter()
                .map(|kind| ApprovalEvidenceKind::parse(kind))
                .collect(),
            required_approvers: value.required_approvers,
            evaluation_requirement_expires_at: optional_timestamp(
                value.evaluation_requirement_expires_at,
            ),
            evidence: value
                .evidence
                .iter()
                .map(DeploymentEvidenceSnapshot::from)
                .collect(),
        }
    }
}

#[derive(SimpleObject)]
pub struct DeploymentPlanReview {
    pub active_agent_version_number: Option<i32>,
    pub change_summary: Option<String>,
    pub added_dependency_versions: Vec<String>,
    pub removed_dependency_versions: Vec<String>,
}

impl From<&AppDeploymentPlanReview> for DeploymentPlanReview {
    fn from(value: &AppDeploymentPlanReview) -> Self {
        Self {
            active_agent_version_number: value
                .active_agent_version_number
                .map(|value| value as i32),
            change_summary: Some(value.change_summary.clone()),
            added_dependency_versions: value.added_dependency_versions.clone(),
            removed_dependency_versions: value.removed_dependency_versions.clone(),
        }
    }
}

#[derive(SimpleObject)]
pub struct DeploymentPlan {
    pub agent_version_id: async_graphql::ID,
    pub agent_content_digest: String,
    pub environment_definition_version_id: async_graphql::ID,
    pub target_digest: String,
    pub plan_digest: String,
    pub package_digest: String,
    pub package_reference: Option<String>,
    pub compiler_version: String,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub canonical_plan: Option<String>,
    pub review: DeploymentPlanReview,
}

impl From<&AppDeploymentPlan> for DeploymentPlan {
    fn from(value: &AppDeploymentPlan) -> Self {
        Self {
            agent_version_id: async_graphql::ID(value.agent_version_id.to_string()),
            agent_content_digest: value.agent_content_digest.clone(),
            environment_definition_version_id: async_graphql::ID(
                value.environment_definition_version_id.to_string(),
            ),
            target_digest: value.target_digest.clone(),
            plan_digest: value.plan_digest.clone(),
            package_digest: value.package_digest.clone(),
            package_reference: value.package_reference.clone(),
            compiler_version: value.compiler_version.clone(),
            catalog_release_id: value.catalog_release_id.clone(),
            catalog_release_digest: value.catalog_release_digest.clone(),
            canonical_plan: value.canonical_plan.clone(),
            review: DeploymentPlanReview::from(&value.review),
        }
    }
}

#[derive(SimpleObject)]
pub struct DeploymentAttempt {
    pub id: async_graphql::ID,
    pub number: i32,
    pub status: DeploymentAttemptStatus,
    pub generation: i32,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub failure_code: Option<String>,
    pub failure_summary: Option<String>,
}

impl From<&AppDeploymentAttempt> for DeploymentAttempt {
    fn from(value: &AppDeploymentAttempt) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            number: value.number as i32,
            status: DeploymentAttemptStatus::parse(&value.status),
            generation: value.generation as i32,
            started_at: optional_timestamp(value.started_at),
            completed_at: optional_timestamp(value.completed_at),
            failure_code: value.failure_code.clone(),
            failure_summary: value.failure_summary.clone(),
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct DeploymentRuntimeHealth {
    pub status: DeploymentRuntimeHealthStatus,
    pub summary: String,
    pub observed_at: Option<String>,
    pub generation: i32,
}

impl From<&AppDeploymentRuntimeHealth> for DeploymentRuntimeHealth {
    fn from(value: &AppDeploymentRuntimeHealth) -> Self {
        Self {
            status: DeploymentRuntimeHealthStatus::parse(&value.status),
            summary: value.summary.clone(),
            observed_at: optional_timestamp(value.observed_at),
            generation: value.generation as i32,
        }
    }
}

#[derive(SimpleObject)]
pub struct DeploymentRollbackTarget {
    pub deployment_id: async_graphql::ID,
    pub agent_version_id: async_graphql::ID,
    pub agent_version_number: i32,
    pub target_digest: String,
    pub runtime_health: DeploymentRuntimeHealth,
}

impl From<&AppDeploymentRollbackTarget> for DeploymentRollbackTarget {
    fn from(value: &AppDeploymentRollbackTarget) -> Self {
        Self {
            deployment_id: async_graphql::ID(value.deployment_id.to_string()),
            agent_version_id: async_graphql::ID(value.agent_version_id.to_string()),
            agent_version_number: value.agent_version_number as i32,
            target_digest: value.target_digest.clone(),
            runtime_health: DeploymentRuntimeHealth::from(&value.runtime_health),
        }
    }
}

#[derive(SimpleObject)]
pub struct Deployment {
    pub id: async_graphql::ID,
    pub project_id: async_graphql::ID,
    pub agent_id: async_graphql::ID,
    pub agent_display_name: String,
    pub agent_version_id: async_graphql::ID,
    pub agent_version_number: i32,
    pub environment_definition_version: DeploymentEnvironmentDefinitionVersion,
    pub strategy: DeploymentStrategy,
    pub lifecycle_status: DeploymentLifecycleStatus,
    pub revision: i32,
    pub projection_revision: i32,
    pub requested_by: async_graphql::ID,
    pub requested_at: String,
    pub plan: DeploymentPlan,
    pub policy: DeploymentPolicySnapshot,
    pub current_attempt: Option<DeploymentAttempt>,
    pub runtime_health: DeploymentRuntimeHealth,
    pub rollback_target: Option<DeploymentRollbackTarget>,
}

impl From<&AppDeployment> for Deployment {
    fn from(value: &AppDeployment) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            project_id: async_graphql::ID(value.project_id.to_string()),
            agent_id: async_graphql::ID(value.agent_id.to_string()),
            agent_display_name: value.agent_display_name.clone(),
            agent_version_id: async_graphql::ID(value.agent_version_id.to_string()),
            agent_version_number: value.agent_version_number as i32,
            environment_definition_version: DeploymentEnvironmentDefinitionVersion::from(
                &value.environment,
            ),
            strategy: DeploymentStrategy::parse(&value.strategy),
            lifecycle_status: value.lifecycle_status.into(),
            revision: value.revision as i32,
            projection_revision: value.projection_revision as i32,
            requested_by: async_graphql::ID(value.requested_by.to_string()),
            requested_at: timestamp(value.requested_at),
            plan: DeploymentPlan::from(&value.plan),
            policy: DeploymentPolicySnapshot::from(&value.policy),
            current_attempt: value.current_attempt.as_ref().map(DeploymentAttempt::from),
            runtime_health: DeploymentRuntimeHealth::from(&value.runtime_health),
            rollback_target: value
                .rollback_target
                .as_ref()
                .map(DeploymentRollbackTarget::from),
        }
    }
}
impl From<AppDeployment> for Deployment {
    fn from(value: AppDeployment) -> Self {
        Self::from(&value)
    }
}

#[derive(SimpleObject)]
pub struct DeploymentTimelineEvent {
    pub id: async_graphql::ID,
    pub attempt_id: Option<async_graphql::ID>,
    pub attempt_number: i32,
    pub sequence: i32,
    pub stage: String,
    pub status: String,
    pub message: Option<String>,
    pub source: String,
    pub occurred_at: String,
}

impl From<&AppDeploymentTimelineEvent> for DeploymentTimelineEvent {
    fn from(value: &AppDeploymentTimelineEvent) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            attempt_id: value.attempt_id.map(|id| async_graphql::ID(id.to_string())),
            attempt_number: value.attempt_number as i32,
            sequence: value.sequence as i32,
            stage: value.stage.clone(),
            status: value.status.clone(),
            message: value.message.clone(),
            source: value.source.clone(),
            occurred_at: timestamp(value.occurred_at),
        }
    }
}

#[derive(SimpleObject)]
pub struct DeploymentCurrentTarget {
    pub alias_name: Option<String>,
    pub deployment_id: async_graphql::ID,
    pub agent_version_id: async_graphql::ID,
    pub agent_version_number: i32,
    pub target_digest: String,
    pub requested_at: String,
}

impl From<&AppPreviewCurrentTarget> for DeploymentCurrentTarget {
    fn from(value: &AppPreviewCurrentTarget) -> Self {
        Self {
            alias_name: Some(value.alias_name.clone()),
            deployment_id: async_graphql::ID(value.deployment_id.to_string()),
            agent_version_id: async_graphql::ID(value.agent_version_id.to_string()),
            agent_version_number: value.agent_version_number as i32,
            target_digest: value.target_digest.clone(),
            requested_at: timestamp(value.requested_at),
        }
    }
}

#[derive(SimpleObject)]
pub struct DeploymentPreview {
    pub environment_definition_version: DeploymentEnvironmentDefinitionVersion,
    pub strategy: DeploymentStrategy,
    pub risk: DeploymentRiskLevel,
    pub policy_digest: String,
    pub policy_revision: i32,
    pub required_evidence: Vec<ApprovalEvidenceKind>,
    pub required_approvers: i32,
    pub plan_digest: String,
    pub package_digest: String,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub agent_content_digest: String,
    pub target_digest: String,
    pub binding_digest: String,
    pub current_target: Option<DeploymentCurrentTarget>,
    pub requirement_expires_at: Option<String>,
    pub warnings: Vec<String>,
    pub compatibility: String,
}

impl From<AppDeploymentPreview> for DeploymentPreview {
    fn from(value: AppDeploymentPreview) -> Self {
        Self {
            environment_definition_version: DeploymentEnvironmentDefinitionVersion::from(
                &AppDeploymentEnvironment {
                    id: value.environment.id,
                    stable_definition_id: value.environment.stable_definition_id.clone(),
                    version: value.environment.version.clone(),
                    display_name: value.environment.display_name.clone(),
                    logical_environment_class: value.environment.logical_environment_class.clone(),
                    catalog_release_id: value.environment.catalog_release_id.clone(),
                    catalog_release_digest: value.environment.catalog_release_digest.clone(),
                    content_digest: value.environment.content_digest.clone(),
                },
            ),
            strategy: DeploymentStrategy::parse(&value.strategy),
            risk: DeploymentRiskLevel::parse(&value.risk),
            policy_digest: value.policy_digest,
            policy_revision: value.policy_revision as i32,
            required_evidence: value
                .required_evidence
                .iter()
                .map(|kind| ApprovalEvidenceKind::parse(kind))
                .collect(),
            required_approvers: value.required_approvers,
            plan_digest: value.plan_digest,
            package_digest: value.package_digest,
            catalog_release_id: value.catalog_release_id,
            catalog_release_digest: value.catalog_release_digest,
            agent_content_digest: value.agent_content_digest,
            target_digest: value.target_digest,
            binding_digest: value.binding_digest,
            current_target: value
                .current_target
                .as_ref()
                .map(DeploymentCurrentTarget::from),
            requirement_expires_at: Some(timestamp(value.requirement_expires_at)),
            warnings: value.warnings,
            compatibility: value.compatibility,
        }
    }
}

#[derive(SimpleObject)]
pub struct DeploymentPageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

/// Ports the repeated `XxxEdge { cursor, node }` / `XxxConnection { edges, page_info:
/// DeploymentPageInfo }` shape every connection in this file shares, matching the
/// `connection_type!`/`from_app_connection!` pattern `schema/evaluation.rs` already established for
/// the same repetition — adapted here because this domain's application-layer connections carry
/// parallel `nodes`/`cursors` vectors rather than a single `edges` list of `(cursor, node)` pairs.
macro_rules! deployment_connection_type {
    ($connection_name:ident, $edge_name:ident, $node:ty) => {
        #[derive(SimpleObject)]
        pub struct $edge_name {
            pub cursor: String,
            pub node: $node,
        }

        #[derive(SimpleObject)]
        pub struct $connection_name {
            pub edges: Vec<$edge_name>,
            pub page_info: DeploymentPageInfo,
        }
    };
}

macro_rules! from_app_deployment_connection {
    ($app_type:ty, $connection_name:ident, $edge_name:ident, $node_from:path) => {
        impl From<$app_type> for $connection_name {
            fn from(value: $app_type) -> Self {
                let edges = value
                    .nodes
                    .iter()
                    .zip(value.cursors.iter())
                    .map(|(node, cursor)| $edge_name {
                        cursor: cursor.clone(),
                        node: $node_from(node),
                    })
                    .collect();
                Self {
                    edges,
                    page_info: DeploymentPageInfo {
                        has_next_page: value.has_next_page,
                        end_cursor: value.end_cursor,
                    },
                }
            }
        }
    };
}

deployment_connection_type!(DeploymentConnection, DeploymentEdge, Deployment);
from_app_deployment_connection!(
    AppDeploymentConnection,
    DeploymentConnection,
    DeploymentEdge,
    Deployment::from
);

deployment_connection_type!(
    DeploymentTimelineConnection,
    DeploymentTimelineEdge,
    DeploymentTimelineEvent
);
from_app_deployment_connection!(
    AppDeploymentTimelineConnection,
    DeploymentTimelineConnection,
    DeploymentTimelineEdge,
    DeploymentTimelineEvent::from
);

#[derive(SimpleObject)]
pub struct DeploymentDetailProjection {
    pub deployment: Deployment,
    pub timeline: DeploymentTimelineConnection,
}

impl From<AppDeploymentDetailProjection> for DeploymentDetailProjection {
    fn from(value: AppDeploymentDetailProjection) -> Self {
        Self {
            deployment: Deployment::from(&value.deployment),
            timeline: DeploymentTimelineConnection::from(value.timeline),
        }
    }
}

deployment_connection_type!(
    DeploymentEnvironmentDefinitionVersionConnection,
    DeploymentEnvironmentDefinitionVersionEdge,
    DeploymentEnvironmentDefinitionVersion
);
from_app_deployment_connection!(
    AppDeploymentEnvironmentConnection,
    DeploymentEnvironmentDefinitionVersionConnection,
    DeploymentEnvironmentDefinitionVersionEdge,
    DeploymentEnvironmentDefinitionVersion::from
);

// --- problem interface ---

#[derive(SimpleObject)]
pub struct DeploymentNotFoundProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct DeploymentAuthorizationProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct DeploymentValidationProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct DeploymentLifecycleProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct DeploymentIdempotencyProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct DeploymentRateLimitedProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct DeploymentRevisionConflict {
    pub code: String,
    pub message: String,
    pub resource_id: async_graphql::ID,
    pub expected_revision: i32,
    pub actual_revision: i32,
}

#[derive(Interface)]
#[allow(clippy::duplicated_attributes)]
#[graphql(field(name = "code", ty = "String"))]
#[graphql(field(name = "message", ty = "String"))]
pub enum DeploymentProblem {
    NotFound(DeploymentNotFoundProblem),
    Authorization(DeploymentAuthorizationProblem),
    Validation(DeploymentValidationProblem),
    Lifecycle(DeploymentLifecycleProblem),
    Idempotency(DeploymentIdempotencyProblem),
    RateLimited(DeploymentRateLimitedProblem),
    RevisionConflict(DeploymentRevisionConflict),
}

impl From<AppProblem> for DeploymentProblem {
    fn from(problem: AppProblem) -> Self {
        match problem.kind {
            AppProblemKind::NotFound => DeploymentProblem::NotFound(DeploymentNotFoundProblem { code: "NOT_FOUND".to_string(), message: "This deployment is unavailable.".to_string() }),
            AppProblemKind::Forbidden => {
                DeploymentProblem::Authorization(DeploymentAuthorizationProblem { code: "FORBIDDEN".to_string(), message: "You do not have permission to perform this deployment action.".to_string() })
            }
            AppProblemKind::InvalidInput => {
                DeploymentProblem::Validation(DeploymentValidationProblem { code: "INVALID_INPUT".to_string(), message: "Choose immutable deployment facts and a valid idempotency key.".to_string() })
            }
            AppProblemKind::ReasonRequired => DeploymentProblem::Validation(DeploymentValidationProblem { code: "REASON_REQUIRED".to_string(), message: "Enter a rollback reason.".to_string() }),
            AppProblemKind::ConfirmationRequired => {
                DeploymentProblem::Validation(DeploymentValidationProblem { code: "CONFIRMATION_REQUIRED".to_string(), message: "Type the production environment identifier to confirm rollback.".to_string() })
            }
            AppProblemKind::ConfirmationMismatch => {
                DeploymentProblem::Validation(DeploymentValidationProblem { code: "CONFIRMATION_MISMATCH".to_string(), message: "The production environment confirmation does not match.".to_string() })
            }
            AppProblemKind::RevisionConflict => DeploymentProblem::RevisionConflict(DeploymentRevisionConflict {
                code: "REVISION_CONFLICT".to_string(),
                message: "This deployment changed before the action was recorded.".to_string(),
                resource_id: async_graphql::ID(problem.resource_id.map(|id| id.to_string()).unwrap_or_default()),
                expected_revision: problem.expected_revision as i32,
                actual_revision: problem.actual_revision as i32,
            }),
            AppProblemKind::LifecycleConflict => {
                DeploymentProblem::Lifecycle(DeploymentLifecycleProblem { code: "LIFECYCLE_CONFLICT".to_string(), message: "This deployment cannot make that transition.".to_string() })
            }
            AppProblemKind::IdempotencyConflict => {
                DeploymentProblem::Idempotency(DeploymentIdempotencyProblem { code: "IDEMPOTENCY_CONFLICT".to_string(), message: "This idempotency key belongs to a different deployment action.".to_string() })
            }
            AppProblemKind::RateLimited => DeploymentProblem::RateLimited(DeploymentRateLimitedProblem {
                code: "RATE_LIMITED".to_string(),
                message: "The local deployment request limit is reached. Wait for an active request to finish.".to_string(),
            }),
        }
    }
}

#[derive(SimpleObject)]
pub struct DeploymentMutationPayload {
    pub deployment: Option<Deployment>,
    pub problems: Vec<DeploymentProblem>,
}

impl From<AppMutationResult> for DeploymentMutationPayload {
    fn from(result: AppMutationResult) -> Self {
        Self {
            deployment: result.deployment.as_ref().map(Deployment::from),
            problems: result
                .problem
                .into_iter()
                .map(DeploymentProblem::from)
                .collect(),
        }
    }
}

// --- inputs ---

#[derive(InputObject)]
pub struct DeploymentFilter {
    pub agent_id: Option<async_graphql::ID>,
    pub agent_version_id: Option<async_graphql::ID>,
    pub environment_definition_version_id: Option<async_graphql::ID>,
    pub lifecycle_status: Option<DeploymentLifecycleStatus>,
    pub strategy: Option<DeploymentStrategy>,
}

fn parse_id(value: &async_graphql::ID) -> Option<Uuid> {
    Uuid::parse_str(value.as_str()).ok()
}

fn build_filter(
    project_id: &async_graphql::ID,
    filter: Option<DeploymentFilter>,
) -> Option<AppDeploymentFilter> {
    let project_id = parse_id(project_id)?;
    Some(AppDeploymentFilter {
        project_id: Some(project_id),
        agent_id: filter
            .as_ref()
            .and_then(|filter| filter.agent_id.as_ref())
            .and_then(parse_id),
        agent_version_id: filter
            .as_ref()
            .and_then(|filter| filter.agent_version_id.as_ref())
            .and_then(parse_id),
        environment_definition_version_id: filter
            .as_ref()
            .and_then(|filter| filter.environment_definition_version_id.as_ref())
            .and_then(parse_id),
        lifecycle_status: filter
            .as_ref()
            .and_then(|filter| filter.lifecycle_status)
            .map(Into::into),
        strategy: filter
            .as_ref()
            .and_then(|filter| filter.strategy)
            .map(|value| value.value().to_string()),
    })
}

#[derive(InputObject)]
pub struct DeployAgentVersionInput {
    pub agent_version_id: async_graphql::ID,
    pub environment_definition_version_id: async_graphql::ID,
    pub strategy: DeploymentStrategy,
    pub idempotency_key: Option<String>,
}

#[derive(InputObject)]
pub struct CancelDeploymentInput {
    pub deployment_id: async_graphql::ID,
    pub expected_revision: i32,
    pub reason: Option<String>,
}

#[derive(InputObject)]
pub struct RetryDeploymentInput {
    pub deployment_id: async_graphql::ID,
    pub expected_revision: i32,
    pub idempotency_key: String,
}

#[derive(InputObject)]
pub struct PromoteDeploymentInput {
    pub deployment_id: async_graphql::ID,
    pub expected_revision: i32,
    pub idempotency_key: String,
}

#[derive(InputObject)]
pub struct RollbackDeploymentInput {
    pub deployment_id: async_graphql::ID,
    pub target_agent_version_id: Option<async_graphql::ID>,
    pub expected_revision: i32,
    pub reason: String,
    pub production_confirmation: Option<String>,
    pub idempotency_key: String,
}

// --- approval enums ---

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum ApprovalRequirementStatus {
    Pending,
    Satisfied,
    Rejected,
    Expired,
    Invalidated,
}

impl From<hive_domain::deployment::ApprovalRequirementStatus> for ApprovalRequirementStatus {
    fn from(value: hive_domain::deployment::ApprovalRequirementStatus) -> Self {
        use hive_domain::deployment::ApprovalRequirementStatus as Domain;
        match value {
            Domain::Pending => Self::Pending,
            Domain::Satisfied => Self::Satisfied,
            Domain::Rejected => Self::Rejected,
            Domain::Expired => Self::Expired,
            Domain::Invalidated => Self::Invalidated,
        }
    }
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum ApprovalDecisionValue {
    Approve,
    Reject,
}

impl ApprovalDecisionValue {
    fn value(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::Reject => "REJECT",
        }
    }
}

// --- approval object types ---

#[derive(SimpleObject)]
pub struct ProjectApprovalPolicyRule {
    pub required_evidence: Vec<ApprovalEvidenceKind>,
    pub required_distinct_approver_count: i32,
}

impl From<&AppApprovalRule> for ProjectApprovalPolicyRule {
    fn from(value: &AppApprovalRule) -> Self {
        Self {
            required_evidence: value
                .required_evidence
                .iter()
                .map(|kind| ApprovalEvidenceKind::parse(kind))
                .collect(),
            required_distinct_approver_count: value.required_distinct_approver_count,
        }
    }
}

#[derive(SimpleObject)]
pub struct ApprovalTargetSnapshot {
    pub agent_version_id: async_graphql::ID,
    pub agent_version_digest: String,
    pub environment_definition_version_id: async_graphql::ID,
    pub environment_definition_digest: String,
    pub target_digest: String,
    pub deployment_plan_digest: String,
    pub artifact_digest: String,
}

impl From<&AppApprovalTarget> for ApprovalTargetSnapshot {
    fn from(value: &AppApprovalTarget) -> Self {
        Self {
            agent_version_id: async_graphql::ID(value.agent_version_id.to_string()),
            agent_version_digest: value.agent_version_digest.clone(),
            environment_definition_version_id: async_graphql::ID(
                value.environment_definition_version_id.to_string(),
            ),
            environment_definition_digest: value.environment_definition_digest.clone(),
            target_digest: value.target_digest.clone(),
            deployment_plan_digest: value.deployment_plan_digest.clone(),
            artifact_digest: value.artifact_digest.clone(),
        }
    }
}

#[derive(SimpleObject)]
pub struct DeploymentApprovalSnapshot {
    pub policy_digest: String,
    pub policy_revision: i32,
    pub environment_class: LogicalEnvironmentClass,
    pub risk: DeploymentRiskLevel,
    pub risk_level: DeploymentRiskLevel,
    pub rule: ProjectApprovalPolicyRule,
    pub target: ApprovalTargetSnapshot,
    pub evidence: Vec<DeploymentEvidenceSnapshot>,
    pub expires_at: String,
}

impl From<&AppApprovalSnapshot> for DeploymentApprovalSnapshot {
    fn from(value: &AppApprovalSnapshot) -> Self {
        let risk = DeploymentRiskLevel::parse(&value.risk);
        Self {
            policy_digest: value.policy_digest.clone(),
            policy_revision: value.policy_revision as i32,
            environment_class: LogicalEnvironmentClass::parse(&value.environment_class),
            risk,
            risk_level: risk,
            rule: ProjectApprovalPolicyRule::from(&value.rule),
            target: ApprovalTargetSnapshot::from(&value.target),
            evidence: value
                .evidence
                .iter()
                .map(DeploymentEvidenceSnapshot::from)
                .collect(),
            expires_at: timestamp(value.expires_at),
        }
    }
}

fn approval_principal(id: Uuid, principal: Option<&AppApprovalPrincipal>) -> Principal {
    match principal {
        Some(principal) => Principal {
            id: async_graphql::ID(principal.id.to_string()),
            subject: principal.subject.clone(),
        },
        None => Principal {
            id: async_graphql::ID(id.to_string()),
            subject: id.to_string(),
        },
    }
}

#[derive(SimpleObject)]
pub struct ApprovalDecision {
    pub id: async_graphql::ID,
    pub requirement_id: async_graphql::ID,
    pub actor_principal_id: async_graphql::ID,
    pub decision: ApprovalDecisionValueOutput,
    pub comment: Option<String>,
    pub rejection_reason: Option<String>,
    pub eligibility_checked_at: String,
    pub decided_at: String,
}

/// Java declares a single `ApprovalDecisionValue` enum for both `decideDeploymentApproval`'s input
/// and `ApprovalDecision.decision`'s output; async-graphql requires an enum used as both an
/// `InputObject` field and an `Object` field to still be one type, which `ApprovalDecisionValue`
/// already is — this alias just names the output-position use for readability.
pub type ApprovalDecisionValueOutput = ApprovalDecisionValue;

impl From<&AppApprovalDecision> for ApprovalDecision {
    fn from(value: &AppApprovalDecision) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            requirement_id: async_graphql::ID(value.requirement_id.to_string()),
            actor_principal_id: async_graphql::ID(value.actor_principal_id.to_string()),
            decision: match value.value.as_str() {
                "APPROVE" => ApprovalDecisionValue::Approve,
                "REJECT" => ApprovalDecisionValue::Reject,
                other => panic!("unrecognized approval decision value `{other}`"),
            },
            comment: value.comment.clone(),
            rejection_reason: value.rejection_reason.clone(),
            eligibility_checked_at: timestamp(value.eligibility_checked_at),
            decided_at: timestamp(value.decided_at),
        }
    }
}

deployment_connection_type!(
    ApprovalDecisionConnection,
    ApprovalDecisionEdge,
    ApprovalDecision
);
from_app_deployment_connection!(
    AppDecisionConnection,
    ApprovalDecisionConnection,
    ApprovalDecisionEdge,
    ApprovalDecision::from
);

#[derive(SimpleObject)]
#[graphql(complex)]
pub struct ApprovalRequirement {
    pub id: async_graphql::ID,
    pub deployment_id: async_graphql::ID,
    pub project_id: async_graphql::ID,
    pub revision: i32,
    pub revision_number: i32,
    pub status: ApprovalRequirementStatus,
    pub expires_at: Option<String>,
    pub satisfied_at: Option<String>,
    pub rejected_at: Option<String>,
    pub invalidated_at: Option<String>,
    pub requester_id: async_graphql::ID,
    pub requester: Principal,
    pub required_distinct_approver_count: i32,
    pub qualifying_approval_count: i32,
    pub satisfied_participant_ids: Vec<async_graphql::ID>,
    pub satisfied_participants: Vec<Principal>,
    pub approval_snapshot: DeploymentApprovalSnapshot,
    #[graphql(skip)]
    pub decision_preview: Option<(Vec<AppApprovalDecision>, Vec<String>)>,
}

impl From<&AppApprovalRequirement> for ApprovalRequirement {
    fn from(value: &AppApprovalRequirement) -> Self {
        let revision = value.revision as i32;
        Self {
            id: async_graphql::ID(value.id.to_string()),
            deployment_id: async_graphql::ID(value.deployment_id.to_string()),
            project_id: async_graphql::ID(value.project_id.to_string()),
            revision,
            revision_number: revision,
            status: value.status.into(),
            expires_at: optional_timestamp(value.expires_at),
            satisfied_at: optional_timestamp(value.satisfied_at),
            rejected_at: optional_timestamp(value.rejected_at),
            invalidated_at: optional_timestamp(value.invalidated_at),
            requester_id: async_graphql::ID(value.requester_id.to_string()),
            requester: approval_principal(value.requester_id, value.requester.as_ref()),
            required_distinct_approver_count: value.required_distinct_approver_count,
            qualifying_approval_count: value.qualifying_approval_count,
            satisfied_participant_ids: value
                .satisfied_participants
                .iter()
                .map(|id| async_graphql::ID(id.to_string()))
                .collect(),
            satisfied_participants: value
                .satisfied_participant_details
                .iter()
                .map(|principal| approval_principal(principal.id, Some(principal)))
                .collect(),
            approval_snapshot: DeploymentApprovalSnapshot::from(&value.approval_snapshot),
            decision_preview: value
                .decision_preview
                .as_ref()
                .map(|preview| (preview.nodes.clone(), preview.cursors.clone())),
        }
    }
}

#[async_graphql::ComplexObject]
impl ApprovalRequirement {
    /// Ports `Resolver::approvalDecisions`'s in-memory-preview optimization: when the enclosing
    /// `approvalInbox` query asked for `edges/node/requirement/decisions`, the repository
    /// pre-fetched the first decisions page alongside the inbox row; a first-page request
    /// (`after == null`) slices that instead of a second round trip.
    async fn decisions(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 20)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<ApprovalDecisionConnection> {
        if after.is_none() {
            if let Some((nodes, cursors)) = &self.decision_preview {
                let clamped = first.clamp(1, 50) as usize;
                let has_next_page = nodes.len() > clamped;
                let end_cursor = cursors.iter().take(clamped).next_back().cloned();
                let edges = nodes
                    .iter()
                    .zip(cursors.iter())
                    .take(clamped)
                    .map(|(node, cursor)| ApprovalDecisionEdge {
                        cursor: cursor.clone(),
                        node: ApprovalDecision::from(node),
                    })
                    .collect();
                return Ok(ApprovalDecisionConnection {
                    edges,
                    page_info: DeploymentPageInfo {
                        has_next_page,
                        end_cursor,
                    },
                });
            }
        }
        let connection = deployment_service(ctx)?
            .approval_decisions(principal(ctx)?, self.id.as_str(), after.as_deref(), first)
            .await
            .map_err(map_error)?
            .ok_or_else(|| {
                async_graphql::Error::new("Invalid approval decision pagination arguments.")
            })?;
        Ok(ApprovalDecisionConnection::from(connection))
    }
}

#[derive(SimpleObject)]
pub struct ApprovalInboxItem {
    pub requirement: ApprovalRequirement,
    pub deployment: Deployment,
    pub decision_available: bool,
    pub eligible: bool,
    pub status: ApprovalRequirementStatus,
    pub risk_level: DeploymentRiskLevel,
    pub expires_at: Option<String>,
}

impl From<&AppInboxItem> for ApprovalInboxItem {
    fn from(value: &AppInboxItem) -> Self {
        Self {
            requirement: ApprovalRequirement::from(&value.requirement),
            deployment: Deployment::from(&value.deployment),
            decision_available: value.decision_available,
            eligible: value.eligible,
            status: value.requirement.status.into(),
            risk_level: DeploymentRiskLevel::parse(&value.requirement.approval_snapshot.risk),
            expires_at: optional_timestamp(value.requirement.expires_at),
        }
    }
}

deployment_connection_type!(
    ApprovalInboxConnection,
    ApprovalInboxEdge,
    ApprovalInboxItem
);
from_app_deployment_connection!(
    AppInboxConnection,
    ApprovalInboxConnection,
    ApprovalInboxEdge,
    ApprovalInboxItem::from
);

// --- approval problem interface ---

#[derive(SimpleObject)]
pub struct ApprovalPolicyProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct ApprovalIdempotencyProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct ApprovalRequirementRevisionConflict {
    pub code: String,
    pub message: String,
    pub resource_id: async_graphql::ID,
    pub expected_revision: i32,
    pub actual_revision: i32,
}

#[derive(Interface)]
#[allow(clippy::duplicated_attributes)]
#[graphql(field(name = "code", ty = "String"))]
#[graphql(field(name = "message", ty = "String"))]
pub enum DeploymentApprovalProblem {
    Policy(ApprovalPolicyProblem),
    Idempotency(ApprovalIdempotencyProblem),
    RevisionConflict(ApprovalRequirementRevisionConflict),
}

impl From<AppDecisionProblem> for DeploymentApprovalProblem {
    fn from(problem: AppDecisionProblem) -> Self {
        match problem.code.as_str() {
            "NOT_FOUND" => DeploymentApprovalProblem::Policy(ApprovalPolicyProblem {
                code: problem.code,
                message: "This approval requirement is unavailable.".to_string(),
            }),
            "REVISION_CONFLICT" => {
                DeploymentApprovalProblem::RevisionConflict(ApprovalRequirementRevisionConflict {
                    code: problem.code,
                    message: "This approval requirement changed before the decision was recorded."
                        .to_string(),
                    resource_id: async_graphql::ID(
                        problem
                            .resource_id
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                    ),
                    expected_revision: problem.expected_revision as i32,
                    actual_revision: problem.actual_revision as i32,
                })
            }
            "IDEMPOTENCY_CONFLICT" => {
                DeploymentApprovalProblem::Idempotency(ApprovalIdempotencyProblem {
                    code: problem.code,
                    message: "This idempotency key belongs to a different approval decision."
                        .to_string(),
                })
            }
            "REJECTION_REASON_REQUIRED" => {
                DeploymentApprovalProblem::Policy(ApprovalPolicyProblem {
                    code: problem.code,
                    message: "Enter a rejection reason before recording that decision.".to_string(),
                })
            }
            _ => DeploymentApprovalProblem::Policy(ApprovalPolicyProblem {
                code: problem.code,
                message: "This approval decision is unavailable.".to_string(),
            }),
        }
    }
}

#[derive(SimpleObject)]
pub struct DecideDeploymentApprovalPayload {
    pub decision: Option<ApprovalDecision>,
    pub requirement: Option<ApprovalRequirement>,
    pub deployment: Option<Deployment>,
    pub problems: Vec<DeploymentApprovalProblem>,
}

impl From<AppDecisionMutationResult> for DecideDeploymentApprovalPayload {
    fn from(result: AppDecisionMutationResult) -> Self {
        Self {
            decision: result.decision.as_ref().map(ApprovalDecision::from),
            requirement: result.requirement.as_ref().map(ApprovalRequirement::from),
            deployment: result.deployment.as_ref().map(Deployment::from),
            problems: result
                .problem
                .into_iter()
                .map(DeploymentApprovalProblem::from)
                .collect(),
        }
    }
}

#[derive(InputObject)]
pub struct DecideDeploymentApprovalInput {
    pub approval_requirement_id: async_graphql::ID,
    pub expected_revision: i32,
    pub decision: ApprovalDecisionValue,
    pub idempotency_key: async_graphql::ID,
    pub comment: Option<String>,
    pub rejection_reason: Option<String>,
}

// --- resolvers ---

fn deployment_service(
    ctx: &Context<'_>,
) -> async_graphql::Result<DeploymentService<PgDeploymentRepository>> {
    let repository = PgDeploymentRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
    Ok(DeploymentService::new(repository))
}

fn principal(ctx: &Context<'_>) -> async_graphql::Result<Uuid> {
    Ok(ctx.data::<RequestPrincipal>()?.0)
}

fn map_error(error: impl std::fmt::Display) -> async_graphql::Error {
    async_graphql::Error::new(error.to_string())
}

pub struct DeploymentQueries;

#[Object]
impl DeploymentQueries {
    /// Ports `DeploymentGraphql.Resolver.list`.
    async fn deployments(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        #[graphql(default = 20)] first: i32,
        after: Option<String>,
        filter: Option<DeploymentFilter>,
    ) -> async_graphql::Result<Option<DeploymentConnection>> {
        let Some(app_filter) = build_filter(&project_id, filter) else {
            return Ok(None);
        };
        let connection = deployment_service(ctx)?
            .list(principal(ctx)?, Some(&app_filter), after.as_deref(), first)
            .await
            .map_err(map_error)?;
        Ok(connection.map(DeploymentConnection::from))
    }

    /// Ports `DeploymentGraphql.Resolver.timeline`.
    #[graphql(
        deprecation = "Use deploymentProjection for the detail and timeline from an authorized projection."
    )]
    async fn deployment_timeline(
        &self,
        ctx: &Context<'_>,
        deployment_id: async_graphql::ID,
        #[graphql(default = 20)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<Option<DeploymentTimelineConnection>> {
        let connection = deployment_service(ctx)?
            .timeline(
                principal(ctx)?,
                deployment_id.as_str(),
                after.as_deref(),
                first,
            )
            .await
            .map_err(map_error)?;
        Ok(connection.map(DeploymentTimelineConnection::from))
    }

    /// Ports `DeploymentGraphql.Resolver.detail`.
    async fn deployment_projection(
        &self,
        ctx: &Context<'_>,
        deployment_id: async_graphql::ID,
        #[graphql(default = 20)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<Option<DeploymentDetailProjection>> {
        let projection = deployment_service(ctx)?
            .detail(
                principal(ctx)?,
                deployment_id.as_str(),
                after.as_deref(),
                first,
            )
            .await
            .map_err(map_error)?;
        Ok(projection.map(DeploymentDetailProjection::from))
    }

    /// Ports `DeploymentGraphql.Resolver.environments`.
    async fn deployment_environment_definition_versions(
        &self,
        ctx: &Context<'_>,
        agent_version_id: async_graphql::ID,
        #[graphql(default = 20)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<Option<DeploymentEnvironmentDefinitionVersionConnection>> {
        let connection = deployment_service(ctx)?
            .environments(
                principal(ctx)?,
                agent_version_id.as_str(),
                after.as_deref(),
                first,
            )
            .await
            .map_err(map_error)?;
        Ok(connection.map(DeploymentEnvironmentDefinitionVersionConnection::from))
    }

    /// Ports `DeploymentGraphql.Resolver.preview`.
    async fn deployment_preview(
        &self,
        ctx: &Context<'_>,
        agent_version_id: async_graphql::ID,
        environment_definition_version_id: async_graphql::ID,
        strategy: DeploymentStrategy,
    ) -> async_graphql::Result<Option<DeploymentPreview>> {
        let preview = deployment_service(ctx)?
            .preview(
                principal(ctx)?,
                agent_version_id.as_str(),
                environment_definition_version_id.as_str(),
                strategy.value(),
            )
            .await
            .map_err(map_error)?;
        Ok(preview.map(DeploymentPreview::from))
    }

    /// Ports `DeploymentGraphql.Resolver.approvalInbox`. `includeDecisionPreview` is derived from
    /// the selection set (whether the query asks for `edges/node/requirement/decisions`), not a
    /// client argument, so the repository can pre-fetch the first decisions page in the same query
    /// instead of a per-row round trip.
    async fn approval_inbox(
        &self,
        ctx: &Context<'_>,
        organization_id: Option<async_graphql::ID>,
        project_id: Option<async_graphql::ID>,
        #[graphql(default = 20)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<Option<ApprovalInboxConnection>> {
        let include_decision_preview = ctx
            .look_ahead()
            .field("edges")
            .field("node")
            .field("requirement")
            .field("decisions")
            .exists();
        let connection = deployment_service(ctx)?
            .approval_inbox(
                principal(ctx)?,
                organization_id.as_ref().map(|id| id.as_str()),
                project_id.as_ref().map(|id| id.as_str()),
                after.as_deref(),
                first,
                include_decision_preview,
            )
            .await
            .map_err(map_error)?;
        Ok(connection.map(ApprovalInboxConnection::from))
    }

    /// Ports `DeploymentGraphql.Resolver.approvalDetail`. Java names the GraphQL field
    /// `approvalRequirement`, but it resolves through `approvalDetail` and returns the same
    /// `ApprovalInboxItem` wrapper `approvalInbox` does (deployment, eligible, decisionAvailable
    /// alongside the requirement), not a bare `ApprovalRequirement`.
    async fn approval_requirement(
        &self,
        ctx: &Context<'_>,
        approval_requirement_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<ApprovalInboxItem>> {
        let item = deployment_service(ctx)?
            .approval_detail(principal(ctx)?, approval_requirement_id.as_str())
            .await
            .map_err(map_error)?;
        Ok(item.as_ref().map(ApprovalInboxItem::from))
    }
}

pub struct DeploymentMutations;

#[Object]
impl DeploymentMutations {
    /// Ports `DeploymentGraphql.Resolver.deploy`.
    async fn deploy_agent_version(
        &self,
        ctx: &Context<'_>,
        input: DeployAgentVersionInput,
    ) -> async_graphql::Result<DeploymentMutationPayload> {
        let result = deployment_service(ctx)?
            .deploy(
                principal(ctx)?,
                input.agent_version_id.as_str(),
                input.environment_definition_version_id.as_str(),
                input.strategy.value(),
                input.idempotency_key.as_deref().unwrap_or(""),
            )
            .await
            .map_err(map_error)?;
        Ok(DeploymentMutationPayload::from(result))
    }

    /// Ports `DeploymentGraphql.Resolver.cancel`.
    async fn cancel_deployment(
        &self,
        ctx: &Context<'_>,
        input: CancelDeploymentInput,
    ) -> async_graphql::Result<DeploymentMutationPayload> {
        let result = deployment_service(ctx)?
            .cancel(
                principal(ctx)?,
                input.deployment_id.as_str(),
                input.expected_revision as i64,
                input.reason.as_deref().unwrap_or(""),
            )
            .await
            .map_err(map_error)?;
        Ok(DeploymentMutationPayload::from(result))
    }

    /// Ports `DeploymentGraphql.Resolver.retry`.
    async fn retry_deployment(
        &self,
        ctx: &Context<'_>,
        input: RetryDeploymentInput,
    ) -> async_graphql::Result<DeploymentMutationPayload> {
        let result = deployment_service(ctx)?
            .retry(
                principal(ctx)?,
                input.deployment_id.as_str(),
                input.expected_revision as i64,
                &input.idempotency_key,
            )
            .await
            .map_err(map_error)?;
        Ok(DeploymentMutationPayload::from(result))
    }

    /// Ports `DeploymentGraphql.Resolver.promote`.
    async fn promote_deployment(
        &self,
        ctx: &Context<'_>,
        input: PromoteDeploymentInput,
    ) -> async_graphql::Result<DeploymentMutationPayload> {
        let result = deployment_service(ctx)?
            .promote(
                principal(ctx)?,
                input.deployment_id.as_str(),
                input.expected_revision as i64,
                &input.idempotency_key,
            )
            .await
            .map_err(map_error)?;
        Ok(DeploymentMutationPayload::from(result))
    }

    /// Ports `DeploymentGraphql.Resolver.rollback`.
    async fn rollback_deployment(
        &self,
        ctx: &Context<'_>,
        input: RollbackDeploymentInput,
    ) -> async_graphql::Result<DeploymentMutationPayload> {
        let result = deployment_service(ctx)?
            .rollback(
                principal(ctx)?,
                input.deployment_id.as_str(),
                input.target_agent_version_id.as_ref().map(|id| id.as_str()),
                input.expected_revision as i64,
                &input.reason,
                input.production_confirmation.as_deref(),
                &input.idempotency_key,
            )
            .await
            .map_err(map_error)?;
        Ok(DeploymentMutationPayload::from(result))
    }

    /// Ports `DeploymentGraphql.Resolver.decide`. `idempotencyKey` doubles as `decide_approval`'s
    /// `request_id` (parsed as a UUID, unlike the other deployment mutations' free-form idempotency
    /// keys); `correlation_id` comes from the ambient per-HTTP-request id `graphql::graphql`
    /// inserts, mirroring Java's `environment.getGraphQlContext().get("requestCorrelationId")`.
    async fn decide_deployment_approval(
        &self,
        ctx: &Context<'_>,
        input: DecideDeploymentApprovalInput,
    ) -> async_graphql::Result<DecideDeploymentApprovalPayload> {
        let correlation_id = ctx.data::<RequestCorrelationId>()?.0.to_string();
        let result = deployment_service(ctx)?
            .decide_approval(
                principal(ctx)?,
                input.approval_requirement_id.as_str(),
                input.expected_revision as i64,
                input.decision.value(),
                input.comment.as_deref(),
                input.rejection_reason.as_deref(),
                input.idempotency_key.as_str(),
                &correlation_id,
            )
            .await
            .map_err(map_error)?;
        Ok(DecideDeploymentApprovalPayload::from(result))
    }
}
