//! Deployments and their recovery actions. The reads are the Seaography-generated `deployments`
//! and `environmentDefinitionVersions` entity queries; the nested plan, policy, evidence, attempt,
//! runtime health, rollback target and timeline are the generated object's own relations and
//! computed fields. The five commands and the approval surface keep their own payload types, and
//! those payloads answer the same generated `Deployments` object, so one fragment covers them all.

use crate::api::generated::{
    is_uuid, OrderByEnum, PageInput, PaginationInput, ProjectsFilterInput, TextFilterInput,
};
use crate::api::page::{GeneratedPageInfo, Page};
use crate::graphql::{execute_within, schema, GeneratedJson, GraphqlError};

/// A deployment request that has no answer after this long is reported as a failure.
const REQUEST_TIMEOUT_MILLIS: i32 = 10_000;
/// The deployments one list request asks for.
const PAGE_SIZE: i32 = 50;

const DEPLOYMENT_VIEW: &str = "DEPLOYMENT.VIEW";

use cynic::{MutationBuilder, QueryBuilder};

pub use super::enums::{
    ApprovalDecisionValue, ApprovalEvidenceKind, DeploymentStrategy, LogicalEnvironmentClass,
};

cynic::impl_scalar!(i64, schema::Long);

fn page(number: i32) -> PaginationInput {
    PaginationInput::Page(PageInput {
        limit: PAGE_SIZE,
        page: number,
    })
}

fn one() -> PaginationInput {
    PaginationInput::Page(PageInput { limit: 1, page: 0 })
}

fn scope(project_id: &str) -> ProjectsFilterInput {
    ProjectsFilterInput {
        id: Some(TextFilterInput::eq(project_id)),
        ..Default::default()
    }
}

/// A JSON column that stores a list of strings.
fn strings(value: &GeneratedJson) -> Vec<String> {
    serde_json::from_value(value.0.clone()).unwrap_or_default()
}

/// The project's capability codes, or `None` when the project itself is not visible. A list whose
/// scope the principal cannot see is "unavailable", not an empty list.
#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Projects")]
pub struct ProjectCapabilities {
    pub capabilities: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectsConnection")]
pub struct ProjectScopeConnection {
    pub nodes: Vec<ProjectCapabilities>,
}

impl ProjectScopeConnection {
    fn holds(&self, code: &str) -> bool {
        self.nodes
            .first()
            .is_some_and(|node| node.capabilities.iter().any(|held| held == code))
    }
}

// --- the generated filter, order and row fragments ---------------------------------------------

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct DeploymentsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<TextFilterInput>,
}

/// Newest first; the key is the final tie-break, so deployments requested in one instant keep one
/// order across pages.
#[derive(cynic::InputObject, Debug, Clone)]
pub struct DeploymentsOrderInput {
    pub requested_at: OrderByEnum,
    pub id: OrderByEnum,
}

fn newest_first() -> DeploymentsOrderInput {
    DeploymentsOrderInput {
        requested_at: OrderByEnum::Desc,
        id: OrderByEnum::Desc,
    }
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct AgentVersionsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<TextFilterInput>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct EnvironmentDefinitionVersionsOrderInput {
    pub stable_definition_id: OrderByEnum,
    pub version: OrderByEnum,
    pub id: OrderByEnum,
}

/// A published environment definition version, read through the generated entity API.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EnvironmentDefinitionVersions")]
pub struct EnvironmentDefinitionVersion {
    pub id: String,
    pub stable_definition_id: String,
    pub version: String,
    pub display_name: String,
    pub logical_environment_class: String,
    pub catalog_release_digest: String,
}

impl EnvironmentDefinitionVersion {
    pub fn environment_class(&self) -> Option<LogicalEnvironmentClass> {
        LogicalEnvironmentClass::from_wire(&self.logical_environment_class)
    }
}

/// The approval surface's own evidence type, which is not the generated entity: it lists the
/// required kinds, so it has a `MISSING` state the stored snapshots cannot have. Both vocabularies
/// are `String` on the wire, as every text enum is; the registered `ApprovalEvidenceKind` and
/// `ApprovalEvidenceState` enums still check this console's own spelling against the schema.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentEvidenceSnapshot {
    pub kind: String,
    pub digest: Option<String>,
    pub expires_at: Option<String>,
    pub state: String,
}

