//! Ports `PostgresConfigurationRepository` in full: `catalog`, `resources`,
//! `resource`, `mcpServers`, `createResource`, `updateDraft`, `validate`,
//! `publish`, `createMcpServer`, `updateMcpServer`, `saveLegacyTool`, and
//! every helper they call. `queries` holds the four reads, `mutations` the
//! write commands, and `rows` the diagnostics, reference resolution, read
//! assembly, and locked-row helpers both share; this module keeps only the
//! struct and the trait `impl`, which dispatches one call per method.
//!
//! Three tables (`reusable_resources.dependencies` via
//! `reusable_resource_drafts`/`reusable_resource_versions`, and
//! `catalog_definitions.available_environments`) store a JSON array of
//! strings, not an array type — Aurora DSQL rejects array types outright — so
//! array-membership tests use a `jsonb` containment check (`@>` against a
//! single-element array built from the parameter) rather than `= ANY(...)`.
//!
//! Every write command re-evaluates the current server capability inside the
//! same transaction it mutates in, via `capability::tx::configuration_write`
//! (`CONFIGURATION.AUTHOR`/`CONFIGURATION.PUBLISH`/`TOOL_CONNECTION.UPDATE`
//! all route through it) and `capability::tx::active_project`. Read-only
//! methods call the existing pool-based, unlocked `capability::has_capability`
//! directly, matching Java's `lock = false` read paths.
//!
//! `updateDraft`/`validate`/`publish`/`updateMcpServer`/`saveLegacyTool` read
//! their row `FOR UPDATE` and then write to it unconditionally (no `WHERE
//! revision = ?` on the UPDATE); DSQL validates that locked read at commit
//! instead of blocking a concurrent writer, surfacing SQLSTATE 40001 for the
//! loser — the same pattern `agent::draft`'s `command()` established, ported
//! here as an inline retry at each `tx.commit()` in `mutations` (Java's own
//! `transaction(Command, Command onConflict)` overload). The retry re-reads
//! the row `FOR UPDATE` again (matching Java exactly) rather than unlocked —
//! by the time it runs the racing writer has already committed, so this read
//! cannot itself race further.
//!
//! Every shared helper takes a concrete `&mut PgConnection` (see
//! `agent::draft`'s module doc comment for why, not a generic executor):
//! read-only repository methods acquire one via `pool.acquire()`, write
//! methods pass `&mut tx`.

mod mutations;
mod queries;
mod rows;

use hive_application::configuration::{
    CatalogRelease, ConfigurationMutationResult, ConfigurationRepository,
    ConfigurationRepositoryError as RepositoryError, McpServerConfiguration, ReusableResource,
    TypedReference,
};
use sqlx::PgPool;
use uuid::Uuid;

pub struct PgConfigurationRepository {
    pool: PgPool,
}

impl PgConfigurationRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ConfigurationRepository for PgConfigurationRepository {
    async fn catalog(
        &self,
        principal: Uuid,
        organization: Uuid,
    ) -> Result<Option<CatalogRelease>, RepositoryError> {
        queries::catalog(&self.pool, principal, organization).await
    }

    async fn resources(
        &self,
        principal: Uuid,
        project: Uuid,
        kind: Option<String>,
    ) -> Result<Option<Vec<ReusableResource>>, RepositoryError> {
        queries::resources(&self.pool, principal, project, kind).await
    }

    async fn resource(
        &self,
        principal: Uuid,
        project: Uuid,
        resource_id: Uuid,
    ) -> Result<Option<ReusableResource>, RepositoryError> {
        queries::resource(&self.pool, principal, project, resource_id).await
    }

    async fn mcp_servers(
        &self,
        principal: Uuid,
        project: Uuid,
    ) -> Result<Option<Vec<McpServerConfiguration>>, RepositoryError> {
        queries::mcp_servers(&self.pool, principal, project).await
    }

    async fn create_resource(
        &self,
        actor: Uuid,
        project: Uuid,
        kind: String,
        name: String,
        identity: String,
        content: String,
        dependencies: Vec<TypedReference>,
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        mutations::create_resource(
            &self.pool,
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
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        mutations::update_draft(
            &self.pool,
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
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        mutations::validate(&self.pool, actor, project, id, expected_revision).await
    }

    async fn publish(
        &self,
        actor: Uuid,
        project: Uuid,
        id: Uuid,
        expected_revision: i64,
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        mutations::publish(&self.pool, actor, project, id, expected_revision).await
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
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        mutations::create_mcp_server(
            &self.pool,
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
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        mutations::update_mcp_server(
            &self.pool,
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
    ) -> Result<ConfigurationMutationResult, RepositoryError> {
        mutations::save_legacy_tool(
            &self.pool,
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
