//! The four agent draft commands. Drafts, versions, the publication review and the version
//! comparison are generated reads: `agentDrafts`, `agentVersions`, and the computed fields in
//! `hive_persistence::agent::computed` (`Agents.draft`, `AgentDrafts.canUpdate` / `canPublish` /
//! `review`, `AgentVersions.comparison`).
//!
//! A payload returns the stored rows themselves. `agent_drafts::Model` and `agent_versions::Model`
//! are Seaography output types as they are (`seaography::GqlModelHolderType` for `Option<Model>`),
//! resolving to the generated `AgentDrafts` and `AgentVersions` objects with their relations and
//! computed fields.

use crate::schema::problem::Problem;
use crate::schema::scalars::{Id, Json};
use crate::schema::{repository_failure, services, RequestPrincipal};
use hive_application::agent::{
    AgentDraftMutationProblem as AppProblem, AgentDraftMutationResult as AppMutationResult,
    AgentDraftProblemKind as AppProblemKind,
};
use hive_persistence::agent::{AgentDraftDiagnostic, AgentDraftReview, AgentVersionComparison};
use hive_persistence::entity::{agent_drafts, agent_versions};
use seaography::{CustomFields, CustomInputType, CustomOutputType};
use uuid::Uuid;

#[allow(non_snake_case)]
mod wire {
    use super::*;

    impl From<AppProblem> for Problem {
        fn from(problem: AppProblem) -> Self {
            match problem.kind {
                AppProblemKind::NotFound => Problem::new("NOT_FOUND", "This agent is unavailable."),
                AppProblemKind::Forbidden => Problem::new(
                    "FORBIDDEN",
                    "You do not have permission to edit this draft.",
                ),
                AppProblemKind::RevisionConflict => Problem::revision_conflict(
                    "This draft changed after you opened it.",
                    problem.resource_id.map(|id| id.to_string()),
                    problem.expected_revision,
                    problem.actual_revision,
                ),
                AppProblemKind::InvalidDocument => {
                    Problem::new("INVALID_DOCUMENT", "The draft document must be an object.")
                }
                AppProblemKind::InvalidDraft => Problem::new(
                    "INVALID_DRAFT",
                    "Resolve the server validation errors before publication.",
                ),
                AppProblemKind::WarningAcknowledgementRequired => Problem::new(
                    "WARNING_ACKNOWLEDGEMENT_REQUIRED",
                    "Review and acknowledge the server warnings before publication.",
                ),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentDraftMutationPayload {
        pub agentDraft: Option<agent_drafts::Model>,
        pub agentVersion: Option<agent_versions::Model>,
        pub problems: Vec<Problem>,
    }

    impl From<AppMutationResult<agent_drafts::Model, agent_versions::Model>>
        for AgentDraftMutationPayload
    {
        fn from(result: AppMutationResult<agent_drafts::Model, agent_versions::Model>) -> Self {
            Self {
                agentDraft: result.agent_draft,
                agentVersion: result.agent_version,
                problems: result.problem.into_iter().map(Problem::from).collect(),
            }
        }
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "CreateAgentDraftInput")]
    pub struct CreateAgentDraftInput {
        pub projectId: Id,
        pub displayName: String,
        pub slug: Option<String>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "UpdateAgentDraftInput")]
    pub struct UpdateAgentDraftInput {
        pub projectId: Id,
        pub agentId: Id,
        pub expectedRevision: i32,
        pub document: Json,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "ValidateAgentDraftInput")]
    pub struct ValidateAgentDraftInput {
        pub projectId: Id,
        pub agentId: Id,
        pub expectedRevision: i32,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "PublishAgentDraftInput")]
    pub struct PublishAgentDraftInput {
        pub projectId: Id,
        pub agentId: Id,
        pub expectedRevision: i32,
        pub warningsAcknowledged: bool,
    }

    fn principal(ctx: &async_graphql::Context<'_>) -> async_graphql::Result<Uuid> {
        Ok(ctx.data::<RequestPrincipal>()?.0)
    }

    pub struct AgentMutations;

    #[CustomFields]
    impl AgentMutations {
        async fn createAgentDraft(
            ctx: &async_graphql::Context<'_>,
            input: CreateAgentDraftInput,
        ) -> async_graphql::Result<AgentDraftMutationPayload> {
            let result = services(ctx)?
                .agent_draft
                .create_draft(
                    principal(ctx)?,
                    &input.projectId.0,
                    input.displayName,
                    input.slug,
                )
                .await
                .map_err(repository_failure)?;
            Ok(AgentDraftMutationPayload::from(result))
        }

        async fn updateAgentDraft(
            ctx: &async_graphql::Context<'_>,
            input: UpdateAgentDraftInput,
        ) -> async_graphql::Result<AgentDraftMutationPayload> {
            let document = serde_json::to_string(&input.document.0)
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            let result = services(ctx)?
                .agent_draft
                .update_draft(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.agentId.0,
                    input.expectedRevision as i64,
                    document,
                )
                .await
                .map_err(repository_failure)?;
            Ok(AgentDraftMutationPayload::from(result))
        }

        async fn validateAgentDraft(
            ctx: &async_graphql::Context<'_>,
            input: ValidateAgentDraftInput,
        ) -> async_graphql::Result<AgentDraftMutationPayload> {
            let result = services(ctx)?
                .agent_draft
                .validate_draft(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.agentId.0,
                    input.expectedRevision as i64,
                )
                .await
                .map_err(repository_failure)?;
            Ok(AgentDraftMutationPayload::from(result))
        }

        async fn publishAgentDraft(
            ctx: &async_graphql::Context<'_>,
            input: PublishAgentDraftInput,
        ) -> async_graphql::Result<AgentDraftMutationPayload> {
            let result = services(ctx)?
                .agent_draft
                .publish_draft(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.agentId.0,
                    input.expectedRevision as i64,
                    input.warningsAcknowledged,
                )
                .await
                .map_err(repository_failure)?;
            Ok(AgentDraftMutationPayload::from(result))
        }
    }
}

pub use wire::{
    AgentDraftMutationPayload, AgentMutations, CreateAgentDraftInput, PublishAgentDraftInput,
    UpdateAgentDraftInput, ValidateAgentDraftInput,
};

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_mutation::<AgentMutations>();
    builder.register_custom_output::<AgentDraftDiagnostic>();
    builder.register_custom_output::<AgentDraftReview>();
    builder.register_custom_output::<AgentVersionComparison>();
    builder.register_custom_output::<AgentDraftMutationPayload>();
    builder.register_custom_input::<CreateAgentDraftInput>();
    builder.register_custom_input::<UpdateAgentDraftInput>();
    builder.register_custom_input::<ValidateAgentDraftInput>();
    builder.register_custom_input::<PublishAgentDraftInput>();
}
