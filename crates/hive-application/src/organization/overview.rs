use uuid::Uuid;

/// Membership-scoped organization summary used by the read-only overview. Ports
/// `OrganizationOverview`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationOverview {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Port `JpaOrganizationOverviewRepository` implements.
#[async_trait::async_trait]
pub trait OrganizationOverviewRepository: Send + Sync {
    async fn find_organization(
        &self,
        principal_id: Uuid,
        organization_id: Uuid,
    ) -> Result<Option<OrganizationOverview>, RepositoryError>;
}

/// Transport-neutral read use case for one organization visible to the verified
/// principal. Ports `OrganizationOverviewQueryService`. `requested_organization_id`
/// stays a raw string, exactly as the GraphQL `ID!` argument arrives: an
/// unparseable value is indistinguishable not-found, not an error, matching
/// `findOrganization`'s `catch (IllegalArgumentException) { return Optional.empty(); }`.
pub struct OrganizationOverviewQueryService<R: OrganizationOverviewRepository> {
    repository: R,
}

impl<R: OrganizationOverviewRepository> OrganizationOverviewQueryService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub async fn find_organization(
        &self,
        principal_id: Uuid,
        requested_organization_id: &str,
    ) -> Result<Option<OrganizationOverview>, RepositoryError> {
        let Ok(organization_id) = Uuid::parse_str(requested_organization_id) else {
            return Ok(None);
        };
        self.repository
            .find_organization(principal_id, organization_id)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubRepository(Option<OrganizationOverview>);

    #[async_trait::async_trait]
    impl OrganizationOverviewRepository for StubRepository {
        async fn find_organization(
            &self,
            _principal_id: Uuid,
            _organization_id: Uuid,
        ) -> Result<Option<OrganizationOverview>, RepositoryError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn an_unparseable_id_is_not_found_rather_than_an_error() {
        let service = OrganizationOverviewQueryService::new(StubRepository(None));
        let result = service
            .find_organization(Uuid::new_v4(), "not-a-uuid")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn a_valid_id_reaches_the_repository() {
        let organization = OrganizationOverview {
            id: Uuid::new_v4(),
            slug: "product".to_string(),
            display_name: "Product".to_string(),
            lifecycle_status: "ACTIVE".to_string(),
        };
        let service =
            OrganizationOverviewQueryService::new(StubRepository(Some(organization.clone())));
        let result = service
            .find_organization(Uuid::new_v4(), &organization.id.to_string())
            .await
            .unwrap();
        assert_eq!(result, Some(organization));
    }
}
