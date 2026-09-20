use crate::agent::directory::{AgentDirectoryProject, AgentFilter, AgentPage};
use async_trait::async_trait;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("Invalid agent cursor.")]
    InvalidCursor,
    #[error("The agent cursor does not match the directory filters.")]
    CursorFilterMismatch,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Port `JpaProjectAgentDirectoryRepository` implements.
#[async_trait]
pub trait ProjectAgentDirectoryRepository: Send + Sync {
    async fn find_project(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
    ) -> Result<Option<AgentDirectoryProject>, RepositoryError>;

    async fn find_agents(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        first: i64,
        after: Option<&str>,
        filter: &AgentFilter,
    ) -> Result<AgentPage, RepositoryError>;

    async fn find_agents_before(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        last: i64,
        before: Option<&str>,
        filter: &AgentFilter,
    ) -> Result<AgentPage, RepositoryError>;
}

const MAX_PAGE_SIZE: i64 = 50;

/// Transport-neutral lookup for one member-scoped project agent directory. Ports
/// `ProjectAgentDirectoryQueryService`.
pub struct ProjectAgentDirectoryQueryService<R: ProjectAgentDirectoryRepository> {
    repository: R,
}

impl<R: ProjectAgentDirectoryRepository> ProjectAgentDirectoryQueryService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    /// An unparseable `requested_project_id` is not found, not an error, matching
    /// `parseProjectId`'s `catch (IllegalArgumentException) { return Optional.empty(); }`.
    pub async fn find_project(
        &self,
        principal_id: Uuid,
        requested_project_id: &str,
    ) -> Result<Option<AgentDirectoryProject>, RepositoryError> {
        let Ok(project_id) = Uuid::parse_str(requested_project_id) else {
            return Ok(None);
        };
        self.repository.find_project(principal_id, project_id).await
    }

    pub async fn find_agents(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        requested_first: i64,
        after: Option<&str>,
        filter: Option<AgentFilter>,
    ) -> Result<AgentPage, RepositoryError> {
        let first = if requested_first <= 0 {
            MAX_PAGE_SIZE
        } else {
            requested_first.min(MAX_PAGE_SIZE)
        };
        let filter = filter.unwrap_or_else(AgentFilter::none);
        self.repository
            .find_agents(principal_id, project_id, first, after, &filter)
            .await
    }

    pub async fn find_agents_before(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        requested_last: i64,
        before: Option<&str>,
        filter: Option<AgentFilter>,
    ) -> Result<AgentPage, RepositoryError> {
        let last = if requested_last <= 0 {
            MAX_PAGE_SIZE
        } else {
            requested_last.min(MAX_PAGE_SIZE)
        };
        let filter = filter.unwrap_or_else(AgentFilter::none);
        self.repository
            .find_agents_before(principal_id, project_id, last, before, &filter)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubRepository(Option<AgentDirectoryProject>);

    #[async_trait]
    impl ProjectAgentDirectoryRepository for StubRepository {
        async fn find_project(
            &self,
            _principal_id: Uuid,
            _project_id: Uuid,
        ) -> Result<Option<AgentDirectoryProject>, RepositoryError> {
            Ok(self.0.clone())
        }

        async fn find_agents(
            &self,
            _principal_id: Uuid,
            _project_id: Uuid,
            _first: i64,
            _after: Option<&str>,
            _filter: &AgentFilter,
        ) -> Result<AgentPage, RepositoryError> {
            unimplemented!()
        }

        async fn find_agents_before(
            &self,
            _principal_id: Uuid,
            _project_id: Uuid,
            _last: i64,
            _before: Option<&str>,
            _filter: &AgentFilter,
        ) -> Result<AgentPage, RepositoryError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn an_unparseable_project_id_is_not_found_rather_than_an_error() {
        let service = ProjectAgentDirectoryQueryService::new(StubRepository(None));
        let result = service
            .find_project(Uuid::new_v4(), "not-a-uuid")
            .await
            .unwrap();
        assert!(result.is_none());
    }
}
