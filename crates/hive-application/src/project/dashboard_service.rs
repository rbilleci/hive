use crate::project::dashboard::ProjectDashboard;
use async_trait::async_trait;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Port `PostgresProjectDashboardRepository` implements.
#[async_trait]
pub trait ProjectDashboardRepository: Send + Sync {
    async fn find_dashboard(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
    ) -> Result<Option<ProjectDashboard>, RepositoryError>;
}

/// Transport-neutral member-scoped dashboard lookup with indistinguishable absent
/// targets. Ports `ProjectDashboardQueryService`.
pub struct ProjectDashboardQueryService<R: ProjectDashboardRepository> {
    repository: R,
}

impl<R: ProjectDashboardRepository> ProjectDashboardQueryService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    /// An unparseable `requested_project_id` is not found, not an error, matching
    /// `findDashboard`'s `catch (IllegalArgumentException) { return Optional.empty(); }`.
    pub async fn find_dashboard(
        &self,
        principal_id: Uuid,
        requested_project_id: &str,
    ) -> Result<Option<ProjectDashboard>, RepositoryError> {
        let Ok(project_id) = Uuid::parse_str(requested_project_id) else {
            return Ok(None);
        };
        self.repository
            .find_dashboard(principal_id, project_id)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubRepository;

    #[async_trait]
    impl ProjectDashboardRepository for StubRepository {
        async fn find_dashboard(
            &self,
            _principal_id: Uuid,
            _project_id: Uuid,
        ) -> Result<Option<ProjectDashboard>, RepositoryError> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn an_unparseable_project_id_is_not_found_rather_than_an_error() {
        let service = ProjectDashboardQueryService::new(StubRepository);
        let result = service
            .find_dashboard(Uuid::new_v4(), "not-a-uuid")
            .await
            .unwrap();
        assert!(result.is_none());
    }
}
