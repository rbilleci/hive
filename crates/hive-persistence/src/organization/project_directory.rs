//! Ports `JpaOrganizationProjectDirectoryRepository`: forward (`first`/`after`) and
//! backward (`last`/`before`) keyset pagination over one organization's projects,
//! both filtered by the same tenant `EXISTS` predicate `ProjectReadEntity`'s
//! `@Filter` applies, and both honoring the optional `lifecycleStatus` equality and
//! `search` `ILIKE`-equivalent (`lower(...) LIKE ... ESCAPE '\'`) predicates.
//!
//! `GSR-PERSISTENCE`: runs through `sea_orm::ConnectionTrait` via
//! `Statement::from_sql_and_values` + `query_all_raw`/`query_one_raw`, preserving the original SQL
//! text and its dynamic-parameter-count shape verbatim (same idiom as `capability`/`console`,
//! `GSR-PHASE-P5`) — the bind list is built as a `Vec<sea_orm::Value>` in the same conditional
//! order the original `sqlx` query builder chained `.bind()` calls in, rather than a fixed-arity
//! call.

use hive_application::organization::{
    OrganizationProject, OrganizationProjectCursor, OrganizationProjectCursorError,
    OrganizationProjectDirectoryRepository,
    OrganizationProjectDirectoryRepositoryError as RepositoryError, OrganizationProjectFilter,
    OrganizationProjectPage,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, Value};
use uuid::Uuid;

pub struct PgOrganizationProjectDirectoryRepository {
    db: DatabaseConnection,
}

impl PgOrganizationProjectDirectoryRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
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

fn other(error: sea_orm::DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

/// Shared implementation: builds the dynamic WHERE clause (organization + tenant +
/// optional lifecycle/search/cursor predicates), runs the bounded `LIMIT count + 1`
/// query in the requested direction's order, and the matching total count.
async fn run(
    db: &DatabaseConnection,
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
    // `totalCount` describes the filtered list, not the rows that remain after the cursor.
    let count_sql = format!("SELECT count(*) AS total_count FROM projects p WHERE {where_clause}");
    if cursor.is_some() {
        where_clause.push_str(&format!(
            " AND (p.display_name, p.id) {comparison} (${next_param}, ${})",
            next_param + 1
        ));
        next_param += 2;
    }
    let limit_param = next_param;

    let list_sql = format!(
        "SELECT p.id, p.slug, p.display_name, p.lifecycle_status FROM projects p WHERE {where_clause} ORDER BY {order} LIMIT ${limit_param}"
    );

    let mut list_values: Vec<Value> = vec![organization_id.into(), principal_id.into()];
    let mut count_values: Vec<Value> = vec![organization_id.into(), principal_id.into()];
    if let Some(status) = filter.lifecycle_status() {
        list_values.push(status.into());
        count_values.push(status.into());
    }
    if filter.search().is_some() {
        let pattern = filter.literal_search_pattern();
        list_values.push(pattern.clone().into());
        count_values.push(pattern.into());
    }
    if let Some(cursor) = &cursor {
        list_values.push(cursor.display_name.clone().into());
        list_values.push(cursor.id.into());
    }
    list_values.push((count + 1).into());

    let backend = db.get_database_backend();
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            &list_sql,
            list_values,
        ))
        .await
        .map_err(other)?;
    let total_count: i64 = db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            &count_sql,
            count_values,
        ))
        .await
        .map_err(other)?
        .expect("count(*) always returns exactly one row")
        .try_get_by("total_count")
        .map_err(other)?;

    let mut projects: Vec<OrganizationProject> = rows
        .iter()
        .map(|row| {
            Ok(OrganizationProject {
                id: row.try_get_by("id")?,
                slug: row.try_get_by("slug")?,
                display_name: row.try_get_by("display_name")?,
                lifecycle_status: row.try_get_by("lifecycle_status")?,
            })
        })
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(other)?;
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
            &self.db,
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
            &self.db,
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
