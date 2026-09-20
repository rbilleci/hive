use crate::organization::project_directory::{OrganizationProjectFilter, OrganizationProjectPage};
use async_trait::async_trait;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("Invalid project cursor.")]
    InvalidCursor,
    #[error("The project cursor does not match the directory filters.")]
    CursorFilterMismatch,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Port `JpaOrganizationProjectDirectoryRepository` implements.
#[async_trait]
pub trait OrganizationProjectDirectoryRepository: Send + Sync {
    async fn find_projects(
        &self,
        principal_id: Uuid,
        organization_id: Uuid,
        first: i64,
        after: Option<&str>,
        filter: &OrganizationProjectFilter,
    ) -> Result<OrganizationProjectPage, RepositoryError>;

    async fn find_projects_before(
        &self,
        principal_id: Uuid,
        organization_id: Uuid,
        last: i64,
        before: Option<&str>,
        filter: &OrganizationProjectFilter,
    ) -> Result<OrganizationProjectPage, RepositoryError>;
}

const MAX_PAGE_SIZE: i64 = 50;

/// Transport-neutral use case for one member-scoped, keyset-paged project
/// directory. Ports `OrganizationProjectDirectoryQueryService`.
pub struct OrganizationProjectDirectoryQueryService<R: OrganizationProjectDirectoryRepository> {
    repository: R,
}

impl<R: OrganizationProjectDirectoryRepository> OrganizationProjectDirectoryQueryService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub async fn find_projects(
        &self,
        principal_id: Uuid,
        organization_id: Uuid,
        requested_first: i64,
        after: Option<&str>,
        filter: Option<OrganizationProjectFilter>,
    ) -> Result<OrganizationProjectPage, RepositoryError> {
        let first = if requested_first <= 0 {
            MAX_PAGE_SIZE
        } else {
            requested_first.min(MAX_PAGE_SIZE)
        };
        let filter = filter.unwrap_or_else(OrganizationProjectFilter::none);
        self.repository
            .find_projects(principal_id, organization_id, first, after, &filter)
            .await
    }

    pub async fn find_projects_before(
        &self,
        principal_id: Uuid,
        organization_id: Uuid,
        requested_last: i64,
        before: Option<&str>,
        filter: Option<OrganizationProjectFilter>,
    ) -> Result<OrganizationProjectPage, RepositoryError> {
        let last = if requested_last <= 0 {
            MAX_PAGE_SIZE
        } else {
            requested_last.min(MAX_PAGE_SIZE)
        };
        let filter = filter.unwrap_or_else(OrganizationProjectFilter::none);
        self.repository
            .find_projects_before(principal_id, organization_id, last, before, &filter)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingRepository {
        last_first_or_last: AtomicI64,
        page: Mutex<Option<OrganizationProjectPage>>,
    }

    fn empty_page() -> OrganizationProjectPage {
        OrganizationProjectPage {
            projects: vec![],
            start_cursor: None,
            end_cursor: None,
            has_previous_page: false,
            has_next_page: false,
            total_count: 0,
        }
    }

    #[async_trait]
    impl OrganizationProjectDirectoryRepository for RecordingRepository {
        async fn find_projects(
            &self,
            _principal_id: Uuid,
            _organization_id: Uuid,
            first: i64,
            _after: Option<&str>,
            _filter: &OrganizationProjectFilter,
        ) -> Result<OrganizationProjectPage, RepositoryError> {
            self.last_first_or_last.store(first, Ordering::SeqCst);
            Ok(self.page.lock().unwrap().take().unwrap_or_else(empty_page))
        }

        async fn find_projects_before(
            &self,
            _principal_id: Uuid,
            _organization_id: Uuid,
            last: i64,
            _before: Option<&str>,
            _filter: &OrganizationProjectFilter,
        ) -> Result<OrganizationProjectPage, RepositoryError> {
            self.last_first_or_last.store(last, Ordering::SeqCst);
            Ok(self.page.lock().unwrap().take().unwrap_or_else(empty_page))
        }
    }

    #[tokio::test]
    async fn forward_page_size_is_clamped_to_the_maximum() {
        let repository = RecordingRepository::default();
        let service = OrganizationProjectDirectoryQueryService::new(repository);
        service
            .find_projects(Uuid::new_v4(), Uuid::new_v4(), 500, None, None)
            .await
            .unwrap();
        assert_eq!(
            service.repository.last_first_or_last.load(Ordering::SeqCst),
            MAX_PAGE_SIZE
        );
    }

    #[tokio::test]
    async fn backward_page_size_is_clamped_to_the_maximum() {
        let repository = RecordingRepository::default();
        let service = OrganizationProjectDirectoryQueryService::new(repository);
        service
            .find_projects_before(Uuid::new_v4(), Uuid::new_v4(), 0, None, None)
            .await
            .unwrap();
        assert_eq!(
            service.repository.last_first_or_last.load(Ordering::SeqCst),
            MAX_PAGE_SIZE
        );
    }

    #[tokio::test]
    async fn an_in_bounds_request_passes_through_unchanged() {
        let repository = RecordingRepository::default();
        let service = OrganizationProjectDirectoryQueryService::new(repository);
        service
            .find_projects(Uuid::new_v4(), Uuid::new_v4(), 5, None, None)
            .await
            .unwrap();
        assert_eq!(
            service.repository.last_first_or_last.load(Ordering::SeqCst),
            5
        );
    }
}
