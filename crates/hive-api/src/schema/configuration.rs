//! The seven configuration commands. The catalog, reusable resources with their drafts and
//! versions, and MCP servers are generated reads (`catalogProjectionHeads`, `catalogReleases`,
//! `catalogDefinitions`, `catalogEnvironments`, `reusableResources`, `reusableResourceDrafts`,
//! `reusableResourceVersions`, `projectToolConnections`) with the computed fields in
//! `hive_persistence::configuration::computed`.
//!
//! A payload returns the stored rows themselves: `reusable_resources::Model` and
//! `project_tool_connections::Model` are Seaography output types as they are, resolving to the
//! generated `ReusableResources` and `ProjectToolConnections` objects with their relations and
//! computed fields. `tool` is the same row as `mcpServer`: `project_tool_connections` stores both
//! the MCP server descriptor and the legacy tool metadata.

use crate::schema::problem::Problem;
use crate::schema::scalars::Id;
use crate::schema::RequestPrincipal;
use hive_application::configuration::{
    ConfigurationMutationResult as AppMutationResult, ConfigurationProblem as AppProblem,
    ConfigurationProblemKind as AppProblemKind, ConfigurationService,
};
use hive_persistence::configuration::PgConfigurationRepository;
use hive_persistence::entity::{project_tool_connections, reusable_resources};
use seaography::{CustomFields, CustomInputType, CustomOutputType};
use uuid::Uuid;

#[allow(non_snake_case)]
mod wire {
    use super::*;

