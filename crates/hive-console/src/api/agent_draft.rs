//! Ports the draft operations of `agent-draft.graphql` and `agentDraftApi.ts`. The root structs keep
//! the React operation names: cynic names an operation after its root struct, and the end-to-end
//! checks intercept requests by that name (`LFP-PARITY`).

use crate::graphql::{execute, schema, GraphqlError};
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

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AgentDraftVariables")]
pub struct AgentVersions {
    #[arguments(projectId: $project_id, agentId: $agent_id)]
    pub agent_versions: Option<Vec<AgentVersionFields>>,
}

pub async fn request_agent_versions(
    project_id: &str,
    agent_id: &str,
) -> Result<Option<Vec<AgentVersionFields>>, GraphqlError> {
    let variables = AgentDraftVariables {
        project_id: project_id.into(),
        agent_id: agent_id.into(),
    };
    Ok(execute(AgentVersions::build(variables))
        .await?
        .agent_versions)
}

#[derive(cynic::QueryVariables, Debug)]
pub struct AgentVersionVariables {
    pub project_id: cynic::Id,
    pub agent_id: cynic::Id,
    pub version_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AgentVersionVariables")]
pub struct AgentVersion {
    #[arguments(projectId: $project_id, agentId: $agent_id, versionId: $version_id)]
    pub agent_version: Option<AgentVersionFields>,
}

pub async fn request_agent_version(
    project_id: &str,
    agent_id: &str,
    version_id: &str,
) -> Result<Option<AgentVersionFields>, GraphqlError> {
    let variables = AgentVersionVariables {
        project_id: project_id.into(),
        agent_id: agent_id.into(),
        version_id: version_id.into(),
    };
    Ok(execute(AgentVersion::build(variables)).await?.agent_version)
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
