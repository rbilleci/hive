//! Ports `JpaOrganizationProjectDirectoryRepository`: forward (`first`/`after`) and
//! backward (`last`/`before`) keyset pagination over one organization's projects,
//! both filtered by the same tenant `EXISTS` predicate `ProjectReadEntity`'s
//! `@Filter` applies, and both honoring the optional `lifecycleStatus` equality and
//! `search` `ILIKE`-equivalent (`lower(...) LIKE ... ESCAPE '\'`) predicates.

use hive_application::organization::{
    OrganizationProject, OrganizationProjectCursor, OrganizationProjectCursorError,
    OrganizationProjectDirectoryRepository,
    OrganizationProjectDirectoryRepositoryError as RepositoryError, OrganizationProjectFilter,
    OrganizationProjectPage,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub struct PgOrganizationProjectDirectoryRepository {
    pool: PgPool,
}

impl PgOrganizationProjectDirectoryRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const TENANT_EXISTS: &str = "EXISTS (\
    SELECT 1 FROM organization_memberships tenant_membership \
    WHERE tenant_membership.organization_id = p.organization_id \
      AND tenant_membership.principal_id = $2 \
      AND tenant_membership.started_at <= CURRENT_TIMESTAMP \
      AND tenant_membership.ended_at IS NULL)";

enum Direction {
    Forward,
    Backward,
}

fn cursor_error(error: OrganizationProjectCursorError) -> RepositoryError {
    match error {
        OrganizationProjectCursorError::Invalid(_) => RepositoryError::InvalidCursor,
        OrganizationProjectCursorError::FilterMismatch => RepositoryError::CursorFilterMismatch,
    }
}

/// Shared implementation: builds the dynamic WHERE clause (organization + tenant +
/// optional lifecycle/search/cursor predicates), runs the bounded `LIMIT count + 1`
/// query in the requested direction's order, and the matching total count.
async fn run(
    pool: &PgPool,
    direction: Direction,
    principal_id: Uuid,
    organization_id: Uuid,
    count: i64,
    cursor_text: Option<&str>,
    filter: &OrganizationProjectFilter,
) -> Result<(Vec<OrganizationProject>, bool, i64), RepositoryError> {
    let cursor = cursor_text
        .map(|text| OrganizationProjectCursor::decode(text, filter))
        .transpose()
        .map_err(cursor_error)?;

    let comparison = match direction {
        Direction::Forward => ">",
        Direction::Backward => "<",
    };
    let order = match direction {
        Direction::Forward => "p.display_name, p.id",
        Direction::Backward => "p.display_name DESC, p.id DESC",
    };

    let mut where_clause = format!("p.organization_id = $1 AND {TENANT_EXISTS}");
    let mut next_param = 3;
    if filter.lifecycle_status().is_some() {
        where_clause.push_str(&format!(" AND p.lifecycle_status = ${next_param}"));
        next_param += 1;
    }
    if filter.search().is_some() {
        where_clause.push_str(&format!(
            " AND lower(p.display_name) LIKE ${next_param} ESCAPE '\\'"
        ));
        next_param += 1;
    }
    if cursor.is_some() {
        where_clause.push_str(&format!(
            " AND (p.display_name, p.id) {comparison} (${next_param}, ${})",
            next_param + 1
        ));
        next_param += 2;
    }
    let limit_param = next_param;

    let list_sql =
        format!("SELECT p.id, p.slug, p.display_name, p.lifecycle_status FROM projects p WHERE {where_clause} ORDER BY {order} LIMIT ${limit_param}");
    let count_sql = format!("SELECT count(*) FROM projects p WHERE {where_clause}");

    let mut list_query = sqlx::query(&list_sql)
        .bind(organization_id)
        .bind(principal_id);
    let mut count_query = sqlx::query(&count_sql)
        .bind(organization_id)
        .bind(principal_id);
    if let Some(status) = filter.lifecycle_status() {
        list_query = list_query.bind(status);
        count_query = count_query.bind(status);
    }
    if filter.search().is_some() {
        let pattern = filter.literal_search_pattern();
        list_query = list_query.bind(pattern.clone());
        count_query = count_query.bind(pattern);
    }
    if let Some(cursor) = &cursor {
        list_query = list_query.bind(cursor.display_name.clone()).bind(cursor.id);
        count_query = count_query
            .bind(cursor.display_name.clone())
            .bind(cursor.id);
    }
    list_query = list_query.bind(count + 1);

    let rows = list_query
        .fetch_all(pool)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let total_count: i64 = count_query
        .fetch_one(pool)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
        .get(0);

    let mut projects: Vec<OrganizationProject> = rows
        .iter()
        .map(|row| OrganizationProject {
            id: row.get("id"),
            slug: row.get("slug"),
            display_name: row.get("display_name"),
            lifecycle_status: row.get("lifecycle_status"),
        })
        .collect();
    let overflow = projects.len() as i64 > count;
    if overflow {
        projects.pop();
    }
    Ok((projects, overflow, total_count))
}

fn cursors(
    projects: &[OrganizationProject],
    filter: &OrganizationProjectFilter,
) -> (Option<String>, Option<String>) {
    let start = projects
        .first()
        .map(|project| OrganizationProjectCursor::encode(project, filter));
    let end = projects
        .last()
        .map(|project| OrganizationProjectCursor::encode(project, filter));
    (start, end)
}

#[async_trait::async_trait]
impl OrganizationProjectDirectoryRepository for PgOrganizationProjectDirectoryRepository {
    async fn find_projects(
        &self,
        principal_id: Uuid,
        organization_id: Uuid,
        first: i64,
        after: Option<&str>,
        filter: &OrganizationProjectFilter,
    ) -> Result<OrganizationProjectPage, RepositoryError> {
        let (projects, has_next_page, total_count) = run(
            &self.pool,
            Direction::Forward,
            principal_id,
            organization_id,
            first,
            after,
            filter,
        )
        .await?;
        let (start_cursor, end_cursor) = cursors(&projects, filter);
        Ok(OrganizationProjectPage {
            projects,
            start_cursor,
            end_cursor,
            has_previous_page: after.is_some(),
            has_next_page,
            total_count,
        })
    }

    async fn find_projects_before(
        &self,
        principal_id: Uuid,
        organization_id: Uuid,
        last: i64,
        before: Option<&str>,
        filter: &OrganizationProjectFilter,
    ) -> Result<OrganizationProjectPage, RepositoryError> {
        let (mut projects, has_previous_page, total_count) = run(
            &self.pool,
            Direction::Backward,
            principal_id,
            organization_id,
            last,
            before,
            filter,
        )
        .await?;
        projects.reverse();
        let (start_cursor, end_cursor) = cursors(&projects, filter);
        Ok(OrganizationProjectPage {
            projects,
            start_cursor,
            end_cursor,
            has_previous_page,
            has_next_page: before.is_some(),
            total_count,
        })
    }
}