    impl From<AppProblem> for Problem {
        fn from(problem: AppProblem) -> Self {
            match problem.kind {
                AppProblemKind::NotFound => {
                    Problem::new("NOT_FOUND", "This configuration resource is unavailable.")
                }
                AppProblemKind::Forbidden => {
                    Problem::new("FORBIDDEN", "This configuration resource is unavailable.")
                }
                AppProblemKind::InvalidInput => Problem::new(
                    "INVALID_INPUT",
                    "The submitted configuration values are not supported.",
                ),
                AppProblemKind::InvalidDraft => Problem::new(
                    "INVALID_DRAFT",
                    "Choose an approved definition available in this environment.",
                ),
                AppProblemKind::ProtectedLifecycle => Problem::new(
                    "PROTECTED_LIFECYCLE",
                    "This configuration resource is not active.",
                ),
                AppProblemKind::RevisionConflict => Problem::revision_conflict(
                    "This configuration changed after you opened it.",
                    problem.resource_id.map(|id| id.to_string()),
                    problem.expected_revision,
                    problem.actual_revision,
                ),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ConfigurationMutationPayload {
        pub resource: Option<reusable_resources::Model>,
        pub mcpServer: Option<project_tool_connections::Model>,
        /// The same row as `mcpServer`, for a client that works with legacy tool metadata.
        pub tool: Option<project_tool_connections::Model>,
        pub problems: Vec<Problem>,
    }

    impl From<AppMutationResult<reusable_resources::Model, project_tool_connections::Model>>
        for ConfigurationMutationPayload
    {
        fn from(
            result: AppMutationResult<reusable_resources::Model, project_tool_connections::Model>,
        ) -> Self {
            Self {
                resource: result.resource,
                tool: result.mcp_server.clone(),
                mcpServer: result.mcp_server,
                problems: result.problem.into_iter().map(Problem::from).collect(),
            }
        }
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "CreateReusableResourceInput")]
    pub struct CreateReusableResourceInput {
        pub projectId: Id,
        pub kind: String,
        pub name: String,
        pub content: String,
        pub dependencies: Vec<String>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "UpdateReusableResourceDraftInput")]
    pub struct UpdateReusableResourceDraftInput {
        pub projectId: Id,
        pub resourceId: Id,
        pub expectedRevision: i32,
        pub content: String,
        pub dependencies: Vec<String>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "ReusableResourceRevisionInput")]
    pub struct ReusableResourceRevisionInput {
        pub projectId: Id,
        pub resourceId: Id,
        pub expectedRevision: i32,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "CreateProjectMcpServerInput")]
    pub struct CreateProjectMcpServerInput {
        pub projectId: Id,
        pub serverId: String,
        pub name: String,
        pub definition: String,
        pub environment: String,
        pub enabled: bool,
        pub transportType: String,
        pub command: Option<String>,
        pub arguments: Vec<String>,
        pub remoteUrl: Option<String>,
        pub redactedBindings: Vec<String>,
        pub tools: Vec<String>,
        pub resources: Vec<String>,
        pub prompts: Vec<String>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "UpdateProjectMcpServerInput")]
    pub struct UpdateProjectMcpServerInput {
        pub projectId: Id,
        pub id: Id,
        pub expectedRevision: i32,
        pub name: String,
        pub definition: String,
        pub environment: String,
        pub enabled: bool,
        pub transportType: String,
        pub command: Option<String>,
        pub arguments: Vec<String>,
        pub remoteUrl: Option<String>,
        pub redactedBindings: Vec<String>,
        pub tools: Vec<String>,
        pub resources: Vec<String>,
        pub prompts: Vec<String>,
        pub lifecycleStatus: String,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "SaveProjectToolConnectionMetadataInput")]
    pub struct SaveProjectToolConnectionMetadataInput {
        pub projectId: Id,
        pub toolId: Option<Id>,
        pub expectedRevision: i32,
        pub name: String,
        pub definition: String,
        pub environment: String,
        pub redactedSecretReference: String,
        pub lifecycleStatus: String,
        pub rotationSummary: String,
    }

    fn configuration_service(
        ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<ConfigurationService<PgConfigurationRepository>> {
        let repository =
            PgConfigurationRepository::new(ctx.data::<sea_orm::DatabaseConnection>()?.clone());
        Ok(ConfigurationService::new(repository))
    }

    fn principal(ctx: &async_graphql::Context<'_>) -> async_graphql::Result<Uuid> {
        Ok(ctx.data::<RequestPrincipal>()?.0)
    }

    pub struct ConfigurationMutations;

    #[CustomFields]
    impl ConfigurationMutations {
        async fn createReusableResource(
            ctx: &async_graphql::Context<'_>,
            input: CreateReusableResourceInput,
        ) -> async_graphql::Result<ConfigurationMutationPayload> {
            let result = configuration_service(ctx)?
                .create(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.kind,
                    &input.name,
                    &input.content,
                    input.dependencies,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(ConfigurationMutationPayload::from(result))
        }

        async fn updateReusableResourceDraft(
            ctx: &async_graphql::Context<'_>,
            input: UpdateReusableResourceDraftInput,
        ) -> async_graphql::Result<ConfigurationMutationPayload> {
            let result = configuration_service(ctx)?
                .update(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.resourceId.0,
                    input.expectedRevision as i64,
                    &input.content,
                    input.dependencies,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(ConfigurationMutationPayload::from(result))
        }

        async fn validateReusableResource(
            ctx: &async_graphql::Context<'_>,
            input: ReusableResourceRevisionInput,
        ) -> async_graphql::Result<ConfigurationMutationPayload> {
            let result = configuration_service(ctx)?
                .validate(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.resourceId.0,
                    input.expectedRevision as i64,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(ConfigurationMutationPayload::from(result))
        }

        async fn publishReusableResource(
            ctx: &async_graphql::Context<'_>,
            input: ReusableResourceRevisionInput,
        ) -> async_graphql::Result<ConfigurationMutationPayload> {
            let result = configuration_service(ctx)?
                .publish(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.resourceId.0,
                    input.expectedRevision as i64,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(ConfigurationMutationPayload::from(result))
        }

        async fn createProjectMcpServer(
            ctx: &async_graphql::Context<'_>,
            input: CreateProjectMcpServerInput,
        ) -> async_graphql::Result<ConfigurationMutationPayload> {
            let result = configuration_service(ctx)?
                .create_mcp_server(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.serverId,
                    &input.name,
                    &input.definition,
                    &input.environment,
                    input.enabled,
                    &input.transportType,
                    input.command.as_deref(),
                    input.arguments,
                    input.remoteUrl.as_deref(),
                    input.redactedBindings,
                    input.tools,
                    input.resources,
                    input.prompts,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(ConfigurationMutationPayload::from(result))
        }

        async fn updateProjectMcpServer(
            ctx: &async_graphql::Context<'_>,
            input: UpdateProjectMcpServerInput,
        ) -> async_graphql::Result<ConfigurationMutationPayload> {
            let result = configuration_service(ctx)?
                .update_mcp_server(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.id.0,
                    input.expectedRevision as i64,
                    &input.name,
                    &input.definition,
                    &input.environment,
                    input.enabled,
                    &input.transportType,
                    input.command.as_deref(),
                    input.arguments,
                    input.remoteUrl.as_deref(),
                    input.redactedBindings,
                    input.tools,
                    input.resources,
                    input.prompts,
                    &input.lifecycleStatus,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(ConfigurationMutationPayload::from(result))
        }

        async fn saveProjectToolConnectionMetadata(
            ctx: &async_graphql::Context<'_>,
            input: SaveProjectToolConnectionMetadataInput,
        ) -> async_graphql::Result<ConfigurationMutationPayload> {
            let result = configuration_service(ctx)?
                .save_legacy_tool(
                    principal(ctx)?,
                    &input.projectId.0,
                    input.toolId.as_ref().map(|id| id.0.as_str()),
                    input.expectedRevision as i64,
                    &input.name,
                    &input.definition,
                    &input.environment,
                    &input.redactedSecretReference,
                    &input.lifecycleStatus,
                    &input.rotationSummary,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(ConfigurationMutationPayload::from(result))
        }
    }
}

pub use wire::{
    ConfigurationMutationPayload, ConfigurationMutations, CreateProjectMcpServerInput,
    CreateReusableResourceInput, ReusableResourceRevisionInput,
    SaveProjectToolConnectionMetadataInput, UpdateProjectMcpServerInput,
    UpdateReusableResourceDraftInput,
};

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_mutation::<ConfigurationMutations>();
    builder.register_custom_output::<ConfigurationMutationPayload>();
    builder.register_custom_input::<CreateReusableResourceInput>();
    builder.register_custom_input::<UpdateReusableResourceDraftInput>();
    builder.register_custom_input::<ReusableResourceRevisionInput>();
    builder.register_custom_input::<CreateProjectMcpServerInput>();
    builder.register_custom_input::<UpdateProjectMcpServerInput>();
    builder.register_custom_input::<SaveProjectToolConnectionMetadataInput>();
}
