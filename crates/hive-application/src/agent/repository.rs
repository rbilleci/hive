//! The persistence boundary the agent-draft commands are written against.

use crate::agent::models::AgentDraftMutationResult;
use crate::RepositoryError;
use async_trait::async_trait;
use uuid::Uuid;

/// What a repository's commands answer with.
pub type CommandResult<R> = Result<
    AgentDraftMutationResult<
        <R as AgentDraftRepository>::Draft,
        <R as AgentDraftRepository>::Version,
    >,
    RepositoryError,
>;

/// Persistence boundary for atomic compare-and-set draft commands. `Draft` and `Version` are the
/// repository's own stored rows.
#[async_trait]
pub trait AgentDraftRepository: Send + Sync {
    type Draft: Send;
    type Version: Send;

    async fn update_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        expected_revision: i64,
        document: String,
    ) -> CommandResult<Self>;

    async fn validate_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        expected_revision: i64,
    ) -> CommandResult<Self>;

    async fn create_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        display_name: String,
        slug: Option<String>,
    ) -> CommandResult<Self>;

    async fn publish_draft(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        expected_revision: i64,
        warnings_acknowledged: bool,
    ) -> CommandResult<Self>;
}
