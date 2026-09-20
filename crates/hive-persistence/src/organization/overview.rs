//! Ports `JpaOrganizationOverviewRepository`, replicating the Hibernate
//! `tenantScope` filter for a single primary-key lookup: `entityManager.find` still
//! applies the enabled filter, so the SQL adds the same active-membership `EXISTS`
//! predicate `accessible_organization.rs` uses.

use hive_application::organization::{
    OrganizationOverview, OrganizationOverviewRepository,
    OrganizationOverviewRepositoryError as RepositoryError,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub struct PgOrganizationOverviewRepository {
    pool: PgPool,
}

impl PgOrganizationOverviewRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl OrganizationOverviewRepository for PgOrganizationOverviewRepository {
    async fn find_organization(
        &self,
        principal_id: Uuid,
        organization_id: Uuid,
    ) -> Result<Option<OrganizationOverview>, RepositoryError> {
        let row = sqlx::query(
            "SELECT o.id, o.slug, o.display_name, o.lifecycle_status FROM organizations o \
             WHERE o.id = $1 AND EXISTS (\
               SELECT 1 FROM organization_memberships tenant_membership \
               WHERE tenant_membership.organization_id = o.id \
                 AND tenant_membership.principal_id = $2 \
                 AND tenant_membership.started_at <= CURRENT_TIMESTAMP \
                 AND tenant_membership.ended_at IS NULL)",
        )
        .bind(organization_id)
        .bind(principal_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;

        Ok(row.map(|row| OrganizationOverview {
            id: row.get("id"),
            slug: row.get("slug"),
            display_name: row.get("display_name"),
            lifecycle_status: row.get("lifecycle_status"),
        }))
    }
}
