//! Ports the draft operations of `agent-draft.graphql` and `agentDraftApi.ts`. The root structs keep
//! the React operation names: cynic names an operation after its root struct, and the end-to-end
//! checks intercept requests by that name (`LFP-PARITY`).

use crate::api::generated::{
    AgentVersionsFilterInput, AgentsFilterInput, PageInput, PaginationInput, TextFilterInput,
};
use crate::graphql::{execute, schema, GeneratedJson, GraphqlError};
use cynic::{MutationBuilder, QueryBuilder};
use serde_json::{Map, Value};

pub type DraftDocument = Map<String, Value>;

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AgentDraftDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub path: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentDraft")]
pub struct AgentDraftFields {
    pub id: cynic::Id,
    pub agent_id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub document: Value,
    pub revision: i32,
    pub validation_status: String,
    pub validated_at: Option<String>,
    pub can_update: bool,
    pub can_publish: bool,
    pub latest_version: Option<i32>,
    pub validation_diagnostics: Vec<AgentDraftDiagnostic>,
}

impl AgentDraftFields {
    pub fn document(&self) -> DraftDocument {
        self.document.as_object().cloned().unwrap_or_default()
    }
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentDraftProblem")]
pub struct AgentDraftProblemFields {
    pub code: String,
    pub message: String,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct AgentDraftVariables {
    pub project_id: cynic::Id,
    pub agent_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AgentDraftVariables")]
pub struct AgentDraft {
    #[arguments(projectId: $project_id, agentId: $agent_id)]
    pub agent_draft: Option<AgentDraftFields>,
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

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentVersion")]
pub struct AgentVersionFields {
    pub id: cynic::Id,
    pub agent_id: cynic::Id,
    pub number: i32,
    pub slug: String,
    pub display_name: String,
    pub canonical_document: Value,
    pub content_digest: String,
    pub dependencies: Vec<String>,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub published_by: cynic::Id,
    pub published_at: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentDraftMutationPayload")]
pub struct AgentDraftPublication {
    pub agent_version: Option<AgentVersionFields>,
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

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AgentDraftVariables")]
pub struct AgentDraftReview {
    #[arguments(projectId: $project_id, agentId: $agent_id)]
    pub agent_draft_review: Option<AgentDraftReviewFields>,
}

pub async fn request_agent_draft_review(
    project_id: &str,
    agent_id: &str,
) -> Result<Option<AgentDraftReviewFields>, GraphqlError> {
    let variables = AgentDraftVariables {
        project_id: project_id.into(),
        agent_id: agent_id.into(),
    };
    Ok(execute(AgentDraftReview::build(variables))
        .await?
        .agent_draft_review)
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
    let variables = AgentDraftVariables {
        project_id: project_id.into(),
        agent_id: agent_id.into(),
    };
    Ok(execute(AgentDraft::build(variables)).await?.agent_draft)
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
            project_id: "p".into(),
            agent_id: "a".into(),
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
pub struct AgentVersionRow {
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

impl AgentVersionRow {
    /// `None` when the row's agent is not in `project_id`: the route names both, and a version
    /// reached through another project's URL is "unavailable", not shown.
    fn into_fields(self, project_id: &str) -> Option<AgentVersionFields> {
        let agent = self.agents.filter(|agent| agent.project_id == project_id)?;
        let dependencies = self
            .dependency_versions
            .0
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Some(AgentVersionFields {
            id: self.id.into(),
            agent_id: self.agent_id.into(),
            number: self.version_number,
            slug: agent.slug,
            display_name: agent.display_name,
            canonical_document: self.canonical_document.0,
            content_digest: self.content_digest,
            dependencies,
            catalog_release_id: self.catalog_release_id,
            catalog_release_digest: self.catalog_release_digest,
            published_by: self.published_by.into(),
            published_at: self.published_at,
        })
    }
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "AgentVersionsConnection")]
pub struct AgentVersionRows {
    pub nodes: Vec<AgentVersionRow>,
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
    #[arguments(filters: $filters, orderBy: { versionNumber: DESC }, pagination: $pagination)]
    pub agent_versions: AgentVersionRows,
}

fn agent_in_project(project_id: &str, agent_id: &str) -> AgentsFilterInput {
    AgentsFilterInput {
        id: Some(TextFilterInput::eq(agent_id)),
        project_id: Some(TextFilterInput::eq(project_id)),
        ..Default::default()
    }
}

/// Newest first. `Ok(None)` is "unavailable": the agent is not visible in this project.
pub async fn request_agent_versions(
    project_id: &str,
    agent_id: &str,
) -> Result<Option<Vec<AgentVersionFields>>, GraphqlError> {
    let variables = AgentVersionsVariables {
        agent: agent_in_project(project_id, agent_id),
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
            .filter_map(|row| row.into_fields(project_id))
            .collect(),
    ))
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AgentVersionsVariables")]
pub struct AgentVersion {
    /// Empty when the agent is not visible to the principal in this project.
    #[arguments(filters: $agent)]
    pub agents: VisibleAgents,
    #[arguments(filters: $filters, orderBy: { versionNumber: DESC }, pagination: $pagination)]
    pub agent_versions: AgentVersionRows,
}

pub async fn request_agent_version(
    project_id: &str,
    agent_id: &str,
    version_id: &str,
) -> Result<Option<AgentVersionFields>, GraphqlError> {
    let variables = AgentVersionsVariables {
        agent: agent_in_project(project_id, agent_id),
        filters: AgentVersionsFilterInput {
            id: Some(TextFilterInput::eq(version_id)),
            agent_id: Some(TextFilterInput::eq(agent_id)),
        },
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
        .find_map(|row| row.into_fields(project_id)))
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AgentVersionComparison {
    pub from: AgentVersionFields,
    pub to: AgentVersionFields,
    pub changed_sections: Vec<String>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct CompareAgentVersionsVariables {
    pub project_id: cynic::Id,
    pub agent_id: cynic::Id,
    pub from_version_id: cynic::Id,
    pub to_version_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "CompareAgentVersionsVariables")]
pub struct CompareAgentVersions {
    #[arguments(projectId: $project_id, agentId: $agent_id, fromVersionId: $from_version_id, toVersionId: $to_version_id)]
    pub compare_agent_versions: Option<AgentVersionComparison>,
}

pub async fn request_agent_version_comparison(
    project_id: &str,
    agent_id: &str,
    from: &str,
    to: &str,
) -> Result<Option<AgentVersionComparison>, GraphqlError> {
    let variables = CompareAgentVersionsVariables {
        project_id: project_id.into(),
        agent_id: agent_id.into(),
        from_version_id: from.into(),
        to_version_id: to.into(),
    };
    Ok(execute(CompareAgentVersions::build(variables))
        .await?
        .compare_agent_versions)
}
