//! Ports `ConfigurationGraphql` and the query/mutation field wiring
//! `GraphqlSchemaFactory` declares for it: 5 read fields (`catalogRelease`,
//! `reusableResources`, `reusableResource`, `projectMcpServers`,
//! `projectToolConnections`) and 7 mutations (`createReusableResource`,
//! `updateReusableResourceDraft`, `validateReusableResource`,
//! `publishReusableResource`, `createProjectMcpServer`,
//! `updateProjectMcpServer`, `saveProjectToolConnectionMetadata`).
//!
//! `projectToolConnections` and the mutation payload's `tool` field are both
//! an `McpServerConfiguration` row reshaped through the same `legacyTool()`
//! mapping Java uses: `project_tool_connections` backs both the MCP-server and
//! legacy-tool console views, so a `ProjectToolConnection` is never a
//! separately stored entity, and `tool` is populated whenever `mcpServer` is,
//! for every mutation that produces one (not only `saveLegacyTool`'s).

use crate::schema::RequestPrincipal;
use async_graphql::{Context, InputObject, Interface, Object, SimpleObject};
use hive_application::configuration::{
    CatalogDefinition as AppCatalogDefinition, CatalogRelease as AppCatalogRelease,
    ConfigurationMutationResult as AppMutationResult, ConfigurationProblem as AppProblem,
    ConfigurationProblemKind as AppProblemKind, ConfigurationService,
    McpServerConfiguration as AppMcpServer, ResourceVersion as AppResourceVersion,
    ReusableResource as AppReusableResource,
};
use hive_persistence::configuration::PgConfigurationRepository;
use uuid::Uuid;

#[derive(SimpleObject)]
pub struct CatalogDefinition {
    pub identity: String,
    pub version: String,
    pub kind: String,
    pub display_name: String,
    pub content_digest: String,
    pub available_environments: Vec<String>,
}

impl From<AppCatalogDefinition> for CatalogDefinition {
    fn from(value: AppCatalogDefinition) -> Self {
        Self {
            identity: value.identity,
            version: value.version,
            kind: value.kind,
            display_name: value.display_name,
            content_digest: value.content_digest,
            available_environments: value.available_environments,
        }
    }
}

#[derive(SimpleObject)]
pub struct CatalogRelease {
    pub id: async_graphql::ID,
    pub source: String,
    pub source_digest: String,
    pub released_at: String,
    pub definitions: Vec<CatalogDefinition>,
    pub environments: Vec<String>,
}

impl From<AppCatalogRelease> for CatalogRelease {
    fn from(value: AppCatalogRelease) -> Self {
        Self {
            id: async_graphql::ID(value.id),
            source: value.source,
            source_digest: value.source_digest,
            released_at: value.released_at,
            definitions: value
                .definitions
                .into_iter()
                .map(CatalogDefinition::from)
                .collect(),
            environments: value.environments,
        }
    }
}

#[derive(SimpleObject)]
pub struct ReusableResourceVersion {
    pub version: i32,
    pub content_digest: String,
    pub canonical_document: String,
    pub dependencies: Vec<String>,
    pub published_at: String,
    pub published_by: async_graphql::ID,
}

impl From<AppResourceVersion> for ReusableResourceVersion {
    fn from(value: AppResourceVersion) -> Self {
        Self {
            version: value.version as i32,
            content_digest: value.content_digest,
            canonical_document: value.canonical_document,
            dependencies: value.dependencies,
            published_at: hive_domain::java_offset_date_time_string(value.published_at),
            published_by: async_graphql::ID(value.published_by),
        }
    }
}

#[derive(SimpleObject)]
pub struct ReusableResource {
    pub id: async_graphql::ID,
    pub project_id: async_graphql::ID,
    pub kind: String,
    pub name: String,
    pub identity: String,
    pub draft_revision: i32,
    pub draft_content: String,
    pub draft_digest: String,
    pub draft_dependencies: Vec<String>,
    pub validation_status: String,
    pub diagnostics: Vec<String>,
    pub published_version: Option<i32>,
    pub versions: Vec<ReusableResourceVersion>,
    pub dependent_resources: Vec<String>,
    pub lifecycle_status: String,
}

