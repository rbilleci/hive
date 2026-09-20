//! Ports `JpaOrganizationOverviewRepository`, replicating the Hibernate
//! `tenantScope` filter for a single primary-key lookup: `entityManager.find` still
//! applies the enabled filter, so the SQL adds the same active-membership `EXISTS`
//! predicate `accessible_organization.rs` uses.
//!
//! `GSR-PERSISTENCE`: runs through `sea_orm::ConnectionTrait` via
//! `Statement::from_sql_and_values` + `query_one_raw`, preserving the original SQL text verbatim
//! (same idiom as `capability`/`console`, `GSR-PHASE-P5`).

use hive_application::organization::{
    OrganizationOverview, OrganizationOverviewRepository,
    OrganizationOverviewRepositoryError as RepositoryError,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use uuid::Uuid;

pub struct PgOrganizationOverviewRepository {
    db: DatabaseConnection,
}

impl PgOrganizationOverviewRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl OrganizationOverviewRepository for PgOrganizationOverviewRepository {
    async fn find_organization(
        &self,
        principal_id: Uuid,
        organization_id: Uuid,
    ) -> Result<Option<OrganizationOverview>, RepositoryError> {
        let statement = Statement::from_sql_and_values(
            self.db.get_database_backend(),
            "SELECT o.id, o.slug, o.display_name, o.lifecycle_status FROM organizations o \
             WHERE o.id = $1 AND EXISTS (\
               SELECT 1 FROM organization_memberships tenant_membership \
               WHERE tenant_membership.organization_id = o.id \
                 AND tenant_membership.principal_id = $2 \
                 AND tenant_membership.started_at <= CURRENT_TIMESTAMP \
                 AND tenant_membership.ended_at IS NULL)",
            [organization_id.into(), principal_id.into()],
        );
        let row = self
            .db
            .query_one_raw(statement)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;

        row.map(|row| {
            Ok(OrganizationOverview {
                id: row.try_get_by("id")?,
                slug: row.try_get_by("slug")?,
                display_name: row.try_get_by("display_name")?,
                lifecycle_status: row.try_get_by("lifecycle_status")?,
            })
        })
        .transpose()
        .map_err(|error: sea_orm::DbErr| RepositoryError::Other(error.into()))
    }
}
