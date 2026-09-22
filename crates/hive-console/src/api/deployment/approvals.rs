//! The approval inbox, one approval requirement and the decision command, read through the
//! generated `deploymentApprovalRequirements` and `deploymentApprovalDecisions` entity queries.
//!
//! A service that predates approvals rejects these documents at validation; that is reported as
//! "unavailable" rather than as a failed request, so a console built against a newer schema still
//! runs against it.

use super::*;

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