/// The alias target a preview reports, which is not a generated entity row: the digests are the
/// *current* target's.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentPreviewTarget")]
pub struct DeploymentCurrentTarget {
    pub agent_version_number: i32,
    pub target_digest: String,
    pub requested_at: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentEvidenceSnapshots")]
pub struct DeploymentEvidence {
    pub evidence_kind: String,
    pub evidence_digest: String,
    pub expires_at: Option<String>,
    pub state: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentEvidenceSnapshotsConnection")]
pub struct DeploymentEvidenceConnection {
    pub nodes: Vec<DeploymentEvidence>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentPlanReview {
    pub active_agent_version_number: Option<i32>,
    pub change_summary: String,
    pub added_dependency_versions: Vec<String>,
    pub removed_dependency_versions: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentPlanVersions")]
pub struct DeploymentPlan {
    pub agent_version_id: String,
    pub agent_content_digest: Option<String>,
    pub environment_definition_version_id: Option<String>,
    pub target_digest: Option<String>,
    pub plan_digest: String,
    pub package_digest: String,
    pub package_reference: String,
    pub catalog_release_id: String,
    pub catalog_release_digest: Option<String>,
    pub review: DeploymentPlanReview,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentPlanVersions")]
pub struct RollbackPlan {
    pub target_digest: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentPolicySnapshots")]
pub struct DeploymentPolicySnapshot {
    pub policy_digest: String,
    pub policy_revision: i32,
    pub risk: String,
    pub binding_digest: Option<String>,
    pub required_evidence: GeneratedJson,
    pub required_approvers: i32,
    pub evaluation_requirement_expires_at: Option<String>,
}

impl DeploymentPolicySnapshot {
    /// The evidence kinds this frozen policy requires, dropping any the schema's vocabulary does
    /// not hold.
    pub fn required_evidence(&self) -> Vec<ApprovalEvidenceKind> {
        strings(&self.required_evidence)
            .iter()
            .filter_map(|kind| ApprovalEvidenceKind::from_wire(kind))
            .collect()
    }
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentRuntimeHealth")]
pub struct DeploymentRuntimeHealthFields {
    pub status: String,
    pub summary: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentAttempts")]
pub struct DeploymentAttempt {
    pub attempt_number: i32,
    pub status: String,
    pub failure_code: Option<String>,
    pub failure_summary: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Agents")]
pub struct DeploymentAgent {
    pub display_name: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentVersions")]
pub struct DeploymentAgentVersion {
    pub version_number: i32,
}

/// The prior active target a rollback would return to: a deployment row of its own.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Deployments")]
pub struct DeploymentRollbackTarget {
    pub id: String,
    pub agent_version_id: String,
    pub agent_versions: Option<DeploymentAgentVersion>,
    pub plan: Option<RollbackPlan>,
    pub deployment_runtime_health: Option<DeploymentRuntimeHealthFields>,
}

impl DeploymentRollbackTarget {
    pub fn agent_version_number(&self) -> i32 {
        self.agent_versions
            .as_ref()
            .map_or(0, |version| version.version_number)
    }
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DeploymentTimelineEvent {
    pub id: String,
    pub stage: String,
    pub status: String,
    pub message: String,
    pub source: String,
    pub occurred_at: String,
}

/// The lean row the deployment list renders.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Deployments")]
pub struct DeploymentListItem {
    pub id: String,
    pub lifecycle_status: String,
    pub strategy: String,
    pub requested_at: String,
    pub agents: Option<DeploymentAgent>,
    pub agent_versions: Option<DeploymentAgentVersion>,
    pub environment_definition_versions: Option<EnvironmentDefinitionVersion>,
}

impl DeploymentListItem {
    pub fn agent_display_name(&self) -> String {
        self.agents
            .as_ref()
            .map(|agent| agent.display_name.clone())
            .unwrap_or_default()
    }

    pub fn agent_version_number(&self) -> i32 {
        self.agent_versions
            .as_ref()
            .map_or(0, |version| version.version_number)
    }
}

/// The full deployment the detail page renders, and the row every command payload answers.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Deployments")]
pub struct Deployment {
    pub id: String,
    pub project_id: String,
    pub agent_id: String,
    pub agent_version_id: String,
    pub strategy: String,
    pub lifecycle_status: String,
    pub revision: i32,
    pub projection_revision: Option<i32>,
    pub requested_at: String,
    pub agents: Option<DeploymentAgent>,
    pub agent_versions: Option<DeploymentAgentVersion>,
    pub environment_definition_versions: Option<EnvironmentDefinitionVersion>,
    pub plan: Option<DeploymentPlan>,
    pub deployment_policy_snapshots: Option<DeploymentPolicySnapshot>,
    pub deployment_evidence_snapshots: DeploymentEvidenceConnection,
    pub current_attempt: Option<DeploymentAttempt>,
    pub deployment_runtime_health: Option<DeploymentRuntimeHealthFields>,
    pub rollback_target: Option<DeploymentRollbackTarget>,
    /// The lifecycle state machine and the recovery preconditions, decided by the server against
    /// the requesting principal's capabilities.
    pub terminal: bool,
    pub can_cancel: bool,
    pub can_retry: bool,
    pub can_promote: bool,
    pub can_rollback: bool,
    #[arguments(first: 100)]
    pub timeline: Vec<DeploymentTimelineEvent>,
}

impl Deployment {
    pub fn agent_display_name(&self) -> String {
        self.agents
            .as_ref()
            .map(|agent| agent.display_name.clone())
            .unwrap_or_default()
    }

    pub fn agent_version_number(&self) -> i32 {
        self.agent_versions
            .as_ref()
            .map_or(0, |version| version.version_number)
    }

    /// The evidence kinds this deployment's frozen policy requires.
    pub fn required_evidence(&self) -> Vec<ApprovalEvidenceKind> {
        self.deployment_policy_snapshots
            .as_ref()
            .map(DeploymentPolicySnapshot::required_evidence)
            .unwrap_or_default()
    }

    /// The frozen evidence snapshots, in evidence-kind order.
    pub fn evidence(&self) -> Vec<DeploymentEvidence> {
        let mut rows = self.deployment_evidence_snapshots.nodes.clone();
        rows.sort_by(|left, right| left.evidence_kind.cmp(&right.evidence_kind));
        rows
    }

    /// The projection revision; the column is nullable, and an unwritten projection is 0.
    pub fn projection(&self) -> i32 {
        self.projection_revision.unwrap_or_default()
    }
}

// --- the environment picker --------------------------------------------------------------------

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "EnvironmentDefinitionVersionsConnection")]
pub struct EnvironmentDefinitionVersionsConnection {
    pub nodes: Vec<EnvironmentDefinitionVersion>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "CatalogReleases",
    variables = "DeploymentEnvironmentsVariables"
)]
pub struct DeploymentCatalogRelease {
    #[arguments(orderBy: $order_by, pagination: $pagination)]
    pub environment_definition_versions: EnvironmentDefinitionVersionsConnection,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "AgentVersions",
    variables = "DeploymentEnvironmentsVariables"
)]
pub struct DeploymentAgentVersionRelease {
    pub catalog_releases: Option<DeploymentCatalogRelease>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "AgentVersionsConnection",
    variables = "DeploymentEnvironmentsVariables"
)]
pub struct DeploymentAgentVersionsConnection {
    pub nodes: Vec<DeploymentAgentVersionRelease>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DeploymentEnvironmentsVariables {
    pub filters: AgentVersionsFilterInput,
    pub version_pagination: PaginationInput,
    pub order_by: EnvironmentDefinitionVersionsOrderInput,
    pub pagination: PaginationInput,
}

/// The environments a version may deploy to: the published environment definition versions of the
/// version's own catalog release, reached through the generated relation.
#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "DeploymentEnvironmentsVariables")]
pub struct DeploymentEnvironments {
    #[arguments(filters: $filters, pagination: $version_pagination)]
    pub agent_versions: DeploymentAgentVersionsConnection,
}

pub async fn request_deployment_environments(
    agent_version_id: &str,
) -> Result<Option<Vec<EnvironmentDefinitionVersion>>, GraphqlError> {
    if !is_uuid(agent_version_id) {
        return Ok(None);
    }
    let data = execute_within(
        DeploymentEnvironments::build(DeploymentEnvironmentsVariables {
            filters: AgentVersionsFilterInput {
                id: Some(TextFilterInput::eq(agent_version_id)),
            },
            version_pagination: one(),
            order_by: EnvironmentDefinitionVersionsOrderInput {
                stable_definition_id: OrderByEnum::Asc,
                version: OrderByEnum::Asc,
                id: OrderByEnum::Asc,
            },
            pagination: page(0),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?;
    Ok(data.agent_versions.nodes.into_iter().next().map(|version| {
        version
            .catalog_releases
            .map(|release| release.environment_definition_versions.nodes)
            .unwrap_or_default()
    }))
}

/// The computed `AgentVersions.deploymentPreview`. Its environment is the generated
/// `EnvironmentDefinitionVersions` row, and every text enum is a `String`, as every other
/// generated text enum is.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentPreview")]
pub struct DeploymentPreviewFields {
    pub environment_definition_version: EnvironmentDefinitionVersion,
    pub strategy: String,
    pub risk: String,
    pub policy_digest: String,
    pub policy_revision: i32,
    pub required_evidence: Vec<String>,
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

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Problem")]
pub struct DeploymentProblemFields {
    pub message: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct DeploymentMutationPayload {
    pub deployment: Option<Deployment>,
    pub problems: Vec<DeploymentProblemFields>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DeploymentPreviewVariables {
    pub filters: AgentVersionsFilterInput,
    pub pagination: PaginationInput,
    pub environment_definition_version_id: String,
    pub strategy: String,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    graphql_type = "AgentVersions",
    variables = "DeploymentPreviewVariables"
)]
pub struct AgentVersionPreview {
    #[arguments(environmentDefinitionVersionId: $environment_definition_version_id, strategy: $strategy)]
    pub deployment_preview: Option<DeploymentPreviewFields>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    graphql_type = "AgentVersionsConnection",
    variables = "DeploymentPreviewVariables"
)]
pub struct AgentVersionPreviewConnection {
    pub nodes: Vec<AgentVersionPreview>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "DeploymentPreviewVariables")]
pub struct DeploymentPreview {
    #[arguments(filters: $filters, pagination: $pagination)]
    pub agent_versions: AgentVersionPreviewConnection,
}

/// The frozen inputs a deployment of this version into this environment would carry. The preview
/// is the computed `deploymentPreview` field on the generated `AgentVersions` row, so the
/// version's own tenant rule decides whether it is reachable at all.
pub async fn request_deployment_preview(
    agent_version_id: &str,
    environment_id: &str,
    strategy: DeploymentStrategy,
) -> Result<Option<DeploymentPreviewFields>, GraphqlError> {
    if !is_uuid(agent_version_id) {
        return Ok(None);
    }
    let variables = DeploymentPreviewVariables {
        filters: AgentVersionsFilterInput {
            id: Some(TextFilterInput::eq(agent_version_id)),
        },
        pagination: one(),
        environment_definition_version_id: environment_id.to_string(),
        strategy: strategy.as_str().to_string(),
    };
    Ok(
        execute_within(DeploymentPreview::build(variables), REQUEST_TIMEOUT_MILLIS)
            .await?
            .agent_versions
            .nodes
            .into_iter()
            .next()
            .and_then(|version| version.deployment_preview),
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

// --- the deployment list -----------------------------------------------------------------------

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "DeploymentsConnection")]
pub struct DeploymentListConnection {
    pub nodes: Vec<DeploymentListItem>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DeploymentsVariables {
    pub scope: ProjectsFilterInput,
    pub filters: DeploymentsFilterInput,
    pub order_by: DeploymentsOrderInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "DeploymentsVariables")]
pub struct Deployments {
    #[arguments(filters: $scope)]
    pub projects: ProjectScopeConnection,
    #[arguments(filters: $filters, orderBy: $order_by, pagination: $pagination)]
    pub deployments: DeploymentListConnection,
}

/// `Ok(None)` is "unavailable": the project is invisible, or the principal holds no
/// `DEPLOYMENT.VIEW` there. `agent_id` narrows the list to one agent.
pub async fn request_deployments(
    project_id: &str,
    agent_id: Option<&str>,
) -> Result<Option<Vec<DeploymentListItem>>, GraphqlError> {
    if !is_uuid(project_id) || agent_id.is_some_and(|agent| !is_uuid(agent)) {
        return Ok(None);
    }
    let data = execute_within(
        Deployments::build(DeploymentsVariables {
            scope: scope(project_id),
            filters: DeploymentsFilterInput {
                project_id: Some(TextFilterInput::eq(project_id)),
                agent_id: agent_id.map(TextFilterInput::eq),
                ..Default::default()
            },
            order_by: newest_first(),
            pagination: page(0),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?;
    Ok(data
        .projects
        .holds(DEPLOYMENT_VIEW)
        .then_some(data.deployments.nodes))
}

// --- one deployment, polled --------------------------------------------------------------------

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "DeploymentsConnection")]
pub struct DeploymentConnection {
    pub nodes: Vec<Deployment>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DeploymentPollingVariables {
    pub filters: DeploymentsFilterInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "DeploymentPollingVariables")]
pub struct DeploymentPolling {
    #[arguments(filters: $filters, pagination: $pagination)]
    pub deployments: DeploymentConnection,
}

/// The deployment and its timeline. `Ok(None)` is "unavailable": the row is invisible or gone.
pub async fn request_deployment(
    deployment_id: &str,
) -> Result<Option<(Deployment, Vec<DeploymentTimelineEvent>)>, GraphqlError> {
    if !is_uuid(deployment_id) {
        return Ok(None);
    }
    let data = execute_within(
        DeploymentPolling::build(DeploymentPollingVariables {
            filters: DeploymentsFilterInput {
                id: Some(TextFilterInput::eq(deployment_id)),
                ..Default::default()
            },
            pagination: one(),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?;
    Ok(data.deployments.nodes.into_iter().next().map(|deployment| {
        let timeline = deployment.timeline.clone();
        (deployment, timeline)
    }))
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
            "deploymentApprovalRequirements",
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

/// The requesting principal that a requirement froze, answered by the requirement's own computed
/// `requester` field.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Principals")]
pub struct Requester {
    pub id: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ProjectApprovalPolicyRule {
    pub required_evidence: Vec<String>,
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
    pub policy_revision: i32,
    pub environment_class: String,
    pub risk: String,
    pub rule: ProjectApprovalPolicyRule,
    pub target: ApprovalTargetSnapshot,
    pub evidence: Vec<DeploymentEvidenceSnapshot>,
}

/// One recorded decision, read through the generated `deploymentApprovalDecisions` relation. The
/// review text is the requirement's normalized four-code vocabulary, not free text.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentApprovalDecisions")]
pub struct ApprovalDecision {
    pub id: String,
    pub decision: String,
    pub comment: Option<String>,
    pub rejection_reason: Option<String>,
    pub decided_at: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentApprovalDecisionsConnection")]
pub struct ApprovalDecisionConnection {
    pub nodes: Vec<ApprovalDecision>,
}

/// One approval requirement: the inbox row and the detail page read the same generated object. Its
/// deployment is the `deployments` relation, and its decision history is the first page of the
/// `deploymentApprovalDecisions` relation.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DeploymentApprovalRequirements")]
pub struct ApprovalInboxItem {
    pub id: String,
    pub revision: i32,
    pub status: String,
    pub expires_at: String,
    pub decision_available: bool,
    pub required_approvers: i32,
    pub qualifying_approval_count: i32,
    pub requester: Option<Requester>,
    pub approval_snapshot: DeploymentApprovalSnapshot,
    // Oldest decision first, the key as the final tie-break.
    #[arguments(orderBy: { decidedAt: ASC, id: ASC }, pagination: { page: { limit: 20, page: 0 } })]
    pub deployment_approval_decisions: ApprovalDecisionConnection,
    pub deployments: Option<Deployment>,
}

impl ApprovalInboxItem {
    pub fn requester_id(&self) -> String {
        self.requester
            .as_ref()
            .map(|requester| requester.id.clone())
            .unwrap_or_default()
    }

    pub fn decisions(&self) -> Vec<ApprovalDecision> {
        self.deployment_approval_decisions.nodes.clone()
    }
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "DeploymentApprovalRequirementsConnection")]
pub struct ApprovalInboxConnection {
    pub nodes: Vec<ApprovalInboxItem>,
    pub page_info: GeneratedPageInfo,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct DeploymentApprovalRequirementsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<TextFilterInput>,
}

/// `filters` is never `null`: Seaography's generated query field reads the argument with
/// `.object()`, so a nullable filter variable bound to `null` (or left unbound) fails the request
/// with "internal: not an object". The unscoped inbox sends an empty filter object instead.
#[derive(cynic::QueryVariables, Debug)]
pub struct ApprovalInboxVariables {
    pub filters: DeploymentApprovalRequirementsFilterInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ApprovalInboxVariables")]
pub struct ApprovalInbox {
    // Newest request first, the key as the final tie-break, so requirements recorded in one
    // instant keep one order across pages.
    #[arguments(filters: $filters, orderBy: { requestedAt: DESC, id: DESC }, pagination: $pagination)]
    pub deployment_approval_requirements: ApprovalInboxConnection,
}

/// One page of the approval inbox. `Ok(None)` is "unavailable": the console decides that from the
/// principal's own `DEPLOYMENT_APPROVAL.VIEW` capability, since an unauthorized generated list is
/// an empty connection, not a refusal.
pub async fn request_approval_inbox(
    organization_id: Option<&str>,
    page_number: i32,
) -> Result<Option<Page<ApprovalInboxItem>>, GraphqlError> {
    if let Some(id) = organization_id {
        if !is_uuid(id) {
            return Ok(None);
        }
    }
    let filters = DeploymentApprovalRequirementsFilterInput {
        organization_id: organization_id.map(TextFilterInput::eq),
        ..Default::default()
    };
    let variables = ApprovalInboxVariables {
        filters,
        pagination: page(page_number),
    };
    let found = execute_within(ApprovalInbox::build(variables), REQUEST_TIMEOUT_MILLIS).await;
    Ok(
        approval_result(found.map(|data| Some(data.deployment_approval_requirements)))?.map(
            |connection| Page::from_page_info(connection.nodes, page_number, connection.page_info),
        ),
    )
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ApprovalRequirementVariables {
    pub filters: DeploymentApprovalRequirementsFilterInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ApprovalRequirementVariables")]
pub struct ApprovalRequirement {
    #[arguments(filters: $filters, orderBy: { requestedAt: DESC, id: DESC }, pagination: $pagination)]
    pub deployment_approval_requirements: ApprovalInboxConnection,
}

pub async fn request_approval_requirement(
    id: &str,
) -> Result<Option<ApprovalInboxItem>, GraphqlError> {
    if !is_uuid(id) {
        return Ok(None);
    }
    let variables = ApprovalRequirementVariables {
        filters: DeploymentApprovalRequirementsFilterInput {
            id: Some(TextFilterInput::eq(id)),
            ..Default::default()
        },
        pagination: one(),
    };
    let found = execute_within(
        ApprovalRequirement::build(variables),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await;
    approval_result(found.map(|data| {
        data.deployment_approval_requirements
            .nodes
            .into_iter()
            .next()
    }))
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
#[cynic(graphql_type = "Problem")]
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
