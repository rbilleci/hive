//! Ports `AgentOperationalView`, `AgentOperationalViewRepository`, and
//! `AgentOperationalViewQueryService`: a transport-neutral lookup that makes
//! absent, malformed, foreign, and revoked targets indistinguishable.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftValidationSummary {
    pub status: String,
    pub error_count: i32,
    pub warning_count: i32,
    pub validated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedVersionSummary {
    pub status: String,
    pub version: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasTargetsSummary {
    pub total_count: i32,
    pub active_count: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentSummary {
    pub status: String,
    pub observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationSummary {
    pub outcome: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHealthSummary {
    pub status: String,
    pub observed_at: Option<DateTime<Utc>>,
    pub freshness: String,
}

/// Compact, server-owned operational summaries for exactly one member-scoped
/// agent route. Ports `AgentOperationalView`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOperationalView {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub draft_validation: DraftValidationSummary,
    pub latest_published_version: PublishedVersionSummary,
    pub alias_targets: AliasTargetsSummary,
    pub active_deployment: DeploymentSummary,
    pub recent_evaluation: EvaluationSummary,
    pub runtime_health: RuntimeHealthSummary,
}

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Persistence boundary for a read-only operational view scoped by member,
/// project, and agent. Ports `AgentOperationalViewRepository`.
#[async_trait]
pub trait AgentOperationalViewRepository: Send + Sync {
    async fn find_overview(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
    ) -> Result<Option<AgentOperationalView>, RepositoryError>;
}

/// Ports `AgentOperationalViewQueryService`.
pub struct AgentOperationalViewQueryService<R: AgentOperationalViewRepository> {
    repository: R,
}

impl<R: AgentOperationalViewRepository> AgentOperationalViewQueryService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub async fn find_overview(
        &self,
        principal_id: Uuid,
        requested_project_id: &str,
        requested_agent_id: &str,
    ) -> Result<Option<AgentOperationalView>, RepositoryError> {
        let (Ok(project_id), Ok(agent_id)) = (
            Uuid::parse_str(requested_project_id),
            Uuid::parse_str(requested_agent_id),
        ) else {
            return Ok(None);
        };
        self.repository
            .find_overview(principal_id, project_id, agent_id)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubRepository;

    #[async_trait]
    impl AgentOperationalViewRepository for StubRepository {
        async fn find_overview(
            &self,
            _principal_id: Uuid,
            _project_id: Uuid,
            _agent_id: Uuid,
        ) -> Result<Option<AgentOperationalView>, RepositoryError> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn a_malformed_project_id_is_not_found_rather_than_an_error() {
        let service = AgentOperationalViewQueryService::new(StubRepository);
        let result = service
            .find_overview(Uuid::new_v4(), "not-a-uuid", &Uuid::new_v4().to_string())
            .await
            .unwrap();
        assert!(result.is_none());
    }
}
