//! Ports the draft operations of `agent-draft.graphql` and `agentDraftApi.ts`. The root structs keep
//! the React operation names: cynic names an operation after its root struct, and the end-to-end
//! checks intercept requests by that name.

use crate::api::generated::{
    is_uuid, AgentVersionsFilterInput, AgentsFilterInput, PageInput, PaginationInput,
    TextFilterInput,
};
use crate::graphql::{execute, schema, GeneratedJson, GraphqlError};
use cynic::{MutationBuilder, QueryBuilder};
use serde_json::{Map, Value};

pub type DraftDocument = Map<String, Value>;

/// One server-produced validation result, in `AgentDrafts.validationDiagnostics` (a JSON column)
/// and in `AgentDraftReview.diagnostics`.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AgentDraftDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub path: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentVersions")]
pub struct VersionNumber {
    pub version_number: i32,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentVersionsConnection")]
pub struct NewestVersion {
    pub nodes: Vec<VersionNumber>,
}

/// The agent a draft belongs to, with its newest published version.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Agents")]
pub struct DraftAgent {
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    #[arguments(orderBy: { versionNumber: DESC }, pagination: { page: { limit: 1, page: 0 } })]
    pub agent_versions: NewestVersion,
}

/// A generated `AgentDrafts` row with its computed `canUpdate` / `canPublish`. The draft query
/// and every command payload select this same fragment.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentDrafts")]
pub struct AgentDraftFields {
    pub agent_id: String,
    pub document: GeneratedJson,
    pub revision: i32,
    pub validation_status: String,
    pub validation_diagnostics: GeneratedJson,
    pub validated_at: Option<String>,
    pub can_update: bool,
    pub can_publish: bool,
    pub agents: Option<DraftAgent>,
}

impl AgentDraftFields {
    pub fn document(&self) -> DraftDocument {
        self.document.0.as_object().cloned().unwrap_or_default()
    }

    pub fn diagnostics(&self) -> Vec<AgentDraftDiagnostic> {
        serde_json::from_value(self.validation_diagnostics.0.clone()).unwrap_or_default()
    }

    pub fn slug(&self) -> String {
        self.agents
            .as_ref()
            .map(|agent| agent.slug.clone())
            .unwrap_or_default()
    }

    pub fn display_name(&self) -> String {
        self.agents
            .as_ref()
            .map(|agent| agent.display_name.clone())
            .unwrap_or_default()
    }

