//! Ports `JpaAccessibleOrganizationRepository`, replicating the Hibernate
//! `tenantScope` filter Hibernate injects transparently into both the entity query
//! and the count query: `OrganizationReadEntity`'s `@Filter` restricts rows to
//! organizations where the principal holds an active (`started_at <= now`,
//! `ended_at IS NULL`) `organization_memberships` row. Aurora DSQL's row-value
//! comparison support matches PostgreSQL's, so the keyset predicate
//! `(display_name, id) > (?, ?)` is the same expression on both dialects.
//!
//! `GSR-PERSISTENCE`: runs through `sea_orm::ConnectionTrait` via
//! `Statement::from_sql_and_values` + `query_all_raw`/`query_one_raw`, preserving the original SQL
//! text verbatim (same idiom as `capability`/`console`/`overview.rs`, `GSR-PHASE-P5`).

use hive_application::organization::{
    AccessibleOrganization, AccessibleOrganizationCursor, AccessibleOrganizationPage,
    AccessibleOrganizationRepository, AccessibleOrganizationRepositoryError as RepositoryError,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use uuid::Uuid;

pub struct PgAccessibleOrganizationRepository {
    db: DatabaseConnection,
}

impl PgAccessibleOrganizationRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

const TENANT_MEMBERSHIP_EXISTS: &str = "EXISTS (\
    SELECT 1 FROM organization_memberships tenant_membership \
    WHERE tenant_membership.organization_id = o.id \
      AND tenant_membership.principal_id = $1 \
      AND tenant_membership.started_at <= CURRENT_TIMESTAMP \
      AND tenant_membership.ended_at IS NULL)";

fn other(error: sea_orm::DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

#[async_trait::async_trait]
impl AccessibleOrganizationRepository for PgAccessibleOrganizationRepository {
    async fn find_accessible_organizations(
        &self,
        principal_id: Uuid,
        include_archived: bool,
        first: i64,
        after: Option<&str>,
    ) -> Result<AccessibleOrganizationPage, RepositoryError> {
        let cursor = after
            .map(AccessibleOrganizationCursor::decode)
            .transpose()
            .map_err(|_| RepositoryError::InvalidCursor)?;

        let limit = first + 1;
        let backend = self.db.get_database_backend();
        let statement = match &cursor {
            None => Statement::from_sql_and_values(
                backend,
                format!(
                    "SELECT o.id, o.slug, o.display_name, o.lifecycle_status FROM organizations o \
                     WHERE ($2 OR o.lifecycle_status <> 'ARCHIVED') AND {TENANT_MEMBERSHIP_EXISTS} \
                     ORDER BY o.display_name, o.id LIMIT $3"
                ),
                [principal_id.into(), include_archived.into(), limit.into()],
            ),
            Some(cursor) => Statement::from_sql_and_values(
                backend,
                format!(
                    "SELECT o.id, o.slug, o.display_name, o.lifecycle_status FROM organizations o \
                     WHERE ($2 OR o.lifecycle_status <> 'ARCHIVED') AND {TENANT_MEMBERSHIP_EXISTS} \
                     AND (o.display_name, o.id) > ($4, $5) \
                     ORDER BY o.display_name, o.id LIMIT $3"
                ),
                [
                    principal_id.into(),
                    include_archived.into(),
                    limit.into(),
                    cursor.display_name.clone().into(),
                    cursor.id.into(),
                ],
            ),
        };
        let rows = self.db.query_all_raw(statement).await.map_err(other)?;

        let mut organizations: Vec<AccessibleOrganization> = rows
            .iter()
            .map(|row| {
                Ok(AccessibleOrganization {
                    id: row.try_get_by("id")?,
                    slug: row.try_get_by("slug")?,
                    display_name: row.try_get_by("display_name")?,
                    lifecycle_status: row.try_get_by("lifecycle_status")?,
                })
            })
            .collect::<Result<_, sea_orm::DbErr>>()
            .map_err(other)?;

        let has_next_page = organizations.len() as i64 > first;
        if has_next_page {
            organizations.pop();
        }
        let end_cursor = organizations
            .last()
            .map(AccessibleOrganizationCursor::encode);

        let count_statement = Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT count(*) AS total_count FROM organizations o \
                 WHERE ($2 OR o.lifecycle_status <> 'ARCHIVED') AND {TENANT_MEMBERSHIP_EXISTS}"
            ),
            [principal_id.into(), include_archived.into()],
        );
        let total_count: i64 = self
            .db
            .query_one_raw(count_statement)
            .await
            .map_err(other)?
            .expect("count(*) always returns exactly one row")
            .try_get_by("total_count")
            .map_err(other)?;

        Ok(AccessibleOrganizationPage {
            organizations,
            end_cursor,
            has_next_page,
            total_count,
        })
    }
}