impl From<AppReusableResource> for ReusableResource {
    fn from(value: AppReusableResource) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            project_id: async_graphql::ID(value.project_id.to_string()),
            kind: value.kind,
            name: value.name,
            identity: value.identity,
            draft_revision: value.draft_revision as i32,
            draft_content: value.draft_content,
            draft_digest: value.draft_digest,
            draft_dependencies: value.draft_dependencies,
            validation_status: value.validation_status,
            diagnostics: value.diagnostics,
            published_version: value.published_version.map(|version| version as i32),
            versions: value
                .versions
                .into_iter()
                .map(ReusableResourceVersion::from)
                .collect(),
            dependent_resources: value.dependent_resources,
            lifecycle_status: value.lifecycle_status,
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct McpServerConfiguration {
    pub id: async_graphql::ID,
    pub project_id: async_graphql::ID,
    pub server_id: String,
    pub name: String,
    pub definition_identity: String,
    pub definition_version: String,
    pub environment: String,
    pub enabled: bool,
    pub transport_type: Option<String>,
    pub command: Option<String>,
    pub arguments: Vec<String>,
    pub remote_url: Option<String>,
    pub redacted_bindings: Vec<String>,
    pub tools: Vec<String>,
    pub resources: Vec<String>,
    pub prompts: Vec<String>,
    pub lifecycle_status: String,
    pub status: String,
    pub revision: i32,
    pub dependent_resources: Vec<String>,
}

impl From<AppMcpServer> for McpServerConfiguration {
    fn from(value: AppMcpServer) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            project_id: async_graphql::ID(value.project_id.to_string()),
            server_id: value.server_id,
            name: value.name,
            definition_identity: value.definition_identity,
            definition_version: value.definition_version,
            environment: value.environment,
            enabled: value.enabled,
            transport_type: value.transport_type,
            command: value.command,
            arguments: value.arguments,
            remote_url: value.remote_url,
            redacted_bindings: value.redacted_bindings,
            tools: value.tools,
            resources: value.resources,
            prompts: value.prompts,
            lifecycle_status: value.lifecycle_status,
            status: value.status,
            revision: value.revision as i32,
            dependent_resources: value.dependent_resources,
        }
    }
}

#[derive(SimpleObject)]
pub struct ProjectToolConnection {
    pub id: async_graphql::ID,
    pub project_id: async_graphql::ID,
    pub name: String,
    pub definition_identity: String,
    pub definition_version: String,
    pub environment: String,
    pub redacted_secret_reference: String,
    pub lifecycle_status: String,
    pub rotation_summary: String,
    pub revision: i32,
    pub dependent_resources: Vec<String>,
}

/// Ports the private `legacyTool()` mapping. `rotationSummary` is always this
/// fixed sentence here — the stored `rotation_summary` value only round-trips
/// through `saveProjectToolConnectionMetadata`, never through this read.
impl From<&McpServerConfiguration> for ProjectToolConnection {
    fn from(value: &McpServerConfiguration) -> Self {
        Self {
            id: value.id.clone(),
            project_id: value.project_id.clone(),
            name: value.name.clone(),
            definition_identity: value.definition_identity.clone(),
            definition_version: value.definition_version.clone(),
            environment: value.environment.clone(),
            redacted_secret_reference: value
                .redacted_bindings
                .first()
                .cloned()
                .unwrap_or_else(|| "redacted://local/unbound".to_string()),
            lifecycle_status: value.lifecycle_status.clone(),
            rotation_summary: "Legacy metadata is available through the inert MCP server editor."
                .to_string(),
            revision: value.revision,
            dependent_resources: value.dependent_resources.clone(),
        }
    }
}

#[derive(SimpleObject)]
pub struct ConfigurationNotFoundProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct ConfigurationAuthorizationProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct ConfigurationValidationProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct ConfigurationInvalidDraftProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct ConfigurationLifecycleProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct ConfigurationRevisionConflict {
    pub code: String,
    pub message: String,
    pub resource_id: async_graphql::ID,
    pub expected_revision: i32,
    pub actual_revision: i32,
}