    pub fn latest_version(&self) -> Option<i32> {
        let newest = self.agents.as_ref()?.agent_versions.nodes.first()?;
        Some(newest.version_number)
    }
}

/// The one problem type every command payload lists its refusals with.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Problem")]
pub struct AgentDraftProblemFields {
    pub code: String,
    pub message: String,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct AgentDraftVariables {
    pub agent: AgentsFilterInput,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Agents")]
pub struct AgentWithDraft {
    /// The stored draft, or the default draft an agent starts from.
    pub draft: AgentDraftFields,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "AgentsConnection")]
pub struct AgentsWithDraft {
    pub nodes: Vec<AgentWithDraft>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AgentDraftVariables")]
pub struct AgentDraft {
    /// Empty when the agent is not visible to the principal in this project.
    #[arguments(filters: $agent)]
    pub agents: AgentsWithDraft,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdateAgentDraftInput {
    pub project_id: cynic::Id,
    pub agent_id: cynic::Id,
    pub expected_revision: i32,
    pub document: Value,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct ValidateAgentDraftInput {
    pub project_id: cynic::Id,
    pub agent_id: cynic::Id,
    pub expected_revision: i32,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentDraftMutationPayload")]
pub struct AgentDraftMutation {
    pub agent_draft: Option<AgentDraftFields>,
    pub problems: Vec<AgentDraftProblemFields>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct UpdateAgentDraftVariables {
    pub input: UpdateAgentDraftInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Mutation", variables = "UpdateAgentDraftVariables")]
pub struct UpdateAgentDraft {
    #[arguments(input: $input)]
    pub update_agent_draft: AgentDraftMutation,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ValidateAgentDraftVariables {
    pub input: ValidateAgentDraftInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Mutation", variables = "ValidateAgentDraftVariables")]
pub struct ValidateAgentDraft {
    #[arguments(input: $input)]
    pub validate_agent_draft: AgentDraftMutation,
}

/// The version a publication left behind; the page only follows its id.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentVersions")]
pub struct PublishedVersion {
    pub id: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentDraftMutationPayload")]
pub struct AgentDraftPublication {
    pub agent_version: Option<PublishedVersion>,
    pub problems: Vec<AgentDraftProblemFields>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct PublishAgentDraftInput {
    pub project_id: cynic::Id,
    pub agent_id: cynic::Id,
    pub expected_revision: i32,
    pub warnings_acknowledged: bool,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct PublishAgentDraftVariables {
    pub input: PublishAgentDraftInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Mutation", variables = "PublishAgentDraftVariables")]
pub struct PublishAgentDraft {
    #[arguments(input: $input)]
    pub publish_agent_draft: AgentDraftPublication,
}

/// The computed `AgentDrafts.review`: what publishing the draft would record.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentDraftReview")]
pub struct AgentDraftReviewFields {
    pub content_digest: String,
    pub dependencies: Vec<String>,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub changed_sections: Vec<String>,
    pub diagnostics: Vec<AgentDraftDiagnostic>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "AgentDrafts")]
pub struct DraftReview {
    pub review: AgentDraftReviewFields,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Agents")]
pub struct AgentWithReview {
    pub draft: DraftReview,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "AgentsConnection")]
pub struct AgentsWithReview {
    pub nodes: Vec<AgentWithReview>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AgentDraftVariables")]
pub struct AgentDraftReview {
    #[arguments(filters: $agent)]
    pub agents: AgentsWithReview,
}

/// `None` when either id is not written as a UUID: Seaography answers a malformed id filter with
/// an error, and such an agent is simply unavailable.
fn agent_in_project(project_id: &str, agent_id: &str) -> Option<AgentsFilterInput> {
    (is_uuid(project_id) && is_uuid(agent_id)).then(|| AgentsFilterInput {
        id: Some(TextFilterInput::eq(agent_id)),
        project_id: Some(TextFilterInput::eq(project_id)),
        ..Default::default()
    })
}

pub async fn request_agent_draft_review(
    project_id: &str,
    agent_id: &str,
) -> Result<Option<AgentDraftReviewFields>, GraphqlError> {
    let Some(agent) = agent_in_project(project_id, agent_id) else {
        return Ok(None);
    };
    let data = execute(AgentDraftReview::build(AgentDraftVariables { agent })).await?;
    Ok(data
        .agents
        .nodes
        .into_iter()
        .next()
        .map(|agent| agent.draft.review))
}

pub async fn publish_agent_draft(
    project_id: &str,
    agent_id: &str,
    expected_revision: i32,
    warnings_acknowledged: bool,
) -> Result<AgentDraftPublication, GraphqlError> {
    let input = PublishAgentDraftInput {
        project_id: project_id.into(),
        agent_id: agent_id.into(),
        expected_revision,
        warnings_acknowledged,
    };
    Ok(
        execute(PublishAgentDraft::build(PublishAgentDraftVariables {
            input,
        }))
        .await?
        .publish_agent_draft,
    )
}

/// `Ok(None)` is the server's "unavailable": no such agent, or none this principal may see.
pub async fn request_agent_draft(
    project_id: &str,
    agent_id: &str,
) -> Result<Option<AgentDraftFields>, GraphqlError> {
    let Some(agent) = agent_in_project(project_id, agent_id) else {
        return Ok(None);
    };
    let data = execute(AgentDraft::build(AgentDraftVariables { agent })).await?;
    Ok(data
        .agents
        .nodes
        .into_iter()
        .next()
        .map(|agent| agent.draft))
}

pub async fn save_agent_draft(
    project_id: &str,
    agent_id: &str,
    expected_revision: i32,
    document: &DraftDocument,
) -> Result<AgentDraftMutation, GraphqlError> {
    let input = UpdateAgentDraftInput {
        project_id: project_id.into(),
        agent_id: agent_id.into(),
        expected_revision,
        document: Value::Object(document.clone()),
    };
    Ok(
        execute(UpdateAgentDraft::build(UpdateAgentDraftVariables { input }))
            .await?
            .update_agent_draft,
    )
}

pub async fn validate_agent_draft(
    project_id: &str,
    agent_id: &str,
    expected_revision: i32,
) -> Result<AgentDraftMutation, GraphqlError> {
    let input = ValidateAgentDraftInput {
        project_id: project_id.into(),
        agent_id: agent_id.into(),
        expected_revision,
    };
    Ok(
        execute(ValidateAgentDraft::build(ValidateAgentDraftVariables {
            input,
        }))
        .await?
        .validate_agent_draft,
    )
}

/// Ports `objectAt`: a section's object, or an empty one when the document holds anything else there.
pub fn object_at(document: &DraftDocument, section: &str) -> DraftDocument {
    document
        .get(section)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

pub fn text_at(object: &DraftDocument, field: &str) -> String {
    object
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_keep_the_names_the_end_to_end_checks_intercept() {
        let query = AgentDraft::build(AgentDraftVariables {
            agent: AgentsFilterInput::default(),
        });
        assert!(
            query.query.starts_with("query AgentDraft("),
            "{}",
            query.query
        );
        let input = ValidateAgentDraftInput {
            project_id: "p".into(),
            agent_id: "a".into(),
            expected_revision: 1,
        };
        let mutation = ValidateAgentDraft::build(ValidateAgentDraftVariables { input });
        assert!(
            mutation.query.starts_with("mutation ValidateAgentDraft("),
            "{}",
            mutation.query
        );
        assert_eq!(
            mutation.operation_name.as_deref(),
            Some("ValidateAgentDraft")
        );
    }
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct CreateAgentDraftInput {
    pub project_id: cynic::Id,
    pub display_name: String,
    pub slug: Option<String>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct CreateAgentDraftVariables {
    pub input: CreateAgentDraftInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Mutation", variables = "CreateAgentDraftVariables")]
pub struct CreateAgentDraft {
    #[arguments(input: $input)]
    pub create_agent_draft: AgentDraftMutation,
}

pub async fn create_agent_draft(
    project_id: &str,
    display_name: &str,
    slug: &str,
) -> Result<AgentDraftMutation, GraphqlError> {
    let input = CreateAgentDraftInput {
        project_id: project_id.into(),
        display_name: display_name.to_string(),
        slug: (!slug.is_empty()).then(|| slug.to_string()),
    };
    Ok(
        execute(CreateAgentDraft::build(CreateAgentDraftVariables { input }))
            .await?
            .create_agent_draft,
    )
}

/// The agent a generated `AgentVersions` row belongs to (Seaography's relation field).
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Agents")]
pub struct VersionAgent {
    pub project_id: String,
    pub slug: String,
    pub display_name: String,
}

/// One row of Seaography's generated `agentVersions` field.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentVersions")]
pub struct AgentVersionFields {
    pub id: String,
    pub agent_id: String,
    pub version_number: i32,
    pub canonical_document: GeneratedJson,
    pub content_digest: String,
    pub dependency_versions: GeneratedJson,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub published_by: String,
    pub published_at: String,
    pub agents: Option<VersionAgent>,
}

impl AgentVersionFields {
    /// `false` when the row's agent is not in `project_id`: the route names both, and a version
    /// reached through another project's URL is "unavailable", not shown.
    fn in_project(&self, project_id: &str) -> bool {
        self.agents
            .as_ref()
            .is_some_and(|agent| agent.project_id == project_id)
    }

    pub fn display_name(&self) -> String {
        self.agents
            .as_ref()
            .map(|agent| agent.display_name.clone())
            .unwrap_or_default()
    }

    /// The exact dependency versions the publication recorded.
    pub fn dependencies(&self) -> Vec<String> {
        serde_json::from_value(self.dependency_versions.0.clone()).unwrap_or_default()
    }
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "AgentVersionsConnection")]
pub struct AgentVersionRows {
    pub nodes: Vec<AgentVersionFields>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Agents")]
pub struct VisibleAgent {
    pub id: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "AgentsConnection")]
pub struct VisibleAgents {
    pub nodes: Vec<VisibleAgent>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct AgentVersionsVariables {
    pub agent: AgentsFilterInput,
    pub filters: AgentVersionsFilterInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AgentVersionsVariables")]
pub struct AgentVersions {
    /// Empty when the agent is not visible to the principal in this project.
    #[arguments(filters: $agent)]
    pub agents: VisibleAgents,
    #[arguments(filters: $filters, orderBy: { versionNumber: DESC, id: ASC }, pagination: $pagination)]
    pub agent_versions: AgentVersionRows,
}

fn one_version(agent_id: &str, version_id: &str) -> AgentVersionsFilterInput {
    AgentVersionsFilterInput {
        id: Some(TextFilterInput::eq(version_id)),
        agent_id: Some(TextFilterInput::eq(agent_id)),
    }
}

/// Newest first. `Ok(None)` is "unavailable": the agent is not visible in this project.
pub async fn request_agent_versions(
    project_id: &str,
    agent_id: &str,
) -> Result<Option<Vec<AgentVersionFields>>, GraphqlError> {
    let Some(agent) = agent_in_project(project_id, agent_id) else {
        return Ok(None);
    };
    let variables = AgentVersionsVariables {
        agent,
        filters: AgentVersionsFilterInput {
            agent_id: Some(TextFilterInput::eq(agent_id)),
            ..Default::default()
        },
        pagination: PaginationInput::Page(PageInput {
            limit: 200,
            page: 0,
        }),
    };
    let data = execute(AgentVersions::build(variables)).await?;
    if !data.agents.nodes.iter().any(|agent| agent.id == agent_id) {
        return Ok(None);
    }
    Ok(Some(
        data.agent_versions
            .nodes
            .into_iter()
            .filter(|row| row.in_project(project_id))
            .collect(),
    ))
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AgentVersionsVariables")]
pub struct AgentVersion {
    /// Empty when the agent is not visible to the principal in this project.
    #[arguments(filters: $agent)]
    pub agents: VisibleAgents,
    #[arguments(filters: $filters, orderBy: { versionNumber: DESC, id: ASC }, pagination: $pagination)]
    pub agent_versions: AgentVersionRows,
}

pub async fn request_agent_version(
    project_id: &str,
    agent_id: &str,
    version_id: &str,
) -> Result<Option<AgentVersionFields>, GraphqlError> {
    let Some(agent) = agent_in_project(project_id, agent_id).filter(|_| is_uuid(version_id)) else {
        return Ok(None);
    };
    let variables = AgentVersionsVariables {
        agent,
        filters: one_version(agent_id, version_id),
        pagination: PaginationInput::Page(PageInput { limit: 1, page: 0 }),
    };
    let data = execute(AgentVersion::build(variables)).await?;
    if !data.agents.nodes.iter().any(|agent| agent.id == agent_id) {
        return Ok(None);
    }
    Ok(data
        .agent_versions
        .nodes
        .into_iter()
        .find(|row| row.in_project(project_id)))
}

/// The computed `AgentVersions.comparison`: the older version and what differs from it.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentVersionComparison")]
pub struct VersionChanges {
    pub from: AgentVersionFields,
    pub changed_sections: Vec<String>,
}

/// The newer version of a comparison.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(
    graphql_type = "AgentVersions",
    variables = "CompareAgentVersionsVariables"
)]
pub struct ComparedVersion {
    pub id: String,
    pub version_number: i32,
    pub canonical_document: GeneratedJson,
    /// `None` when the older version is not a version of the same agent.
    #[arguments(fromVersionId: $from_version_id)]
    pub comparison: Option<VersionChanges>,
    pub agents: Option<VersionAgent>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "AgentVersionsConnection",
    variables = "CompareAgentVersionsVariables"
)]
pub struct ComparedVersions {
    pub nodes: Vec<ComparedVersion>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct CompareAgentVersionsVariables {
    pub agent: AgentsFilterInput,
    pub to: AgentVersionsFilterInput,
    pub from_version_id: String,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "CompareAgentVersionsVariables")]
pub struct CompareAgentVersions {
    /// Empty when the agent is not visible to the principal in this project.
    #[arguments(filters: $agent)]
    pub agents: VisibleAgents,
    #[arguments(filters: $to)]
    pub agent_versions: ComparedVersions,
}

/// `Ok(None)` is "unavailable": the agent is not visible in this project, or the two ids are not
/// both versions of it.
pub async fn request_agent_version_comparison(
    project_id: &str,
    agent_id: &str,
    from: &str,
    to: &str,
) -> Result<Option<ComparedVersion>, GraphqlError> {
    let Some(agent) =
        agent_in_project(project_id, agent_id).filter(|_| is_uuid(from) && is_uuid(to))
    else {
        return Ok(None);
    };
    let variables = CompareAgentVersionsVariables {
        agent,
        to: one_version(agent_id, to),
        from_version_id: from.to_string(),
    };
    let data = execute(CompareAgentVersions::build(variables)).await?;
    if !data.agents.nodes.iter().any(|agent| agent.id == agent_id) {
        return Ok(None);
    }
    Ok(data
        .agent_versions
        .nodes
        .into_iter()
        .find(|version| version.comparison.is_some()))
}
