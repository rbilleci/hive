//! Computed fields of the generated configuration
//! objects. Each is derived from the row it is on, plus rows loaded through SeaORM. `hive-api`
//! attaches them to the generated objects.
//!
//! - `ReusableResources.draft`: the draft revision the resource currently points at
//!   (`currentDraftRevision`); older revisions stay readable through `reusableResourceDrafts`.
//! - `ReusableResources.dependentResources`: the names of the project's resources whose current
//!   draft or any published version depends on this resource's current published version.
//! - `ProjectToolConnections.arguments` / `remoteUrl`: the stored stdio arguments and remote URL.
//!   A stored value that looks like secret material is withheld, which is why the two columns
//!   themselves are not generated fields.
//! - `ProjectToolConnections.status`: `ARCHIVED`, `DISABLED`, `INCOMPLETE` or `NOT_CHECKED`,
//!   derived from the descriptor; distinct from the stored `lifecycleStatus`.
//! - `ProjectToolConnections.dependentResources`: the names of the project's resources with a
//!   published version that depends on the server's tool definition.

#![allow(non_snake_case)] // a computed field is named after its method

use super::rows;
use crate::console::requester;
use crate::entity::enums::ConnectionLifecycleStatus;
use crate::entity::{project_tool_connections, reusable_resource_drafts, reusable_resources};
use hive_application::configuration::{mcp_server_status, safe_arguments, safe_remote_url};
// `#[CustomFields]` expands to paths that start with `async_graphql::`.
use seaography::async_graphql::{self, Context};
use seaography::CustomFields;

#[CustomFields]
impl reusable_resources::Model {
    /// The resource's current draft.
    pub async fn draft(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<reusable_resource_drafts::Model> {
        let (_, db) = requester(ctx)?;
        Ok(rows::draft_row(db, self.id, self.current_draft_revision).await?)
    }

    /// The names of the resources that depend on this resource's current published version.
    pub async fn dependentResources(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<String>> {
        let (_, db) = requester(ctx)?;
        match self.current_published_version {
            Some(version) => Ok(rows::resource_dependents(db, self, version).await?),
            None => Ok(Vec::new()),
        }
    }
}

impl project_tool_connections::Model {
    fn stored_arguments(&self) -> Vec<String> {
        self.stdio_arguments
            .as_ref()
            .map(rows::strings)
            .unwrap_or_default()
    }
}

#[CustomFields]
impl project_tool_connections::Model {
    /// The stdio arguments; none when a stored argument looks like secret material.
    pub async fn arguments(&self, _ctx: &Context<'_>) -> async_graphql::Result<Vec<String>> {
        Ok(safe_arguments(self.stored_arguments()))
    }

    /// The remote URL; none when the stored URL carries credentials.
    pub async fn remoteUrl(&self, _ctx: &Context<'_>) -> async_graphql::Result<Option<String>> {
        Ok(safe_remote_url(self.remote_url.clone()))
    }

    /// `ARCHIVED`, `DISABLED`, `INCOMPLETE` or `NOT_CHECKED`.
    pub async fn status(&self, _ctx: &Context<'_>) -> async_graphql::Result<String> {
        Ok(mcp_server_status(
            self.lifecycle_status == ConnectionLifecycleStatus::Archived,
            self.enabled.unwrap_or(true),
            self.transport_type.as_deref(),
            self.stdio_command.as_deref(),
            &self.stored_arguments(),
            self.remote_url.as_deref(),
        )
        .to_string())
    }

    /// The names of the resources with a published version that depends on this server's tool
    /// definition.
    pub async fn dependentResources(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<String>> {
        let (_, db) = requester(ctx)?;
        Ok(rows::tool_dependents(
            db,
            self.project_id,
            &self.definition_identity,
            &self.definition_version,
        )
        .await?)
    }
}
