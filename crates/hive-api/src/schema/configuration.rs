//! Ports `ConfigurationGraphql`: 5 read fields, 7 mutations. No field or argument here carries a
//! default, so every root operation is a plain `#[CustomFields]` method, like `agent.rs`.
//!
//! Fourth interface this port builds (`ConfigurationProblem`, 6 implementors) — same pattern as
//! `console.rs`'s `DisplayPreferencesProblem`/`agent.rs`'s `AgentDraftProblem`: each container-enum
//! variant is named identically to its inner type (`agent.rs`'s doc comment has the full trail on
//! why, and why `#[allow(clippy::enum_variant_names)]` is the correct call here too).
//!
//! Many fields/inputs here are `Vec<String>` (`availableEnvironments`, `environments`,
//! `dependencies`, `diagnostics`, `dependentResources`, `arguments`, `redactedBindings`, `tools`,
//! `resources`, `prompts`) — all `scalars::StringList`, including on the *input* side
//! (`CreateProjectMcpServerInput.arguments` etc.), which is why `StringList` grew a
//! `CustomInputType` impl alongside this file (`scalars.rs`'s own doc comment has the trail: the
//! same SeaORM-blanket ambiguity that breaks a `Vec<String>` output field breaks a `Vec<String>`
//! input field too, since `GqlScalarValueType::gql_input_type_ref` shares the same default body).
//!
//! `projectToolConnections` and the mutation payload's `tool` field are both an
//! `McpServerConfiguration` row reshaped through the same `legacyTool()` mapping Java uses:
//! `project_tool_connections` backs both the MCP-server and legacy-tool console views, so a
//! `ProjectToolConnection` is never a separately stored entity, and `tool` is populated whenever
//! `mcpServer` is, for every mutation that produces one (not only `saveLegacyTool`'s).

use crate::schema::scalars::{Id, StringList};
use crate::schema::RequestPrincipal;
use hive_application::configuration::{
    CatalogDefinition as AppCatalogDefinition, CatalogRelease as AppCatalogRelease,
    ConfigurationMutationResult as AppMutationResult, ConfigurationProblem as AppProblem,
    ConfigurationProblemKind as AppProblemKind, ConfigurationService,
    McpServerConfiguration as AppMcpServer, ResourceVersion as AppResourceVersion,
    ReusableResource as AppReusableResource,
};
use hive_persistence::configuration::PgConfigurationRepository;
use seaography::{
    BuilderContext, CustomFields, CustomInputType, CustomOutputObject, CustomOutputType,
};
use uuid::Uuid;

pub const CONFIGURATION_PROBLEM_INTERFACE: &str = "ConfigurationProblem";

#[allow(non_snake_case)]
mod wire {
    use super::*;

    #[derive(CustomOutputType, Clone)]
    pub struct CatalogDefinition {
        pub identity: String,
        pub version: String,
        pub kind: String,
        pub displayName: String,
        pub contentDigest: String,
        pub availableEnvironments: StringList,
    }

