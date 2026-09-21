//! The eight evaluation commands. Every payload carries the definition or run the command wrote
//! and the refusals it listed, so one fragment covers all of them.

use super::*;
use cynic::MutationBuilder;

/// The one problem type every command payload lists its refusals with.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Problem")]
pub struct EvaluationProblemFields {
    pub code: String,
    pub message: String,
}

/// The generated `EvaluationDefinitions` row a command answers with, and its draft.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDefinitions")]
pub struct CommandDefinitionFields {
    pub id: String,
    pub project_id: String,
    pub draft: CommandDraftFields,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDefinitionDrafts")]
pub struct CommandDraftFields {
    pub revision: i32,
    pub validation_status: String,
    pub canonical_document: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationRuns")]
pub struct CommandRunFields {
    pub id: String,
    pub project_id: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EvaluationMutationPayload {
    pub definition: Option<CommandDefinitionFields>,
    pub run: Option<CommandRunFields>,
    pub problems: Vec<EvaluationProblemFields>,
}

impl EvaluationMutationPayload {
    /// The first refusal, if the command was refused.
    pub fn problem(&self) -> Option<String> {
        self.problems.first().map(|problem| problem.message.clone())
    }
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct CreateEvaluationDefinitionInput {
    pub project_id: cynic::Id,
    pub slug: String,
    pub document: Option<String>,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdateEvaluationDefinitionDraftInput {
    pub definition_id: cynic::Id,
    pub expected_revision: i64,
    pub document: String,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct ValidateEvaluationDefinitionDraftInput {
    pub definition_id: cynic::Id,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct PublishEvaluationDefinitionDraftInput {
    pub definition_id: cynic::Id,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct DuplicateEvaluationDefinitionVersionToDraftInput {
    pub version_id: cynic::Id,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct RunEvaluationInput {
    pub project_id: cynic::Id,
    pub definition_version_id: cynic::Id,
    pub target_kind: EvaluationTargetKind,
    pub target_id: cynic::Id,
    pub environment_definition_version_id: cynic::Id,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct CancelEvaluationInput {
    pub run_id: cynic::Id,
    pub expected_generation: i64,
    pub idempotency_key: String,
    pub reason: Option<String>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct RerunEvaluationInput {
    pub run_id: cynic::Id,
    pub idempotency_key: String,
}

macro_rules! evaluation_mutation {
    ($d:tt, $root:ident, $variables:ident, $variables_name:literal, $input_type:ty, $field:ident) => {
        #[derive(cynic::QueryVariables, Debug)]
        pub struct $variables {
            pub input: $input_type,
        }

        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "Mutation", variables = $variables_name)]
        pub struct $root {
            #[arguments(input: $d input)]
            pub $field: EvaluationMutationPayload,
        }

        pub async fn $field(input: $input_type) -> Result<EvaluationMutationPayload, GraphqlError> {
            Ok(
                execute_within($root::build($variables { input }), REQUEST_TIMEOUT_MILLIS)
                    .await?
                    .$field,
            )
        }
    };
}

evaluation_mutation!($, CreateEvaluationDefinition, CreateEvaluationDefinitionVariables, "CreateEvaluationDefinitionVariables", CreateEvaluationDefinitionInput, create_evaluation_definition);
evaluation_mutation!($, UpdateEvaluationDefinitionDraft, UpdateEvaluationDefinitionDraftVariables, "UpdateEvaluationDefinitionDraftVariables", UpdateEvaluationDefinitionDraftInput, update_evaluation_definition_draft);
evaluation_mutation!($, ValidateEvaluationDefinitionDraft, ValidateEvaluationDefinitionDraftVariables, "ValidateEvaluationDefinitionDraftVariables", ValidateEvaluationDefinitionDraftInput, validate_evaluation_definition_draft);
evaluation_mutation!($, PublishEvaluationDefinitionDraft, PublishEvaluationDefinitionDraftVariables, "PublishEvaluationDefinitionDraftVariables", PublishEvaluationDefinitionDraftInput, publish_evaluation_definition_draft);
evaluation_mutation!($, DuplicateEvaluationDefinitionVersionToDraft, DuplicateEvaluationDefinitionVersionToDraftVariables, "DuplicateEvaluationDefinitionVersionToDraftVariables", DuplicateEvaluationDefinitionVersionToDraftInput, duplicate_evaluation_definition_version_to_draft);
evaluation_mutation!($, RunEvaluation, RunEvaluationVariables, "RunEvaluationVariables", RunEvaluationInput, run_evaluation);
evaluation_mutation!($, CancelEvaluation, CancelEvaluationVariables, "CancelEvaluationVariables", CancelEvaluationInput, cancel_evaluation);
evaluation_mutation!($, RerunEvaluation, RerunEvaluationVariables, "RerunEvaluationVariables", RerunEvaluationInput, rerun_evaluation);