/// Ports the `ConfigurationProblem` GraphQL interface: every variant shares
/// exactly `code`/`message`; `ConfigurationRevisionConflict` carries three
/// more fields of its own that a client reaches through an inline fragment.
#[derive(Interface)]
#[allow(clippy::duplicated_attributes)]
#[graphql(field(name = "code", ty = "String"))]
#[graphql(field(name = "message", ty = "String"))]
pub enum ConfigurationProblem {
    NotFound(ConfigurationNotFoundProblem),
    Authorization(ConfigurationAuthorizationProblem),
    Validation(ConfigurationValidationProblem),
    InvalidDraft(ConfigurationInvalidDraftProblem),
    Lifecycle(ConfigurationLifecycleProblem),
    RevisionConflict(ConfigurationRevisionConflict),
}

impl From<AppProblem> for ConfigurationProblem {
    fn from(problem: AppProblem) -> Self {
        match problem.kind {
            AppProblemKind::NotFound => {
                ConfigurationProblem::NotFound(ConfigurationNotFoundProblem {
                    code: "NOT_FOUND".to_string(),
                    message: "This configuration resource is unavailable.".to_string(),
                })
            }
            AppProblemKind::Forbidden => {
                ConfigurationProblem::Authorization(ConfigurationAuthorizationProblem {
                    code: "FORBIDDEN".to_string(),
                    message: "This configuration resource is unavailable.".to_string(),
                })
            }
            AppProblemKind::InvalidInput => {
                ConfigurationProblem::Validation(ConfigurationValidationProblem {
                    code: "INVALID_INPUT".to_string(),
                    message: "The submitted configuration values are not supported.".to_string(),
                })
            }
            AppProblemKind::InvalidDraft => {
                ConfigurationProblem::InvalidDraft(ConfigurationInvalidDraftProblem {
                    code: "INVALID_DRAFT".to_string(),
                    message: "Choose an approved definition available in this environment."
                        .to_string(),
                })
            }
            AppProblemKind::ProtectedLifecycle => {
                ConfigurationProblem::Lifecycle(ConfigurationLifecycleProblem {
                    code: "PROTECTED_LIFECYCLE".to_string(),
                    message: "This configuration resource is not active.".to_string(),
                })
            }
            AppProblemKind::RevisionConflict => {
                ConfigurationProblem::RevisionConflict(ConfigurationRevisionConflict {
                    code: "REVISION_CONFLICT".to_string(),
                    message: "This configuration changed after you opened it.".to_string(),
                    resource_id: async_graphql::ID(
                        problem
                            .resource_id
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                    ),
                    expected_revision: problem.expected_revision as i32,
                    actual_revision: problem.actual_revision as i32,
                })
            }
        }
    }
}

#[derive(SimpleObject)]
pub struct ConfigurationMutationPayload {
    pub resource: Option<ReusableResource>,
    pub mcp_server: Option<McpServerConfiguration>,
    pub tool: Option<ProjectToolConnection>,
    pub problems: Vec<ConfigurationProblem>,
}

impl From<AppMutationResult> for ConfigurationMutationPayload {
    fn from(result: AppMutationResult) -> Self {
        let mcp_server = result.mcp_server.map(McpServerConfiguration::from);
        let tool = mcp_server.as_ref().map(ProjectToolConnection::from);
        Self {
            resource: result.resource.map(ReusableResource::from),
            mcp_server,
            tool,
            problems: result
                .problem
                .into_iter()
                .map(ConfigurationProblem::from)
                .collect(),
        }
    }
}

#[derive(InputObject)]
pub struct CreateReusableResourceInput {
    pub project_id: async_graphql::ID,
    pub kind: String,
    pub name: String,
    pub content: String,
    pub dependencies: Vec<String>,
}

#[derive(InputObject)]
pub struct UpdateReusableResourceDraftInput {
    pub project_id: async_graphql::ID,
    pub resource_id: async_graphql::ID,
    pub expected_revision: i32,
    pub content: String,
    pub dependencies: Vec<String>,
}

#[derive(InputObject)]
pub struct ReusableResourceRevisionInput {
    pub project_id: async_graphql::ID,
    pub resource_id: async_graphql::ID,
    pub expected_revision: i32,
}

