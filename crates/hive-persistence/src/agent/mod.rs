//! Agent authoring on SeaORM entities. Reads are the generated API (`agents`, `agentDrafts`,
//! `agentVersions` and their relations) plus the computed fields in `computed`; this module's
//! repository holds the four draft commands only (`mutations`). `rows` has the typed row helpers
//! the commands and the computed fields share, and the two audit writers.

pub mod computed;
mod mutations;
mod rows;

use crate::entity::{agent_drafts, agent_versions};
use hive_application::agent::AgentDraftRepository;
use hive_application::RepositoryError;
use mutations::MutationResult;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

pub use computed::{AgentDraftDiagnostic, AgentDraftReview, AgentVersionComparison};

pub struct PgAgentDraftRepository {
    db: DatabaseConnection,
}

impl PgAgentDraftRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl AgentDraftRepository for PgAgentDraftRepository {
    type Draft = agent_drafts::Model;
    type Version = agent_versions::Model;

    async fn create_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        display_name: String,
        requested_slug: Option<String>,
    ) -> Result<MutationResult, RepositoryError> {
        mutations::create_draft(&self.db, principal, project, display_name, requested_slug).await
    }

    async fn update_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        expected_revision: i64,
        document: String,
    ) -> Result<MutationResult, RepositoryError> {
        mutations::update_draft(
            &self.db,
            principal,
            project,
            agent,
            expected_revision,
            document,
        )
        .await
    }

    async fn validate_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        expected_revision: i64,
    ) -> Result<MutationResult, RepositoryError> {
        mutations::validate_draft(&self.db, principal, project, agent, expected_revision).await
    }

    async fn publish_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        expected_revision: i64,
        warnings_acknowledged: bool,
    ) -> Result<MutationResult, RepositoryError> {
        mutations::publish_draft(
            &self.db,
            principal,
            project,
            agent,
            expected_revision,
            warnings_acknowledged,
        )
        .await
    }
}
