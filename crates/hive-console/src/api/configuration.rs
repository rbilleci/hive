//! Ports the catalog and reusable-resource reads of `configuration.graphql` and `configurationApi.ts`.

use crate::graphql::{execute, schema, GraphqlError};
use cynic::{MutationBuilder, QueryBuilder};

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct CatalogDefinition {
    pub identity: String,
    pub version: String,
    pub kind: String,
    pub display_name: String,
    pub content_digest: String,
    pub available_environments: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct CatalogRelease {
    pub id: cynic::Id,
    pub source: String,
    pub source_digest: String,
    pub released_at: String,
    pub environments: Vec<String>,
    pub definitions: Vec<CatalogDefinition>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ReusableResourceVersion {
    pub version: i32,
    pub content_digest: String,
    pub canonical_document: String,
    pub dependencies: Vec<String>,
    pub published_at: String,
    pub published_by: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ReusableResource {
    pub id: cynic::Id,
    pub project_id: cynic::Id,
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
    pub lifecycle_status: String,
    pub dependent_resources: Vec<String>,
    pub versions: Vec<ReusableResourceVersion>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ConfigurationCatalogVariables {
    pub organization_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ConfigurationCatalogVariables")]
pub struct ConfigurationCatalog {
    #[arguments(organizationId: $organization_id)]
    pub catalog_release: Option<CatalogRelease>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ConfigurationResourcesVariables {
    pub project_id: cynic::Id,
    pub kind: Option<String>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ConfigurationResourcesVariables")]
pub struct ConfigurationResources {
    #[arguments(projectId: $project_id, kind: $kind)]
    pub reusable_resources: Option<Vec<ReusableResource>>,
}

pub async fn request_catalog(
    organization_id: &str,
) -> Result<Option<CatalogRelease>, GraphqlError> {
    Ok(
        execute(ConfigurationCatalog::build(ConfigurationCatalogVariables {
            organization_id: organization_id.into(),
        }))
        .await?
        .catalog_release,
    )
}

/// `Ok(None)` is the server's "unavailable", which is not the same as an empty list.
pub async fn request_resources(
    project_id: &str,
    kind: Option<String>,
) -> Result<Option<Vec<ReusableResource>>, GraphqlError> {
    let variables = ConfigurationResourcesVariables {
        project_id: project_id.into(),
        kind,
    };
    Ok(execute(ConfigurationResources::build(variables))
        .await?
        .reusable_resources)
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
        .map(|release| release.definitions.clone())
        .unwrap_or_default()
        .into_iter()
        .map(|definition| {
            let value = format!(
                "{}:{}@{}",
                definition.kind, definition.identity, definition.version
            );
            ReferenceOption {
                label: format!("{} · {value}", definition.display_name),
                value,
                kind: definition.kind,
            }
        })
        .collect();
    for resource in resources {
        let kind = reference_kind(&resource.kind);
        options.extend(resource.versions.iter().map(|version| ReferenceOption {
            value: format!("{kind}:{}@v{}", resource.identity, version.version),
            label: format!("{} · immutable v{}", resource.name, version.version),
            kind: kind.clone(),
        }));
    }
    options
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ConfigurationResourceVariables {
    pub project_id: cynic::Id,
    pub resource_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ConfigurationResourceVariables")]
pub struct ConfigurationResource {
    #[arguments(projectId: $project_id, resourceId: $resource_id)]
    pub reusable_resource: Option<ReusableResource>,
}

pub async fn request_resource(
    project_id: &str,
    resource_id: &str,
) -> Result<Option<ReusableResource>, GraphqlError> {
    let variables = ConfigurationResourceVariables {
        project_id: project_id.into(),
        resource_id: resource_id.into(),
    };
    Ok(execute(ConfigurationResource::build(variables))
        .await?
        .reusable_resource)
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct McpServerConfiguration {
    pub id: cynic::Id,
    pub project_id: cynic::Id,
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

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ConfigurationProblem")]
pub struct ConfigurationProblemFields {
    pub message: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ConfigurationMutationPayload {
    pub resource: Option<ReusableResource>,
    pub mcp_server: Option<McpServerConfiguration>,
    pub problems: Vec<ConfigurationProblemFields>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ProjectMcpServersVariables {
    pub project_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ProjectMcpServersVariables")]
pub struct ProjectMcpServers {
    #[arguments(projectId: $project_id)]
    pub project_mcp_servers: Option<Vec<McpServerConfiguration>>,
}

/// `Ok(None)` is the server's "unavailable".
pub async fn request_mcp_servers(
    project_id: &str,
) -> Result<Option<Vec<McpServerConfiguration>>, GraphqlError> {
    Ok(
        execute(ProjectMcpServers::build(ProjectMcpServersVariables {
            project_id: project_id.into(),
        }))
        .await?
        .project_mcp_servers,
    )
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
