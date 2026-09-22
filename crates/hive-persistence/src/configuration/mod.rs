//! Configuration on SeaORM entities. Reads are generated (`catalogProjectionHeads`,
//! `catalogReleases`, `catalogDefinitions`, `catalogEnvironments`, `reusableResources`,
//! `reusableResourceDrafts`, `reusableResourceVersions`, `projectToolConnections`), with the
//! derived values in `computed`. `mutations` holds the seven commands, `rows` what they share
//! with the computed fields; this module keeps the repository struct and the trait `impl`, which
//! dispatches one call per command.

pub mod computed;
mod mutations;
pub(crate) mod rows;

use crate::entity::{project_tool_connections, reusable_resources};
use hive_application::configuration::{ConfigurationRepository, TypedReference};
use hive_application::RepositoryError;
use mutations::MutationResult;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

pub struct PgConfigurationRepository {
    db: DatabaseConnection,
}

impl PgConfigurationRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl ConfigurationRepository for PgConfigurationRepository {
    type Resource = reusable_resources::Model;
    type McpServer = project_tool_connections::Model;

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
    ) -> Result<MutationResult, RepositoryError> {
        mutations::create_resource(
            &self.db,
            actor,
            project,
            kind,
            name,
            identity,
            content,
            dependencies,
        )
        .await
    }

    async fn update_draft(
        &self,
        actor: Uuid,
        project: Uuid,
        id: Uuid,
        expected_revision: i64,
        content: String,
        dependencies: Vec<TypedReference>,
    ) -> Result<MutationResult, RepositoryError> {
        mutations::update_draft(
            &self.db,
            actor,
            project,
            id,
            expected_revision,
            content,
            dependencies,
        )
        .await
    }

    async fn validate(
        &self,
        actor: Uuid,
        project: Uuid,
        id: Uuid,
        expected_revision: i64,
    ) -> Result<MutationResult, RepositoryError> {
        mutations::validate(&self.db, actor, project, id, expected_revision).await
    }

    async fn publish(
        &self,
        actor: Uuid,
        project: Uuid,
        id: Uuid,
        expected_revision: i64,
    ) -> Result<MutationResult, RepositoryError> {
        mutations::publish(&self.db, actor, project, id, expected_revision).await
    }

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
    ) -> Result<MutationResult, RepositoryError> {
        mutations::create_mcp_server(
            &self.db,
            actor,
            project,
            server_id,
            name,
            definition,
            environment,
            enabled,
            transport_type,
            command,
            arguments,
            remote_url,
            redacted_bindings,
            tools,
            resources,
            prompts,
        )
        .await
    }

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
    ) -> Result<MutationResult, RepositoryError> {
        mutations::update_mcp_server(
            &self.db,
            actor,
            project,
            server,
            expected_revision,
            name,
            definition,
            environment,
            enabled,
            transport_type,
            command,
            arguments,
            remote_url,
            redacted_bindings,
            tools,
            resources,
            prompts,
            lifecycle_status,
        )
        .await
    }

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
    ) -> Result<MutationResult, RepositoryError> {
        mutations::save_legacy_tool(
            &self.db,
            actor,
            project,
            tool,
            expected_revision,
            name,
            definition,
            environment,
            redacted_secret_reference,
            lifecycle,
            rotation_summary,
        )
        .await
    }
}
