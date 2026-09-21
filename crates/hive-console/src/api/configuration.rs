//! The catalog, reusable resources and MCP servers, read through the generated API, and the six
//! configuration commands. cynic names a GraphQL operation after its root
//! struct, and the end-to-end checks intercept requests by that name, so renaming a root struct
//! here breaks those checks.
//!
//! Project rows are read through `projects`, so a project the principal cannot see answers with no
//! node ("unavailable"), which is not the same as a project with no rows. The catalog has no owner,
//! so the catalog query reads the organization next to it for the same reason.

use crate::api::generated::{
    is_uuid, OrderByEnum, OrganizationsFilterInput, ProjectsFilterInput, StringFilterInput,
    TextFilterInput,
};
use crate::graphql::{execute, schema, GeneratedJson, GraphqlError};
use cynic::{MutationBuilder, QueryBuilder};

/// A JSON column that stores a list of strings.
fn strings(value: &GeneratedJson) -> Vec<String> {
    serde_json::from_value(value.0.clone()).unwrap_or_default()
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "CatalogDefinitions")]
pub struct CatalogDefinition {
    pub identity: String,
    pub version: String,
    pub definition_kind: String,
    pub display_name: String,
    pub content_digest: String,
    pub available_environments: GeneratedJson,
}

impl CatalogDefinition {
    pub fn available_environments(&self) -> Vec<String> {
        strings(&self.available_environments)
    }
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "CatalogDefinitionsConnection")]
pub struct CatalogDefinitionRows {
    pub nodes: Vec<CatalogDefinition>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "CatalogEnvironments")]
pub struct CatalogEnvironment {
    pub environment: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "CatalogEnvironmentsConnection")]
pub struct CatalogEnvironmentRows {
    pub nodes: Vec<CatalogEnvironment>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "CatalogReleases")]
pub struct CatalogRelease {
    pub id: String,
    pub source: String,
    pub source_digest: String,
    pub released_at: String,
    #[arguments(orderBy: { environment: ASC })]
    pub catalog_environments: CatalogEnvironmentRows,
    #[arguments(orderBy: { definitionKind: ASC, identity: ASC, version: ASC })]
    pub catalog_definitions: CatalogDefinitionRows,
}

/// The projection head names the release a service reads.
#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "CatalogProjectionHeads")]
pub struct CatalogHead {
    pub catalog_releases: Option<CatalogRelease>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "CatalogProjectionHeadsConnection")]
pub struct CatalogHeads {
    pub nodes: Vec<CatalogHead>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Organizations")]
pub struct VisibleOrganization {
    pub id: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "OrganizationsConnection")]
pub struct VisibleOrganizations {
    pub nodes: Vec<VisibleOrganization>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "ReusableResourceVersions")]
pub struct ReusableResourceVersion {
    pub version: i32,
    pub content_digest: String,
    pub dependencies: GeneratedJson,
    pub published_at: String,
}

impl ReusableResourceVersion {
    pub fn dependencies(&self) -> Vec<String> {
        strings(&self.dependencies)
    }
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "ReusableResourceVersionsConnection")]
pub struct ReusableResourceVersionRows {
    pub nodes: Vec<ReusableResourceVersion>,
}

/// The draft revision a resource currently points at (the computed `ReusableResources.draft`).
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "ReusableResourceDrafts")]
pub struct ReusableResourceDraft {
    pub content: String,
    pub content_digest: String,
    pub dependencies: GeneratedJson,
    pub validation_status: String,
    pub diagnostics: GeneratedJson,
}

impl ReusableResourceDraft {
    pub fn dependencies(&self) -> Vec<String> {
        strings(&self.dependencies)
    }

    pub fn diagnostics(&self) -> Vec<String> {
        strings(&self.diagnostics)
    }
}

/// A generated `ReusableResources` row. The reads and every command payload select this fragment.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "ReusableResources")]
pub struct ReusableResource {
    pub id: String,
    pub project_id: String,
    pub resource_kind: String,
    pub name: String,
    pub identity: String,
    pub current_draft_revision: i32,
    pub current_published_version: Option<i32>,
    pub lifecycle_status: String,
    pub draft: ReusableResourceDraft,
    pub dependent_resources: Vec<String>,
    /// Newest first.
    #[arguments(orderBy: { version: DESC })]
    pub reusable_resource_versions: ReusableResourceVersionRows,
}

