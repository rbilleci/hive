use crate::organization::accessible_organization::AccessibleOrganizationPage;
use crate::organization::repository::{AccessibleOrganizationRepository, RepositoryError};
use uuid::Uuid;

const MAX_PAGE_SIZE: i64 = 50;

/// Transport-neutral selector use case. Ports `AccessibleOrganizationQueryService`.
pub struct AccessibleOrganizationQueryService<R: AccessibleOrganizationRepository> {
    repository: R,
}

impl<R: AccessibleOrganizationRepository> AccessibleOrganizationQueryService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub async fn query(
        &self,
        principal_id: Uuid,
        include_archived: bool,
        requested_first: i64,
        after: Option<&str>,
    ) -> Result<AccessibleOrganizationPage, RepositoryError> {
        let first = if requested_first <= 0 {
            MAX_PAGE_SIZE
        } else {
            requested_first.min(MAX_PAGE_SIZE)
        };
        self.repository
            .find_accessible_organizations(principal_id, include_archived, first, after)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::organization::accessible_organization::AccessibleOrganization;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingRepository {
        last_first: AtomicI64,
        page: Mutex<Option<AccessibleOrganizationPage>>,
    }

    #[async_trait]
    impl AccessibleOrganizationRepository for RecordingRepository {
        async fn find_accessible_organizations(
            &self,
            _principal_id: Uuid,
            _include_archived: bool,
            first: i64,
            _after: Option<&str>,
        ) -> Result<AccessibleOrganizationPage, RepositoryError> {
            self.last_first.store(first, Ordering::SeqCst);
            Ok(self
                .page
                .lock()
                .unwrap()
                .take()
                .unwrap_or(AccessibleOrganizationPage {
                    organizations: vec![],
                    end_cursor: None,
                    has_next_page: false,
                    total_count: 0,
                }))
        }
    }

    fn principal() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
    }

    #[tokio::test]
    async fn a_nonpositive_requested_first_becomes_the_max_page_size() {
        let repository = RecordingRepository::default();
        let service = AccessibleOrganizationQueryService::new(repository);
        service.query(principal(), false, 0, None).await.unwrap();
        assert_eq!(
            service.repository.last_first.load(Ordering::SeqCst),
            MAX_PAGE_SIZE
        );

        service.query(principal(), false, -5, None).await.unwrap();
        assert_eq!(
            service.repository.last_first.load(Ordering::SeqCst),
            MAX_PAGE_SIZE
        );
    }

    #[tokio::test]
    async fn a_requested_first_above_the_max_is_clamped() {
        let repository = RecordingRepository::default();
        let service = AccessibleOrganizationQueryService::new(repository);
        service.query(principal(), false, 500, None).await.unwrap();
        assert_eq!(
            service.repository.last_first.load(Ordering::SeqCst),
            MAX_PAGE_SIZE
        );
    }

    #[tokio::test]
    async fn a_requested_first_within_bounds_passes_through_unchanged() {
        let repository = RecordingRepository::default();
        let service = AccessibleOrganizationQueryService::new(repository);
        service.query(principal(), false, 7, None).await.unwrap();
        assert_eq!(service.repository.last_first.load(Ordering::SeqCst), 7);
    }

    #[tokio::test]
    async fn returns_the_repository_page_unchanged() {
        let organization = AccessibleOrganization {
            id: principal(),
            slug: "product".to_string(),
            display_name: "Alpha Command".to_string(),
            lifecycle_status: "ACTIVE".to_string(),
        };
        let repository = RecordingRepository {
            page: Mutex::new(Some(AccessibleOrganizationPage {
                organizations: vec![organization.clone()],
                end_cursor: Some("cursor".to_string()),
                has_next_page: true,
                total_count: 1,
            })),
            ..Default::default()
        };
        let service = AccessibleOrganizationQueryService::new(repository);
        let page = service.query(principal(), false, 10, None).await.unwrap();
        assert_eq!(page.organizations, vec![organization]);
        assert!(page.has_next_page);
        assert_eq!(page.total_count, 1);
    }
}
