//! Ports `deployment.graphql` and `deploymentApi.ts`: deployments, their recovery actions, and approvals.

use crate::graphql::{execute_within, schema, GraphqlError};

/// A deployment request that has no answer after this long is reported as a failure.
const REQUEST_TIMEOUT_MILLIS: i32 = 10_000;
use cynic::{MutationBuilder, QueryBuilder};

pub use super::enums::{
    ApprovalDecisionValue, ApprovalEvidenceKind, ApprovalEvidenceState, ApprovalRequirementStatus,
    DeploymentAttemptStatus, DeploymentLifecycleStatus, DeploymentRiskLevel,
    DeploymentRuntimeHealthStatus, DeploymentStrategy, LogicalEnvironmentClass,
};

cynic::impl_scalar!(i64, schema::Long);

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentEnvironmentDefinitionVersion {
    pub id: cynic::Id,
    pub stable_definition_id: String,
    pub version: String,
    pub display_name: String,
    pub logical_environment_class: LogicalEnvironmentClass,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentEvidenceSnapshot {
    pub kind: ApprovalEvidenceKind,
    pub digest: Option<String>,
    pub expires_at: Option<String>,
    pub state: ApprovalEvidenceState,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentPlanReview {
    pub active_agent_version_number: Option<i64>,
    pub change_summary: Option<String>,
    pub added_dependency_versions: Vec<String>,
    pub removed_dependency_versions: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentPlan {
    pub agent_version_id: cynic::Id,
    pub agent_content_digest: String,
    pub target_digest: String,
    pub plan_digest: String,
    pub package_digest: String,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub review: DeploymentPlanReview,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentPolicySnapshot {
    pub policy_digest: String,
    pub policy_revision: i64,
    pub risk: DeploymentRiskLevel,
    pub required_evidence: Vec<ApprovalEvidenceKind>,
    pub evidence: Vec<DeploymentEvidenceSnapshot>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentRuntimeHealth {
    pub status: DeploymentRuntimeHealthStatus,
    pub summary: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentAttempt {
    pub number: i64,
    pub status: DeploymentAttemptStatus,
    pub failure_code: Option<String>,
    pub failure_summary: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentRollbackTarget {
    pub agent_version_id: cynic::Id,
    pub agent_version_number: i64,
    pub runtime_health: DeploymentRuntimeHealth,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct Deployment {
    pub id: cynic::Id,
    pub project_id: cynic::Id,
    pub agent_id: cynic::Id,
    pub agent_display_name: String,
    pub agent_version_number: i64,
    pub strategy: DeploymentStrategy,
    pub lifecycle_status: DeploymentLifecycleStatus,
    pub revision: i64,
    pub projection_revision: i64,
    pub requested_at: String,
    pub environment_definition_version: DeploymentEnvironmentDefinitionVersion,
    pub plan: DeploymentPlan,
    pub policy: DeploymentPolicySnapshot,
    pub current_attempt: Option<DeploymentAttempt>,
    pub runtime_health: DeploymentRuntimeHealth,
    pub rollback_target: Option<DeploymentRollbackTarget>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentCurrentTarget {
    pub agent_version_number: i64,
    pub target_digest: String,
    pub requested_at: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentPreview")]
pub struct DeploymentPreviewFields {
    pub environment_definition_version: DeploymentEnvironmentDefinitionVersion,
    pub strategy: DeploymentStrategy,
    pub risk: DeploymentRiskLevel,
    pub policy_digest: String,
    pub policy_revision: i64,
    pub required_evidence: Vec<ApprovalEvidenceKind>,
    pub required_approvers: i32,
    pub plan_digest: String,
    pub package_digest: String,
    pub agent_content_digest: String,
    pub target_digest: String,
    pub binding_digest: String,
    pub current_target: Option<DeploymentCurrentTarget>,
    pub requirement_expires_at: Option<String>,
    pub warnings: Vec<String>,
    pub compatibility: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentTimelineEvent {
    pub id: cynic::Id,
    pub stage: String,
    pub status: String,
    pub message: String,
    pub source: String,
    pub occurred_at: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "DeploymentProblem")]
pub struct DeploymentProblemFields {
    pub message: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct DeploymentMutationPayload {
    pub deployment: Option<Deployment>,
    pub problems: Vec<DeploymentProblemFields>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct DeploymentEnvironmentDefinitionVersionEdge {
    pub node: DeploymentEnvironmentDefinitionVersion,
}
#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct DeploymentEnvironmentDefinitionVersionConnection {
    pub edges: Vec<DeploymentEnvironmentDefinitionVersionEdge>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DeploymentEnvironmentsVariables {
    pub agent_version_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "DeploymentEnvironmentsVariables")]
pub struct DeploymentEnvironments {
    #[arguments(agentVersionId: $agent_version_id, first: 50)]
    pub deployment_environment_definition_versions:
        Option<DeploymentEnvironmentDefinitionVersionConnection>,
}

pub async fn request_deployment_environments(
    agent_version_id: &str,
) -> Result<Option<Vec<DeploymentEnvironmentDefinitionVersion>>, GraphqlError> {
    let data = execute_within(
        DeploymentEnvironments::build(DeploymentEnvironmentsVariables {
            agent_version_id: agent_version_id.into(),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?;
    Ok(data
        .deployment_environment_definition_versions
        .map(|connection| connection.edges.into_iter().map(|edge| edge.node).collect()))
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DeploymentPreviewVariables {
    pub agent_version_id: cynic::Id,
    pub environment_definition_version_id: cynic::Id,
    pub strategy: DeploymentStrategy,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "DeploymentPreviewVariables")]
pub struct DeploymentPreview {
    #[arguments(agentVersionId: $agent_version_id, environmentDefinitionVersionId: $environment_definition_version_id, strategy: $strategy)]
    pub deployment_preview: Option<DeploymentPreviewFields>,
}

pub async fn request_deployment_preview(
    agent_version_id: &str,
    environment_id: &str,
    strategy: DeploymentStrategy,
) -> Result<Option<DeploymentPreviewFields>, GraphqlError> {
    let variables = DeploymentPreviewVariables {
        agent_version_id: agent_version_id.into(),
        environment_definition_version_id: environment_id.into(),
        strategy,
    };
    Ok(
        execute_within(DeploymentPreview::build(variables), REQUEST_TIMEOUT_MILLIS)
            .await?
            .deployment_preview,
    )
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct DeployAgentVersionInput {
    pub agent_version_id: cynic::Id,
    pub environment_definition_version_id: cynic::Id,
    pub strategy: DeploymentStrategy,
    pub idempotency_key: Option<String>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct CancelDeploymentInput {
    pub deployment_id: cynic::Id,
    pub expected_revision: i64,
    pub reason: Option<String>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct RetryDeploymentInput {
    pub deployment_id: cynic::Id,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct PromoteDeploymentInput {
    pub deployment_id: cynic::Id,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct RollbackDeploymentInput {
    pub deployment_id: cynic::Id,
    pub target_agent_version_id: Option<cynic::Id>,
    pub expected_revision: i64,
    pub reason: String,
    pub production_confirmation: Option<String>,
    pub idempotency_key: String,
}

macro_rules! deployment_mutation {
    ($d:tt, $root:ident, $variables:ident, $variables_name:literal, $input_type:ty, $field:ident, $call:ident) => {
        #[derive(cynic::QueryVariables, Debug)]
        pub struct $variables {
            pub input: $input_type,
        }

        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "Mutation", variables = $variables_name)]
        pub struct $root {
            #[arguments(input: $d input)]
            pub $field: DeploymentMutationPayload,
        }

        pub async fn $call(input: $input_type) -> Result<DeploymentMutationPayload, GraphqlError> {
            Ok(
                execute_within($root::build($variables { input }), REQUEST_TIMEOUT_MILLIS)
                    .await?
                    .$field,
            )
        }
    };
}

deployment_mutation!($, DeployAgentVersion, DeployAgentVersionVariables, "DeployAgentVersionVariables", DeployAgentVersionInput, deploy_agent_version, deploy_agent_version);
deployment_mutation!($, CancelDeployment, CancelDeploymentVariables, "CancelDeploymentVariables", CancelDeploymentInput, cancel_deployment, cancel_deployment);
deployment_mutation!($, RetryDeployment, RetryDeploymentVariables, "RetryDeploymentVariables", RetryDeploymentInput, retry_deployment, retry_deployment);
deployment_mutation!($, PromoteDeployment, PromoteDeploymentVariables, "PromoteDeploymentVariables", PromoteDeploymentInput, promote_deployment, promote_deployment);
deployment_mutation!($, RollbackDeployment, RollbackDeploymentVariables, "RollbackDeploymentVariables", RollbackDeploymentInput, rollback_deployment, rollback_deployment);

#[derive(cynic::InputObject, Debug, Clone)]
pub struct DeploymentFilter {
    pub agent_id: Option<cynic::Id>,
    pub agent_version_id: Option<cynic::Id>,
    pub environment_definition_version_id: Option<cynic::Id>,
    pub lifecycle_status: Option<DeploymentLifecycleStatus>,
    pub strategy: Option<DeploymentStrategy>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct DeploymentEdge {
    pub node: Deployment,
}
#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct DeploymentConnection {
    pub edges: Vec<DeploymentEdge>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DeploymentsVariables {
    pub project_id: cynic::Id,
    pub after: Option<String>,
    pub filter: Option<DeploymentFilter>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "DeploymentsVariables")]
pub struct Deployments {
    #[arguments(projectId: $project_id, first: 50, after: $after, filter: $filter)]
    pub deployments: Option<DeploymentConnection>,
}

/// `Ok(None)` is the server's "unavailable". `agent_id` narrows the list to one agent.
pub async fn request_deployments(
    project_id: &str,
    agent_id: Option<&str>,
) -> Result<Option<Vec<Deployment>>, GraphqlError> {
    let filter = agent_id.map(|agent| DeploymentFilter {
        agent_id: Some(agent.into()),
        agent_version_id: None,
        environment_definition_version_id: None,
        lifecycle_status: None,
        strategy: None,
    });
    let variables = DeploymentsVariables {
        project_id: project_id.into(),
        after: None,
        filter,
    };
    Ok(
        execute_within(Deployments::build(variables), REQUEST_TIMEOUT_MILLIS)
            .await?
            .deployments
            .map(|connection| connection.edges.into_iter().map(|edge| edge.node).collect()),
    )
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct DeploymentTimelineEdge {
    pub node: DeploymentTimelineEvent,
}
#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct DeploymentTimelineConnection {
    pub edges: Vec<DeploymentTimelineEdge>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct DeploymentDetailProjection {
    pub deployment: Deployment,
    pub timeline: DeploymentTimelineConnection,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DeploymentPollingVariables {
    pub deployment_id: cynic::Id,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "DeploymentPollingVariables")]
pub struct DeploymentPolling {
    #[arguments(deploymentId: $deployment_id, first: 100, after: $after)]
    pub deployment_projection: Option<DeploymentDetailProjection>,
}

/// The deployment and its timeline from one authorized projection. `Ok(None)` is "unavailable".
pub async fn request_deployment(
    deployment_id: &str,
) -> Result<Option<(Deployment, Vec<DeploymentTimelineEvent>)>, GraphqlError> {
    let variables = DeploymentPollingVariables {
        deployment_id: deployment_id.into(),
        after: None,
    };
    Ok(
        execute_within(DeploymentPolling::build(variables), REQUEST_TIMEOUT_MILLIS)
            .await?
            .deployment_projection
            .map(|projection| {
                (
                    projection.deployment,
                    projection
                        .timeline
                        .edges
                        .into_iter()
                        .map(|edge| edge.node)
                        .collect(),
                )
            }),
    )
}

/// A service that predates approvals rejects these documents at validation. The pages treat that
/// as "unavailable" rather than as a failed request.
fn unsupported_approval_schema(error: &GraphqlError) -> bool {
    let GraphqlError::Transport(message) = error else {
        return false;
    };
    ["Cannot query field", "FieldUndefined", "is undefined"]
        .iter()
        .any(|marker| message.contains(marker))
        && [
            "approvalInbox",
            "approvalRequirement",
            "decideDeploymentApproval",
            "ApprovalRequirement",
            "ApprovalInbox",
            "DecideDeploymentApproval",
        ]
        .iter()
        .any(|name| message.contains(name))
}

fn approval_result<T>(result: Result<Option<T>, GraphqlError>) -> Result<Option<T>, GraphqlError> {
    match result {
        Err(error) if unsupported_approval_schema(&error) => Ok(None),
        other => other,
    }
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Principal")]
pub struct Requester {
    pub id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ProjectApprovalPolicyRule {
    pub required_evidence: Vec<ApprovalEvidenceKind>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ApprovalTargetSnapshot {
    pub agent_version_digest: String,
    pub target_digest: String,
    pub deployment_plan_digest: String,
    pub artifact_digest: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentApprovalSnapshot {
    pub policy_digest: String,
    pub policy_revision: i64,
    pub environment_class: LogicalEnvironmentClass,
    pub risk: DeploymentRiskLevel,
    pub rule: ProjectApprovalPolicyRule,
    pub target: ApprovalTargetSnapshot,
    pub evidence: Vec<DeploymentEvidenceSnapshot>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ApprovalDecision {
    pub id: cynic::Id,
    pub decision: ApprovalDecisionValue,
    pub comment: Option<String>,
    pub rejection_reason: Option<String>,
    pub decided_at: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ApprovalDecisionEdge {
    pub node: ApprovalDecision,
}
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ApprovalDecisionConnection {
    pub edges: Vec<ApprovalDecisionEdge>,
}

/// One requirement fragment for the inbox and the detail page; the detail's decision history is a
/// short first page, so the inbox carries it too rather than paying for a second fragment.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "ApprovalRequirement")]
pub struct ApprovalRequirementFields {
    pub id: cynic::Id,
    pub revision: i64,
    pub status: ApprovalRequirementStatus,
    pub expires_at: String,
    pub requester: Requester,
    pub required_distinct_approver_count: i32,
    pub qualifying_approval_count: i32,
    pub approval_snapshot: DeploymentApprovalSnapshot,
    #[arguments(first: 20)]
    pub decisions: ApprovalDecisionConnection,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ApprovalInboxItem {
    pub decision_available: bool,
    pub requirement: ApprovalRequirementFields,
    pub deployment: Deployment,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ApprovalInboxEdge {
    pub node: ApprovalInboxItem,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct DeploymentPageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ApprovalInboxConnection {
    pub edges: Vec<ApprovalInboxEdge>,
    pub page_info: DeploymentPageInfo,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ApprovalInboxVariables {
    pub organization_id: Option<cynic::Id>,
    pub project_id: Option<cynic::Id>,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ApprovalInboxVariables")]
pub struct ApprovalInbox {
    #[arguments(organizationId: $organization_id, projectId: $project_id, first: 50, after: $after)]
    pub approval_inbox: Option<ApprovalInboxConnection>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalInboxPage {
    pub rows: Vec<ApprovalInboxItem>,
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

pub async fn request_approval_inbox(
    organization_id: Option<&str>,
    after: Option<String>,
) -> Result<Option<ApprovalInboxPage>, GraphqlError> {
    let variables = ApprovalInboxVariables {
        organization_id: organization_id.map(Into::into),
        project_id: None,
        after,
    };
    let found = execute_within(ApprovalInbox::build(variables), REQUEST_TIMEOUT_MILLIS).await;
    Ok(
        approval_result(found.map(|data| data.approval_inbox))?.map(|connection| {
            ApprovalInboxPage {
                rows: connection.edges.into_iter().map(|edge| edge.node).collect(),
                has_next_page: connection.page_info.has_next_page,
                end_cursor: connection.page_info.end_cursor,
            }
        }),
    )
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ApprovalRequirementVariables {
    pub approval_requirement_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ApprovalRequirementVariables")]
pub struct ApprovalRequirement {
    #[arguments(approvalRequirementId: $approval_requirement_id)]
    pub approval_requirement: Option<ApprovalInboxItem>,
}

pub async fn request_approval_requirement(
    id: &str,
) -> Result<Option<ApprovalInboxItem>, GraphqlError> {
    let variables = ApprovalRequirementVariables {
        approval_requirement_id: id.into(),
    };
    let found = execute_within(
        ApprovalRequirement::build(variables),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await;
    approval_result(found.map(|data| data.approval_requirement))
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct DecideDeploymentApprovalInput {
    pub approval_requirement_id: cynic::Id,
    pub expected_revision: i64,
    pub decision: ApprovalDecisionValue,
    pub idempotency_key: cynic::Id,
    pub comment: Option<String>,
    pub rejection_reason: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "DeploymentApprovalProblem")]
pub struct DeploymentApprovalProblemFields {
    pub message: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct DecideDeploymentApprovalPayload {
    pub problems: Vec<DeploymentApprovalProblemFields>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DecideDeploymentApprovalVariables {
    pub input: DecideDeploymentApprovalInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    graphql_type = "Mutation",
    variables = "DecideDeploymentApprovalVariables"
)]
pub struct DecideDeploymentApproval {
    #[arguments(input: $input)]
    pub decide_deployment_approval: DecideDeploymentApprovalPayload,
}

pub async fn decide_deployment_approval(
    input: DecideDeploymentApprovalInput,
) -> Result<Option<DecideDeploymentApprovalPayload>, GraphqlError> {
    let operation = DecideDeploymentApproval::build(DecideDeploymentApprovalVariables { input });
    let decided = execute_within(operation, REQUEST_TIMEOUT_MILLIS).await;
    approval_result(decided.map(|data| Some(data.decide_deployment_approval)))
}
