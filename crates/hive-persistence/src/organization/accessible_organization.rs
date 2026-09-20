//! Ports `JpaAccessibleOrganizationRepository`, replicating the Hibernate
//! `tenantScope` filter Hibernate injects transparently into both the entity query
//! and the count query: `OrganizationReadEntity`'s `@Filter` restricts rows to
//! organizations where the principal holds an active (`started_at <= now`,
//! `ended_at IS NULL`) `organization_memberships` row. Aurora DSQL's row-value
//! comparison support matches PostgreSQL's, so the keyset predicate
//! `(display_name, id) > (?, ?)` is the same expression on both dialects.

use hive_application::organization::{
    AccessibleOrganization, AccessibleOrganizationCursor, AccessibleOrganizationPage,
    AccessibleOrganizationRepository, AccessibleOrganizationRepositoryError as RepositoryError,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub struct PgAccessibleOrganizationRepository {
    pool: PgPool,
}

impl PgAccessibleOrganizationRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const TENANT_MEMBERSHIP_EXISTS: &str = "EXISTS (\
    SELECT 1 FROM organization_memberships tenant_membership \
    WHERE tenant_membership.organization_id = o.id \
      AND tenant_membership.principal_id = $1 \
      AND tenant_membership.started_at <= CURRENT_TIMESTAMP \
      AND tenant_membership.ended_at IS NULL)";

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
        let rows = match &cursor {
            None => sqlx::query(&format!(
                "SELECT o.id, o.slug, o.display_name, o.lifecycle_status FROM organizations o \
                 WHERE ($2 OR o.lifecycle_status <> 'ARCHIVED') AND {TENANT_MEMBERSHIP_EXISTS} \
                 ORDER BY o.display_name, o.id LIMIT $3"
            ))
            .bind(principal_id)
            .bind(include_archived)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?,
            Some(cursor) => sqlx::query(&format!(
                "SELECT o.id, o.slug, o.display_name, o.lifecycle_status FROM organizations o \
                 WHERE ($2 OR o.lifecycle_status <> 'ARCHIVED') AND {TENANT_MEMBERSHIP_EXISTS} \
                 AND (o.display_name, o.id) > ($4, $5) \
                 ORDER BY o.display_name, o.id LIMIT $3"
            ))
            .bind(principal_id)
            .bind(include_archived)
            .bind(limit)
            .bind(&cursor.display_name)
            .bind(cursor.id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?,
        };

        let mut organizations: Vec<AccessibleOrganization> = rows
            .iter()
            .map(|row| AccessibleOrganization {
                id: row.get("id"),
                slug: row.get("slug"),
                display_name: row.get("display_name"),
                lifecycle_status: row.get("lifecycle_status"),
            })
            .collect();

        let has_next_page = organizations.len() as i64 > first;
        if has_next_page {
            organizations.pop();
        }
        let end_cursor = organizations
            .last()
            .map(AccessibleOrganizationCursor::encode);

        let total_count: i64 = sqlx::query(&format!(
            "SELECT count(*) FROM organizations o \
             WHERE ($2 OR o.lifecycle_status <> 'ARCHIVED') AND {TENANT_MEMBERSHIP_EXISTS}"
        ))
        .bind(principal_id)
        .bind(include_archived)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
        .get(0);

        Ok(AccessibleOrganizationPage {
            organizations,
            end_cursor,
            has_next_page,
            total_count,
        })
    }
}