#[derive(InputObject)]
pub struct CreateProjectMcpServerInput {
    pub project_id: async_graphql::ID,
    pub server_id: String,
    pub name: String,
    pub definition: String,
    pub environment: String,
    pub enabled: bool,
    pub transport_type: String,
    pub command: Option<String>,
    pub arguments: Vec<String>,
    pub remote_url: Option<String>,
    pub redacted_bindings: Vec<String>,
    pub tools: Vec<String>,
    pub resources: Vec<String>,
    pub prompts: Vec<String>,
}

#[derive(InputObject)]
pub struct UpdateProjectMcpServerInput {
    pub project_id: async_graphql::ID,
    pub id: async_graphql::ID,
    pub expected_revision: i32,
    pub name: String,
    pub definition: String,
    pub environment: String,
    pub enabled: bool,
    pub transport_type: String,
    pub command: Option<String>,
    pub arguments: Vec<String>,
    pub remote_url: Option<String>,
    pub redacted_bindings: Vec<String>,
    pub tools: Vec<String>,
    pub resources: Vec<String>,
    pub prompts: Vec<String>,
    pub lifecycle_status: String,
}

#[derive(InputObject)]
pub struct SaveProjectToolConnectionMetadataInput {
    pub project_id: async_graphql::ID,
    pub tool_id: Option<async_graphql::ID>,
    pub expected_revision: i32,
    pub name: String,
    pub definition: String,
    pub environment: String,
    pub redacted_secret_reference: String,
    pub lifecycle_status: String,
    pub rotation_summary: String,
}

fn configuration_service(
    ctx: &Context<'_>,
) -> async_graphql::Result<ConfigurationService<PgConfigurationRepository>> {
    let repository = PgConfigurationRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
    Ok(ConfigurationService::new(repository))
}

fn principal(ctx: &Context<'_>) -> async_graphql::Result<Uuid> {
    Ok(ctx.data::<RequestPrincipal>()?.0)
}

pub struct ConfigurationQueries;