    impl From<AppCatalogDefinition> for CatalogDefinition {
        fn from(value: AppCatalogDefinition) -> Self {
            Self {
                identity: value.identity,
                version: value.version,
                kind: value.kind,
                displayName: value.display_name,
                contentDigest: value.content_digest,
                availableEnvironments: value.available_environments.into(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct CatalogRelease {
        pub id: Id,
        pub source: String,
        pub sourceDigest: String,
        pub releasedAt: String,
        pub definitions: Vec<CatalogDefinition>,
        pub environments: StringList,
    }

    impl From<AppCatalogRelease> for CatalogRelease {
        fn from(value: AppCatalogRelease) -> Self {
            Self {
                id: value.id.into(),
                source: value.source,
                sourceDigest: value.source_digest,
                releasedAt: value.released_at,
                definitions: value
                    .definitions
                    .into_iter()
                    .map(CatalogDefinition::from)
                    .collect(),
                environments: value.environments.into(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ReusableResourceVersion {
        pub version: i32,
        pub contentDigest: String,
        pub canonicalDocument: String,
        pub dependencies: StringList,
        pub publishedAt: String,
        pub publishedBy: Id,
    }

    impl From<AppResourceVersion> for ReusableResourceVersion {
        fn from(value: AppResourceVersion) -> Self {
            Self {
                version: value.version as i32,
                contentDigest: value.content_digest,
                canonicalDocument: value.canonical_document,
                dependencies: value.dependencies.into(),
                publishedAt: hive_domain::java_offset_date_time_string(value.published_at),
                publishedBy: value.published_by.into(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ReusableResource {
        pub id: Id,
        pub projectId: Id,
        pub kind: String,
        pub name: String,
        pub identity: String,
        pub draftRevision: i32,
        pub draftContent: String,
        pub draftDigest: String,
        pub draftDependencies: StringList,
        pub validationStatus: String,
        pub diagnostics: StringList,
        pub publishedVersion: Option<i32>,
        pub versions: Vec<ReusableResourceVersion>,
        pub dependentResources: StringList,
        pub lifecycleStatus: String,
    }

    impl From<AppReusableResource> for ReusableResource {
        fn from(value: AppReusableResource) -> Self {
            Self {
                id: value.id.to_string().into(),
                projectId: value.project_id.to_string().into(),
                kind: value.kind,
                name: value.name,
                identity: value.identity,
                draftRevision: value.draft_revision as i32,
                draftContent: value.draft_content,
                draftDigest: value.draft_digest,
                draftDependencies: value.draft_dependencies.into(),
                validationStatus: value.validation_status,
                diagnostics: value.diagnostics.into(),
                publishedVersion: value.published_version.map(|version| version as i32),
                versions: value
                    .versions
                    .into_iter()
                    .map(ReusableResourceVersion::from)
                    .collect(),
                dependentResources: value.dependent_resources.into(),
                lifecycleStatus: value.lifecycle_status,
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct McpServerConfiguration {
        pub id: Id,
        pub projectId: Id,
        pub serverId: String,
        pub name: String,
        pub definitionIdentity: String,
        pub definitionVersion: String,
        pub environment: String,
        pub enabled: bool,
        pub transportType: Option<String>,
        pub command: Option<String>,
        pub arguments: StringList,
        pub remoteUrl: Option<String>,
        pub redactedBindings: StringList,
        pub tools: StringList,
        pub resources: StringList,
        pub prompts: StringList,
        pub lifecycleStatus: String,
        pub status: String,
        pub revision: i32,
        pub dependentResources: StringList,
    }

    impl From<AppMcpServer> for McpServerConfiguration {
        fn from(value: AppMcpServer) -> Self {
            Self {
                id: value.id.to_string().into(),
                projectId: value.project_id.to_string().into(),
                serverId: value.server_id,
                name: value.name,
                definitionIdentity: value.definition_identity,
                definitionVersion: value.definition_version,
                environment: value.environment,
                enabled: value.enabled,
                transportType: value.transport_type,
                command: value.command,
                arguments: value.arguments.into(),
                remoteUrl: value.remote_url,
                redactedBindings: value.redacted_bindings.into(),
                tools: value.tools.into(),
                resources: value.resources.into(),
                prompts: value.prompts.into(),
                lifecycleStatus: value.lifecycle_status,
                status: value.status,
                revision: value.revision as i32,
                dependentResources: value.dependent_resources.into(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ProjectToolConnection {
        pub id: Id,
        pub projectId: Id,
        pub name: String,
        pub definitionIdentity: String,
        pub definitionVersion: String,
        pub environment: String,
        pub redactedSecretReference: String,
        pub lifecycleStatus: String,
        pub rotationSummary: String,
        pub revision: i32,
        pub dependentResources: StringList,
    }

    /// Ports the private `legacyTool()` mapping. `rotationSummary` is always this fixed sentence
    /// here — the stored `rotation_summary` value only round-trips through
    /// `saveProjectToolConnectionMetadata`, never through this read.
    impl From<&McpServerConfiguration> for ProjectToolConnection {
        fn from(value: &McpServerConfiguration) -> Self {
            Self {
                id: value.id.clone(),
                projectId: value.projectId.clone(),
                name: value.name.clone(),
                definitionIdentity: value.definitionIdentity.clone(),
                definitionVersion: value.definitionVersion.clone(),
                environment: value.environment.clone(),
                redactedSecretReference: value
                    .redactedBindings
                    .0
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "redacted://local/unbound".to_string()),
                lifecycleStatus: value.lifecycleStatus.clone(),
                rotationSummary:
                    "Legacy metadata is available through the inert MCP server editor.".to_string(),
                revision: value.revision,
                dependentResources: value.dependentResources.clone(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ConfigurationNotFoundProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ConfigurationAuthorizationProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ConfigurationValidationProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ConfigurationInvalidDraftProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ConfigurationLifecycleProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ConfigurationRevisionConflict {
        pub code: String,
        pub message: String,
        pub resourceId: Id,
        pub expectedRevision: i32,
        pub actualRevision: i32,
    }

    // clippy::enum_variant_names is a false positive here — see `agent.rs`'s
    // `AgentDraftProblem` for the full rationale: each variant's identifier is the exact GraphQL
    // type name interface resolution depends on, not a discretionary choice.
    #[derive(CustomOutputType, Clone)]
    #[allow(clippy::enum_variant_names)]
    pub enum ConfigurationProblem {
        ConfigurationNotFoundProblem(ConfigurationNotFoundProblem),
        ConfigurationAuthorizationProblem(ConfigurationAuthorizationProblem),
        ConfigurationValidationProblem(ConfigurationValidationProblem),
        ConfigurationInvalidDraftProblem(ConfigurationInvalidDraftProblem),
        ConfigurationLifecycleProblem(ConfigurationLifecycleProblem),
        ConfigurationRevisionConflict(ConfigurationRevisionConflict),
    }

    impl From<AppProblem> for ConfigurationProblem {
        fn from(problem: AppProblem) -> Self {
            match problem.kind {
                AppProblemKind::NotFound => ConfigurationProblem::ConfigurationNotFoundProblem(
                    ConfigurationNotFoundProblem {
                        code: "NOT_FOUND".to_string(),
                        message: "This configuration resource is unavailable.".to_string(),
                    },
                ),
                AppProblemKind::Forbidden => {
                    ConfigurationProblem::ConfigurationAuthorizationProblem(
                        ConfigurationAuthorizationProblem {
                            code: "FORBIDDEN".to_string(),
                            message: "This configuration resource is unavailable.".to_string(),
                        },
                    )
                }
                AppProblemKind::InvalidInput => {
                    ConfigurationProblem::ConfigurationValidationProblem(
                        ConfigurationValidationProblem {
                            code: "INVALID_INPUT".to_string(),
                            message: "The submitted configuration values are not supported."
                                .to_string(),
                        },
                    )
                }
                AppProblemKind::InvalidDraft => {
                    ConfigurationProblem::ConfigurationInvalidDraftProblem(
                        ConfigurationInvalidDraftProblem {
                            code: "INVALID_DRAFT".to_string(),
                            message: "Choose an approved definition available in this environment."
                                .to_string(),
                        },
                    )
                }
                AppProblemKind::ProtectedLifecycle => {
                    ConfigurationProblem::ConfigurationLifecycleProblem(
                        ConfigurationLifecycleProblem {
                            code: "PROTECTED_LIFECYCLE".to_string(),
                            message: "This configuration resource is not active.".to_string(),
                        },
                    )
                }
                AppProblemKind::RevisionConflict => {
                    ConfigurationProblem::ConfigurationRevisionConflict(
                        ConfigurationRevisionConflict {
                            code: "REVISION_CONFLICT".to_string(),
                            message: "This configuration changed after you opened it.".to_string(),
                            resourceId: problem
                                .resource_id
                                .map(|id| id.to_string())
                                .unwrap_or_default()
                                .into(),
                            expectedRevision: problem.expected_revision as i32,
                            actualRevision: problem.actual_revision as i32,
                        },
                    )
                }
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ConfigurationMutationPayload {
        pub resource: Option<ReusableResource>,
        pub mcpServer: Option<McpServerConfiguration>,
        pub tool: Option<ProjectToolConnection>,
        pub problems: Vec<ConfigurationProblem>,
    }

    impl From<AppMutationResult> for ConfigurationMutationPayload {
        fn from(result: AppMutationResult) -> Self {
            let mcp_server = result.mcp_server.map(McpServerConfiguration::from);
            let tool = mcp_server.as_ref().map(ProjectToolConnection::from);
            Self {
                resource: result.resource.map(ReusableResource::from),
                mcpServer: mcp_server,
                tool,
                problems: result
                    .problem
                    .into_iter()
                    .map(ConfigurationProblem::from)
                    .collect(),
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
        pub dependencies: StringList,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "UpdateReusableResourceDraftInput")]
    pub struct UpdateReusableResourceDraftInput {
        pub projectId: Id,
        pub resourceId: Id,
        pub expectedRevision: i32,
        pub content: String,
        pub dependencies: StringList,
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
        pub arguments: StringList,
        pub remoteUrl: Option<String>,
        pub redactedBindings: StringList,
        pub tools: StringList,
        pub resources: StringList,
        pub prompts: StringList,
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
        pub arguments: StringList,
        pub remoteUrl: Option<String>,
        pub redactedBindings: StringList,
        pub tools: StringList,
        pub resources: StringList,
        pub prompts: StringList,
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

    pub struct ConfigurationQueries;

    #[CustomFields]
    impl ConfigurationQueries {
        // Ports `ConfigurationGraphql.Resolver.catalog`.
        async fn catalogRelease(
            ctx: &async_graphql::Context<'_>,
            organizationId: Id,
        ) -> async_graphql::Result<Option<CatalogRelease>> {
            let release = configuration_service(ctx)?
                .catalog(principal(ctx)?, &organizationId.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(release.map(CatalogRelease::from))
        }

        // Ports `ConfigurationGraphql.Resolver.resources`.
        async fn reusableResources(
            ctx: &async_graphql::Context<'_>,
            projectId: Id,
            kind: Option<String>,
        ) -> async_graphql::Result<Option<Vec<ReusableResource>>> {
            let resources = configuration_service(ctx)?
                .resources(principal(ctx)?, &projectId.0, kind.as_deref())
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(resources.map(|values| values.into_iter().map(ReusableResource::from).collect()))
        }

        // Ports `ConfigurationGraphql.Resolver.resource`.
        async fn reusableResource(
            ctx: &async_graphql::Context<'_>,
            projectId: Id,
            resourceId: Id,
        ) -> async_graphql::Result<Option<ReusableResource>> {
            let resource = configuration_service(ctx)?
                .resource(principal(ctx)?, &projectId.0, &resourceId.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(resource.map(ReusableResource::from))
        }

        // Ports `ConfigurationGraphql.Resolver.mcpServers`.
        async fn projectMcpServers(
            ctx: &async_graphql::Context<'_>,
            projectId: Id,
        ) -> async_graphql::Result<Option<Vec<McpServerConfiguration>>> {
            let servers = configuration_service(ctx)?
                .mcp_servers(principal(ctx)?, &projectId.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(servers.map(|values| {
                values
                    .into_iter()
                    .map(McpServerConfiguration::from)
                    .collect()
            }))
        }

        // Ports `ConfigurationGraphql.Resolver.legacyTools`.
        async fn projectToolConnections(
            ctx: &async_graphql::Context<'_>,
            projectId: Id,
        ) -> async_graphql::Result<Option<Vec<ProjectToolConnection>>> {
            let servers = configuration_service(ctx)?
                .mcp_servers(principal(ctx)?, &projectId.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(servers.map(|values| {
                values
                    .into_iter()
                    .map(McpServerConfiguration::from)
                    .map(|server| ProjectToolConnection::from(&server))
                    .collect()
            }))
        }
    }

    pub struct ConfigurationMutations;

    #[CustomFields]
    impl ConfigurationMutations {
        // Ports `ConfigurationGraphql.Resolver.create`.
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
                    input.dependencies.0,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(ConfigurationMutationPayload::from(result))
        }

        // Ports `ConfigurationGraphql.Resolver.update`.
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
                    input.dependencies.0,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(ConfigurationMutationPayload::from(result))
        }

        // Ports `ConfigurationGraphql.Resolver.validate`.
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

        // Ports `ConfigurationGraphql.Resolver.publish`.
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

        // Ports `ConfigurationGraphql.Resolver.createMcp`.
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
                    input.arguments.0,
                    input.remoteUrl.as_deref(),
                    input.redactedBindings.0,
                    input.tools.0,
                    input.resources.0,
                    input.prompts.0,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(ConfigurationMutationPayload::from(result))
        }

        // Ports `ConfigurationGraphql.Resolver.updateMcp`.
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
                    input.arguments.0,
                    input.remoteUrl.as_deref(),
                    input.redactedBindings.0,
                    input.tools.0,
                    input.resources.0,
                    input.prompts.0,
                    &input.lifecycleStatus,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(ConfigurationMutationPayload::from(result))
        }

        // Ports `ConfigurationGraphql.Resolver.saveLegacyTool`.
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
    CatalogDefinition, CatalogRelease, ConfigurationAuthorizationProblem,
    ConfigurationInvalidDraftProblem, ConfigurationLifecycleProblem, ConfigurationMutationPayload,
    ConfigurationMutations, ConfigurationNotFoundProblem, ConfigurationQueries,
    ConfigurationRevisionConflict, ConfigurationValidationProblem, CreateProjectMcpServerInput,
    CreateReusableResourceInput, McpServerConfiguration, ProjectToolConnection, ReusableResource,
    ReusableResourceRevisionInput, ReusableResourceVersion, SaveProjectToolConnectionMetadataInput,
    UpdateProjectMcpServerInput, UpdateReusableResourceDraftInput,
};

fn context() -> &'static BuilderContext {
    crate::schema::context()
}

/// This module's `Interface`, registered directly on the `SchemaBuilder` in `mod.rs::build()`
/// (`Builder` itself has no interface vector to push onto — same as `console.rs`/`agent.rs`).
pub fn interfaces() -> Vec<async_graphql::dynamic::Interface> {
    use async_graphql::dynamic::{Interface, InterfaceField, TypeRef};
    vec![Interface::new(CONFIGURATION_PROBLEM_INTERFACE)
        .field(InterfaceField::new(
            "code",
            TypeRef::named_nn(TypeRef::STRING),
        ))
        .field(InterfaceField::new(
            "message",
            TypeRef::named_nn(TypeRef::STRING),
        ))]
}

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_query::<ConfigurationQueries>();
    builder.register_custom_mutation::<ConfigurationMutations>();
    builder.register_custom_output::<CatalogDefinition>();
    builder.register_custom_output::<CatalogRelease>();
    builder.register_custom_output::<ReusableResourceVersion>();
    builder.register_custom_output::<ReusableResource>();
    builder.register_custom_output::<McpServerConfiguration>();
    builder.register_custom_output::<ProjectToolConnection>();
    builder.outputs.push(
        ConfigurationNotFoundProblem::basic_object(context())
            .implement(CONFIGURATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        ConfigurationAuthorizationProblem::basic_object(context())
            .implement(CONFIGURATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        ConfigurationValidationProblem::basic_object(context())
            .implement(CONFIGURATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        ConfigurationInvalidDraftProblem::basic_object(context())
            .implement(CONFIGURATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        ConfigurationLifecycleProblem::basic_object(context())
            .implement(CONFIGURATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        ConfigurationRevisionConflict::basic_object(context())
            .implement(CONFIGURATION_PROBLEM_INTERFACE),
    );
    builder.register_custom_output::<ConfigurationMutationPayload>();
    builder.register_custom_input::<CreateReusableResourceInput>();
    builder.register_custom_input::<UpdateReusableResourceDraftInput>();
    builder.register_custom_input::<ReusableResourceRevisionInput>();
    builder.register_custom_input::<CreateProjectMcpServerInput>();
    builder.register_custom_input::<UpdateProjectMcpServerInput>();
    builder.register_custom_input::<SaveProjectToolConnectionMetadataInput>();
}
