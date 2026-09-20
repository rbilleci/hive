//! Computed fields (`docs/idiomatic-seaography-plan.md`, A4) of the generated agent authoring
//! objects. Each is derived from the row it is on, plus rows loaded through SeaORM, by the
//! application layer's document rules. `hive-api` attaches them to the generated objects.
//!
//! - `Agents.draft`: the agent's stored draft, or the default draft every agent starts from. An
//!   agent has no `agent_drafts` row until its first command, and the editor opens on the default
//!   document, so this is the draft the console reads.
//! - `AgentDrafts.canUpdate` / `canPublish`: answered by the capability evaluator for the
//!   requesting principal; `false` on an archived agent.
//! - `AgentDrafts.review`: what a publication of the draft would record.
//! - `AgentVersions.comparison(fromVersionId)`: the older version and the document sections that
//!   differ from it.

#![allow(non_snake_case)] // a computed field is named after its method

use super::draft;
use crate::capability::{self, Scope};
use crate::console::requester;
use crate::entity::enums::AgentLifecycleStatus;
use crate::entity::{agent_drafts, agent_versions, agents};
use hive_application::agent::canonical_document;
use hive_application::configuration::TypedReference;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
// `#[CustomFields]` and `CustomOutputType` expand to paths that start with `async_graphql::`.
use seaography::async_graphql::{self, Context};
use seaography::{CustomFields, CustomOutputType};
use uuid::Uuid;

/// One server-produced validation result. `path` locates the editor section it belongs to.
#[derive(CustomOutputType, Clone)]
pub struct AgentDraftDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub path: Vec<String>,
}

/// What publishing the draft as it stands would record.
#[derive(CustomOutputType, Clone)]
pub struct AgentDraftReview {
    pub contentDigest: String,
    pub dependencies: Vec<String>,
    /// `UNAVAILABLE` when the local catalog has no release.
    pub catalogReleaseId: String,
    pub catalogReleaseDigest: String,
    /// Against the newest published version, or against the default draft when there is none.
    pub changedSections: Vec<String>,
    pub diagnostics: Vec<AgentDraftDiagnostic>,
}

/// A published version compared with an older version of the same agent.
#[derive(CustomOutputType, Clone)]
pub struct AgentVersionComparison {
    /// The older version.
    pub from: agent_versions::Model,
    /// The document sections that differ between the two.
    pub changedSections: Vec<String>,
}

async fn agent_of(db: &impl ConnectionTrait, agent_id: Uuid) -> Result<agents::Model, DbErr> {
    agents::Entity::find_by_id(agent_id)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no agent with id {agent_id}")))
}

/// Whether the requesting principal holds `code` on an active agent's project.
async fn held_on_active_agent(
    ctx: &Context<'_>,
    agent_id: Uuid,
    code: &str,
) -> async_graphql::Result<bool> {
    let (principal_id, db) = requester(ctx)?;
    let agent = agent_of(db, agent_id).await?;
    Ok(agent.lifecycle_status == AgentLifecycleStatus::Active
        && capability::has_capability(
            db,
            principal_id,
            code,
            Scope::Project(agent.project_id),
            false,
        )
        .await?)
}

#[CustomFields]
impl agents::Model {
    /// The agent's draft: the stored one, or the default draft it starts from.
    pub async fn draft(&self, ctx: &Context<'_>) -> async_graphql::Result<agent_drafts::Model> {
        let (_, db) = requester(ctx)?;
        let stored = agent_drafts::Entity::find_by_id(self.id).one(db).await?;
        Ok(stored.unwrap_or_else(|| draft::default_draft(self)))
    }
}

#[CustomFields]
impl agent_drafts::Model {
    /// Whether the requesting principal may save and validate this draft.
    pub async fn canUpdate(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        held_on_active_agent(ctx, self.agent_id, capability::AGENT_DRAFT_UPDATE).await
    }

    /// Whether the requesting principal may publish this draft.
    pub async fn canPublish(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        held_on_active_agent(ctx, self.agent_id, capability::AGENT_DRAFT_PUBLISH).await
    }

    /// What publishing this draft would record: digest, dependencies, catalog release, the
    /// sections changed since the newest version, and the server's diagnostics.
    pub async fn review(&self, ctx: &Context<'_>) -> async_graphql::Result<AgentDraftReview> {
        let (_, db) = requester(ctx)?;
        let agent = agent_of(db, self.agent_id).await?;
        let document = draft::document_text(&self.document);
        // A stored draft is digested as publication digests it; the default draft is not stored.
        let digested = draft::stored_document_text(db, self.agent_id)
            .await?
            .unwrap_or_else(|| document.clone());
        let release = draft::catalog_release(db).await?;
        let prior = match draft::latest_version(db, self.agent_id).await? {
            Some(version) => draft::document_text(&version.canonical_document),
            None => canonical_document::default_document(&agent.display_name),
        };
        Ok(AgentDraftReview {
            contentDigest: canonical_document::digest(&digested),
            dependencies: canonical_document::dependencies(&document)
                .iter()
                .map(TypedReference::value)
                .collect(),
            catalogReleaseId: release
                .as_ref()
                .map_or_else(|| "UNAVAILABLE".to_string(), |release| release.id.clone()),
            catalogReleaseDigest: release
                .map(|release| release.source_digest)
                .unwrap_or_default(),
            changedSections: canonical_document::changed_sections(&prior, &document),
            diagnostics: draft::diagnostics(db, agent.project_id, &document)
                .await?
                .into_iter()
                .map(|value| AgentDraftDiagnostic {
                    code: value.code,
                    severity: value.severity,
                    message: value.message,
                    path: value.path,
                })
                .collect(),
        })
    }
}

#[CustomFields]
impl agent_versions::Model {
    /// This version compared with an older one. `null` when `fromVersionId` is not a version of
    /// the same agent.
    pub async fn comparison(
        &self,
        ctx: &Context<'_>,
        fromVersionId: String,
    ) -> async_graphql::Result<Option<AgentVersionComparison>> {
        let (_, db) = requester(ctx)?;
        let Ok(from_id) = Uuid::parse_str(&fromVersionId) else {
            return Ok(None);
        };
        let from = agent_versions::Entity::find_by_id(from_id)
            .filter(agent_versions::Column::AgentId.eq(self.agent_id))
            .one(db)
            .await?;
        Ok(from.map(|from| AgentVersionComparison {
            changedSections: canonical_document::changed_sections(
                &draft::document_text(&from.canonical_document),
                &draft::document_text(&self.canonical_document),
            ),
            from,
        }))
    }
}
