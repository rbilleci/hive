use crate::organization::accessible_organization::AccessibleOrganizationPage;
use async_trait::async_trait;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("Invalid selector cursor.")]
    InvalidCursor,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Port `JpaAccessibleOrganizationRepository` implements. `hive-persistence` owns
/// the real SQL; a fake in `hive-application`'s own tests exercises
/// `AccessibleOrganizationQueryService`'s clamping logic without a database.
#[async_trait]
pub trait AccessibleOrganizationRepository: Send + Sync {
    async fn find_accessible_organizations(
        &self,
        principal_id: Uuid,
        include_archived: bool,
        first: i64,
        after: Option<&str>,
    ) -> Result<AccessibleOrganizationPage, RepositoryError>;
}