#[Object]
impl ConfigurationQueries {
    /// Ports `ConfigurationGraphql.Resolver.catalog`.
    async fn catalog_release(
        &self,
        ctx: &Context<'_>,
        organization_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<CatalogRelease>> {
        let release = configuration_service(ctx)?
            .catalog(principal(ctx)?, organization_id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(release.map(CatalogRelease::from))
    }

    /// Ports `ConfigurationGraphql.Resolver.resources`.
    async fn reusable_resources(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        kind: Option<String>,
    ) -> async_graphql::Result<Option<Vec<ReusableResource>>> {
        let resources = configuration_service(ctx)?
            .resources(principal(ctx)?, project_id.as_str(), kind.as_deref())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(resources.map(|values| values.into_iter().map(ReusableResource::from).collect()))
    }

    /// Ports `ConfigurationGraphql.Resolver.resource`.
    async fn reusable_resource(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        resource_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<ReusableResource>> {
        let resource = configuration_service(ctx)?
            .resource(principal(ctx)?, project_id.as_str(), resource_id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(resource.map(ReusableResource::from))
    }

    /// Ports `ConfigurationGraphql.Resolver.mcpServers`.
    async fn project_mcp_servers(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<Vec<McpServerConfiguration>>> {
        let servers = configuration_service(ctx)?
            .mcp_servers(principal(ctx)?, project_id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(servers.map(|values| {
            values
                .into_iter()
                .map(McpServerConfiguration::from)
                .collect()
        }))
    }

    /// Ports `ConfigurationGraphql.Resolver.legacyTools`.
    async fn project_tool_connections(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<Vec<ProjectToolConnection>>> {
        let servers = configuration_service(ctx)?
            .mcp_servers(principal(ctx)?, project_id.as_str())
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

#[Object]
impl ConfigurationMutations {
    /// Ports `ConfigurationGraphql.Resolver.create`.
    async fn create_reusable_resource(
        &self,
        ctx: &Context<'_>,
        input: CreateReusableResourceInput,
    ) -> async_graphql::Result<ConfigurationMutationPayload> {
        let result = configuration_service(ctx)?
            .create(
                principal(ctx)?,
                input.project_id.as_str(),
                &input.kind,
                &input.name,
                &input.content,
                input.dependencies,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(ConfigurationMutationPayload::from(result))
    }

    /// Ports `ConfigurationGraphql.Resolver.update`.
    async fn update_reusable_resource_draft(
        &self,
        ctx: &Context<'_>,
        input: UpdateReusableResourceDraftInput,
    ) -> async_graphql::Result<ConfigurationMutationPayload> {
        let result = configuration_service(ctx)?
            .update(
                principal(ctx)?,
                input.project_id.as_str(),
                input.resource_id.as_str(),
                input.expected_revision as i64,
                &input.content,
                input.dependencies,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(ConfigurationMutationPayload::from(result))
    }

    /// Ports `ConfigurationGraphql.Resolver.validate`.
    async fn validate_reusable_resource(
        &self,
        ctx: &Context<'_>,
        input: ReusableResourceRevisionInput,
    ) -> async_graphql::Result<ConfigurationMutationPayload> {
        let result = configuration_service(ctx)?
            .validate(
                principal(ctx)?,
                input.project_id.as_str(),
                input.resource_id.as_str(),
                input.expected_revision as i64,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(ConfigurationMutationPayload::from(result))
    }

    /// Ports `ConfigurationGraphql.Resolver.publish`.
    async fn publish_reusable_resource(
        &self,
        ctx: &Context<'_>,
        input: ReusableResourceRevisionInput,
    ) -> async_graphql::Result<ConfigurationMutationPayload> {
        let result = configuration_service(ctx)?
            .publish(
                principal(ctx)?,
                input.project_id.as_str(),
                input.resource_id.as_str(),
                input.expected_revision as i64,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(ConfigurationMutationPayload::from(result))
    }

    /// Ports `ConfigurationGraphql.Resolver.createMcp`.
    async fn create_project_mcp_server(
        &self,
        ctx: &Context<'_>,
        input: CreateProjectMcpServerInput,
    ) -> async_graphql::Result<ConfigurationMutationPayload> {
        let result = configuration_service(ctx)?
            .create_mcp_server(
                principal(ctx)?,
                input.project_id.as_str(),
                &input.server_id,
                &input.name,
                &input.definition,
                &input.environment,
                input.enabled,
                &input.transport_type,
                input.command.as_deref(),
                input.arguments,
                input.remote_url.as_deref(),
                input.redacted_bindings,
                input.tools,
                input.resources,
                input.prompts,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(ConfigurationMutationPayload::from(result))
    }

    /// Ports `ConfigurationGraphql.Resolver.updateMcp`.
    async fn update_project_mcp_server(
        &self,
        ctx: &Context<'_>,
        input: UpdateProjectMcpServerInput,
    ) -> async_graphql::Result<ConfigurationMutationPayload> {
        let result = configuration_service(ctx)?
            .update_mcp_server(
                principal(ctx)?,
                input.project_id.as_str(),
                input.id.as_str(),
                input.expected_revision as i64,
                &input.name,
                &input.definition,
                &input.environment,
                input.enabled,
                &input.transport_type,
                input.command.as_deref(),
                input.arguments,
                input.remote_url.as_deref(),
                input.redacted_bindings,
                input.tools,
                input.resources,
                input.prompts,
                &input.lifecycle_status,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(ConfigurationMutationPayload::from(result))
    }

    /// Ports `ConfigurationGraphql.Resolver.saveLegacyTool`.
    async fn save_project_tool_connection_metadata(
        &self,
        ctx: &Context<'_>,
        input: SaveProjectToolConnectionMetadataInput,
    ) -> async_graphql::Result<ConfigurationMutationPayload> {
        let result = configuration_service(ctx)?
            .save_legacy_tool(
                principal(ctx)?,
                input.project_id.as_str(),
                input.tool_id.as_ref().map(|id| id.as_str()),
                input.expected_revision as i64,
                &input.name,
                &input.definition,
                &input.environment,
                &input.redacted_secret_reference,
                &input.lifecycle_status,
                &input.rotation_summary,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(ConfigurationMutationPayload::from(result))
    }
}
