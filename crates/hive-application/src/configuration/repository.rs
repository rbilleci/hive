//! The persistence boundary of the configuration commands. Reads go through the generated API,
//! so this trait has none.

use super::identity::TypedReference;
use super::models::ConfigurationMutationResult;
use async_trait::async_trait;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[async_trait]
pub trait ConfigurationRepository: Send + Sync {
    /// The stored `reusable_resources` row a resource command answers with.
    type Resource: Send;
    /// The stored `project_tool_connections` row an MCP server command answers with.
    type McpServer: Send;

    #[allow(clippy::too_many_arguments)]
    async fn create_resource(
        &self,
        actor: Uuid,
        project: Uuid,
        kind: String,
        name: String,
        identity: String,
        content: String,
        dependencies: Vec<TypedReference>,
    ) -> Result<ConfigurationMutationResult<Self::Resource, Self::McpServer>, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn update_draft(
        &self,
        actor: Uuid,
        project: Uuid,
        resource: Uuid,
        expected_revision: i64,
        content: String,
        dependencies: Vec<TypedReference>,
    ) -> Result<ConfigurationMutationResult<Self::Resource, Self::McpServer>, RepositoryError>;

    async fn validate(
        &self,
        actor: Uuid,
        project: Uuid,
        resource: Uuid,
        expected_revision: i64,
    ) -> Result<ConfigurationMutationResult<Self::Resource, Self::McpServer>, RepositoryError>;

    async fn publish(
        &self,
        actor: Uuid,
        project: Uuid,
        resource: Uuid,
        expected_revision: i64,
    ) -> Result<ConfigurationMutationResult<Self::Resource, Self::McpServer>, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn create_mcp_server(
        &self,
        actor: Uuid,
        project: Uuid,
        server_id: String,
        name: String,
        definition: TypedReference,
        environment: String,
        enabled: bool,
        transport_type: String,
        command: Option<String>,
        arguments: Vec<String>,
        remote_url: Option<String>,
        redacted_bindings: Vec<String>,
        tools: Vec<String>,
        resources: Vec<String>,
        prompts: Vec<String>,
    ) -> Result<ConfigurationMutationResult<Self::Resource, Self::McpServer>, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn update_mcp_server(
        &self,
        actor: Uuid,
        project: Uuid,
        server: Uuid,
        expected_revision: i64,
        name: String,
        definition: TypedReference,
        environment: String,
        enabled: bool,
        transport_type: String,
        command: Option<String>,
        arguments: Vec<String>,
        remote_url: Option<String>,
        redacted_bindings: Vec<String>,
        tools: Vec<String>,
        resources: Vec<String>,
        prompts: Vec<String>,
        lifecycle_status: String,
    ) -> Result<ConfigurationMutationResult<Self::Resource, Self::McpServer>, RepositoryError>;

    /// M11 compatibility is intentionally inert; it cannot create an
    /// executable transport. Ports `saveLegacyTool`.
    #[allow(clippy::too_many_arguments)]
    async fn save_legacy_tool(
        &self,
        actor: Uuid,
        project: Uuid,
        tool: Option<Uuid>,
        expected_revision: i64,
        name: String,
        definition: TypedReference,
        environment: String,
        redacted_secret_reference: String,
        lifecycle: String,
        rotation_summary: String,
    ) -> Result<ConfigurationMutationResult<Self::Resource, Self::McpServer>, RepositoryError>;
}