impl ReusableResource {
    pub fn versions(&self) -> &[ReusableResourceVersion] {
        &self.reusable_resource_versions.nodes
    }
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ConfigurationCatalogVariables {
    pub organization: OrganizationsFilterInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ConfigurationCatalogVariables")]
pub struct ConfigurationCatalog {
    /// Empty when the principal cannot see the organization the catalog was asked for.
    #[arguments(filters: $organization)]
    pub organizations: VisibleOrganizations,
    #[arguments(filters: { id: { eq: "local" } })]
    pub catalog_projection_heads: CatalogHeads,
}

/// The release the local catalog projection points at. `Ok(None)` is "unavailable": the
/// organization is not visible to the principal, or the catalog has no release.
pub async fn request_catalog(
    organization_id: &str,
) -> Result<Option<CatalogRelease>, GraphqlError> {
    if !is_uuid(organization_id) {
        return Ok(None);
    }
    let data = execute(ConfigurationCatalog::build(ConfigurationCatalogVariables {
        organization: OrganizationsFilterInput {
            id: Some(TextFilterInput::eq(organization_id)),
            ..Default::default()
        },
    }))
    .await?;
    let visible = data
        .organizations
        .nodes
        .iter()
        .any(|organization| organization.id.eq_ignore_ascii_case(organization_id));
    if !visible {
        return Ok(None);
    }
    Ok(data
        .catalog_projection_heads
        .nodes
        .into_iter()
        .next()
        .and_then(|head| head.catalog_releases))
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct ReusableResourcesFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub resource_kind: Option<StringFilterInput>,
}

/// By identity, with the primary key as the final tie-break (`directory::ascending`).
#[derive(cynic::InputObject, Debug, Clone)]
pub struct ReusableResourcesOrderInput {
    pub identity: OrderByEnum,
    pub id: OrderByEnum,
}

/// By name, with the primary key as the final tie-break.
#[derive(cynic::InputObject, Debug, Clone)]
pub struct ProjectToolConnectionsOrderInput {
    pub name: OrderByEnum,
    pub id: OrderByEnum,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ReusableResourcesConnection")]
pub struct ReusableResourceRows {
    pub nodes: Vec<ReusableResource>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ConfigurationResourcesVariables {
    pub project: ProjectsFilterInput,
    pub filters: ReusableResourcesFilterInput,
    pub order_by: ReusableResourcesOrderInput,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "Projects",
    variables = "ConfigurationResourcesVariables"
)]
pub struct ProjectResources {
    #[arguments(filters: $filters, orderBy: $order_by)]
    pub reusable_resources: ReusableResourceRows,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "ProjectsConnection",
    variables = "ConfigurationResourcesVariables"
)]
pub struct ProjectsWithResources {
    pub nodes: Vec<ProjectResources>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ConfigurationResourcesVariables")]
pub struct ConfigurationResources {
    /// Empty when the project is not visible to the principal.
    #[arguments(filters: $project)]
    pub projects: ProjectsWithResources,
}

fn resources_of(
    project_id: &str,
    filters: ReusableResourcesFilterInput,
) -> ConfigurationResourcesVariables {
    ConfigurationResourcesVariables {
        project: ProjectsFilterInput {
            id: Some(TextFilterInput::eq(project_id)),
            ..Default::default()
        },
        filters,
        order_by: ReusableResourcesOrderInput {
            identity: OrderByEnum::Asc,
            id: OrderByEnum::Asc,
        },
    }
}

/// The project's resources by identity. `Ok(None)` is "unavailable", which is not the same as an
/// empty list.
pub async fn request_resources(
    project_id: &str,
    kind: Option<String>,
) -> Result<Option<Vec<ReusableResource>>, GraphqlError> {
    if !is_uuid(project_id) {
        return Ok(None);
    }
    let filters = ReusableResourcesFilterInput {
        resource_kind: kind.as_deref().map(StringFilterInput::eq),
        ..Default::default()
    };
    let projects = execute(ConfigurationResources::build(resources_of(
        project_id, filters,
    )))
    .await?
    .projects;
    Ok(projects
        .nodes
        .into_iter()
        .next()
        .map(|project| project.reusable_resources.nodes))
}

/// A selectable dependency: a catalog definition or an immutable version of a project resource.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceOption {
    pub value: String,
    pub label: String,
    pub kind: String,
}

/// What `requestKnownReferences` returns: the catalog, the project's resources, and every reference
/// either of them makes selectable.
pub struct KnownReferences {
    pub catalog: Option<CatalogRelease>,
    pub resources: Vec<ReusableResource>,
    pub options: Vec<ReferenceOption>,
}

pub async fn request_known(
    project_id: &str,
    organization_id: &str,
) -> Result<KnownReferences, GraphqlError> {
    let catalog = request_catalog(organization_id).await?;
    let resources = request_resources(project_id, None)
        .await?
        .unwrap_or_default();
    let options = reference_options(catalog.as_ref(), &resources);
    Ok(KnownReferences {
        catalog,
        resources,
        options,
    })
}

/// Everything an agent draft may name as its model or a dependency.
pub async fn request_known_references(
    project_id: &str,
    organization_id: &str,
) -> Result<Vec<ReferenceOption>, GraphqlError> {
    Ok(request_known(project_id, organization_id).await?.options)
}

/// `policy`, `model-profile`: the kind as typed references spell it.
pub fn reference_kind(kind: &str) -> String {
    kind.to_lowercase().replacen('_', "-", 1)
}

fn reference_options(
    catalog: Option<&CatalogRelease>,
    resources: &[ReusableResource],
) -> Vec<ReferenceOption> {
    let mut options: Vec<ReferenceOption> = catalog
        .map(|release| release.catalog_definitions.nodes.clone())
        .unwrap_or_default()
        .into_iter()
        .map(|definition| {
            let value = format!(
                "{}:{}@{}",
                definition.definition_kind, definition.identity, definition.version
            );
            ReferenceOption {
                label: format!("{} · {value}", definition.display_name),
                value,
                kind: definition.definition_kind,
            }
        })
        .collect();
    for resource in resources {
        let kind = reference_kind(&resource.resource_kind);
        options.extend(resource.versions().iter().map(|version| ReferenceOption {
            value: format!("{kind}:{}@v{}", resource.identity, version.version),
            label: format!("{} · immutable v{}", resource.name, version.version),
            kind: kind.clone(),
        }));
    }
    options
}

/// The same read as `ConfigurationResources`, for one resource, under its own operation name.
#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ConfigurationResourcesVariables")]
pub struct ConfigurationResource {
    /// Empty when the project is not visible to the principal.
    #[arguments(filters: $project)]
    pub projects: ProjectsWithResources,
}

pub async fn request_resource(
    project_id: &str,
    resource_id: &str,
) -> Result<Option<ReusableResource>, GraphqlError> {
    if !is_uuid(project_id) || !is_uuid(resource_id) {
        return Ok(None);
    }
    let filters = ReusableResourcesFilterInput {
        id: Some(TextFilterInput::eq(resource_id)),
        ..Default::default()
    };
    let projects = execute(ConfigurationResource::build(resources_of(
        project_id, filters,
    )))
    .await?
    .projects;
    Ok(projects
        .nodes
        .into_iter()
        .next()
        .and_then(|project| project.reusable_resources.nodes.into_iter().next()))
}

/// A generated `ProjectToolConnections` row: an inert MCP server descriptor. `arguments`,
/// `remote_url`, `status` and `dependent_resources` are computed fields.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "ProjectToolConnections")]
pub struct McpServerConfiguration {
    pub id: String,
    pub project_id: String,
    pub server_id: Option<String>,
    pub name: String,
    pub definition_identity: String,
    pub definition_version: String,
    pub environment: String,
    pub enabled: Option<bool>,
    pub transport_type: Option<String>,
    pub stdio_command: Option<String>,
    pub arguments: Vec<String>,
    pub remote_url: Option<String>,
    pub redacted_bindings: Option<GeneratedJson>,
    pub declared_tools: Option<GeneratedJson>,
    pub declared_resources: Option<GeneratedJson>,
    pub declared_prompts: Option<GeneratedJson>,
    pub lifecycle_status: String,
    pub status: String,
    pub revision: i32,
    pub dependent_resources: Vec<String>,
}

impl McpServerConfiguration {
    pub fn redacted_bindings(&self) -> Vec<String> {
        self.redacted_bindings
            .as_ref()
            .map(strings)
            .unwrap_or_default()
    }

    pub fn tools(&self) -> Vec<String> {
        self.declared_tools
            .as_ref()
            .map(strings)
            .unwrap_or_default()
    }

    pub fn resources(&self) -> Vec<String> {
        self.declared_resources
            .as_ref()
            .map(strings)
            .unwrap_or_default()
    }

    pub fn prompts(&self) -> Vec<String> {
        self.declared_prompts
            .as_ref()
            .map(strings)
            .unwrap_or_default()
    }
}

/// The one problem type every command payload lists its refusals with.
#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Problem")]
pub struct ConfigurationProblemFields {
    pub message: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ConfigurationMutationPayload {
    pub resource: Option<ReusableResource>,
    pub mcp_server: Option<McpServerConfiguration>,
    pub problems: Vec<ConfigurationProblemFields>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectToolConnectionsConnection")]
pub struct McpServerRows {
    pub nodes: Vec<McpServerConfiguration>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ProjectMcpServersVariables {
    pub project: ProjectsFilterInput,
    pub order_by: ProjectToolConnectionsOrderInput,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Projects", variables = "ProjectMcpServersVariables")]
pub struct ProjectServers {
    #[arguments(orderBy: $order_by)]
    pub project_tool_connections: McpServerRows,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "ProjectsConnection",
    variables = "ProjectMcpServersVariables"
)]
pub struct ProjectsWithServers {
    pub nodes: Vec<ProjectServers>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ProjectMcpServersVariables")]
pub struct ProjectMcpServers {
    /// Empty when the project is not visible to the principal.
    #[arguments(filters: $project)]
    pub projects: ProjectsWithServers,
}

/// The project's MCP servers by name. `Ok(None)` is "unavailable".
pub async fn request_mcp_servers(
    project_id: &str,
) -> Result<Option<Vec<McpServerConfiguration>>, GraphqlError> {
    if !is_uuid(project_id) {
        return Ok(None);
    }
    let projects = execute(ProjectMcpServers::build(ProjectMcpServersVariables {
        project: ProjectsFilterInput {
            id: Some(TextFilterInput::eq(project_id)),
            ..Default::default()
        },
        order_by: ProjectToolConnectionsOrderInput {
            name: OrderByEnum::Asc,
            id: OrderByEnum::Asc,
        },
    }))
    .await?
    .projects;
    Ok(projects
        .nodes
        .into_iter()
        .next()
        .map(|project| project.project_tool_connections.nodes))
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct CreateReusableResourceInput {
    pub project_id: cynic::Id,
    pub kind: String,
    pub name: String,
    pub content: String,
    pub dependencies: Vec<String>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdateReusableResourceDraftInput {
    pub project_id: cynic::Id,
    pub resource_id: cynic::Id,
    pub expected_revision: i32,
    pub content: String,
    pub dependencies: Vec<String>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct ReusableResourceRevisionInput {
    pub project_id: cynic::Id,
    pub resource_id: cynic::Id,
    pub expected_revision: i32,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct CreateProjectMcpServerInput {
    pub project_id: cynic::Id,
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

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdateProjectMcpServerInput {
    pub project_id: cynic::Id,
    pub id: cynic::Id,
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

/// One mutation root per operation, each returning the shared payload under its own field. `$d` is
/// a literal `$`: cynic spells a variable `$input`, which a macro would otherwise read as its own.
macro_rules! configuration_mutation {
    ($d:tt, $root:ident, $variables:ident, $variables_name:literal, $input_type:ty, $field:ident, $call:ident) => {
        #[derive(cynic::QueryVariables, Debug)]
        pub struct $variables {
            pub input: $input_type,
        }

        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "Mutation", variables = $variables_name)]
        pub struct $root {
            #[arguments(input: $d input)]
            pub $field: ConfigurationMutationPayload,
        }

        pub async fn $call(
            input: $input_type,
        ) -> Result<ConfigurationMutationPayload, GraphqlError> {
            Ok(execute($root::build($variables { input })).await?.$field)
        }
    };
}

configuration_mutation!($, CreateReusableResource, CreateReusableResourceVariables, "CreateReusableResourceVariables", CreateReusableResourceInput, create_reusable_resource, create_resource);
configuration_mutation!($, UpdateReusableResourceDraft, UpdateReusableResourceDraftVariables, "UpdateReusableResourceDraftVariables", UpdateReusableResourceDraftInput, update_reusable_resource_draft, update_resource_draft);
configuration_mutation!($, ValidateReusableResource, ValidateReusableResourceVariables, "ValidateReusableResourceVariables", ReusableResourceRevisionInput, validate_reusable_resource, validate_resource);
configuration_mutation!($, PublishReusableResource, PublishReusableResourceVariables, "PublishReusableResourceVariables", ReusableResourceRevisionInput, publish_reusable_resource, publish_resource);
configuration_mutation!($, CreateProjectMcpServer, CreateProjectMcpServerVariables, "CreateProjectMcpServerVariables", CreateProjectMcpServerInput, create_project_mcp_server, create_mcp_server);
configuration_mutation!($, UpdateProjectMcpServer, UpdateProjectMcpServerVariables, "UpdateProjectMcpServerVariables", UpdateProjectMcpServerInput, update_project_mcp_server, update_mcp_server);
