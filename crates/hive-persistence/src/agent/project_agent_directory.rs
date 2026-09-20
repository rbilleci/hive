//! Ports `JpaProjectAgentDirectoryRepository`: the tenant-scoped project lookup,
//! the case-insensitive-ordered (`lower(display_name)`) bidirectional agent
//! directory, and the batched "latest published version" lookup keyed by the
//! already-tenant-filtered agent id set (no separate tenant recheck needed there:
//! the id set fed into it is already exactly the visible rows).

use hive_application::agent::{
    Agent, AgentCursor, AgentCursorError, AgentDirectoryProject, AgentFilter, AgentPage,
    ProjectAgentDirectoryRepository, ProjectAgentDirectoryRepositoryError as RepositoryError,
};
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use uuid::Uuid;

pub struct PgProjectAgentDirectoryRepository {
    pool: PgPool,
}

impl PgProjectAgentDirectoryRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const TENANT_EXISTS: &str = "EXISTS (\
    SELECT 1 FROM projects tenant_project \
    JOIN organization_memberships tenant_membership \
      ON tenant_membership.organization_id = tenant_project.organization_id \
    WHERE tenant_project.id = a.project_id \
      AND tenant_membership.principal_id = $2 \
      AND tenant_membership.started_at <= CURRENT_TIMESTAMP \
      AND tenant_membership.ended_at IS NULL)";

enum Direction {
    Forward,
    Backward,
}

fn cursor_error(error: AgentCursorError) -> RepositoryError {
    match error {
        AgentCursorError::Invalid(_) => RepositoryError::InvalidCursor,
        AgentCursorError::FilterMismatch => RepositoryError::CursorFilterMismatch,
    }
}

async fn latest_versions(
    pool: &PgPool,
    agent_ids: &[Uuid],
) -> Result<HashMap<Uuid, (i32, Option<String>)>, RepositoryError> {
    if agent_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query(
        "SELECT v.agent_id, v.version_number, v.canonical_document FROM agent_versions v \
         WHERE v.agent_id = ANY($1) \
           AND v.version_number = (SELECT max(v2.version_number) FROM agent_versions v2 WHERE v2.agent_id = v.agent_id)",
    )
    .bind(agent_ids)
    .fetch_all(pool)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;

    let mut result = HashMap::new();
    for row in rows {
        let agent_id: Uuid = row.get("agent_id");
        let version_number: i64 = row.get("version_number");
        let document: Value = row.get("canonical_document");
        let model = document
            .get("model")
            .and_then(|m| m.get("reference"))
            .and_then(Value::as_str)
            .map(str::to_string);
        result.insert(agent_id, (version_number as i32, model));
    }
    Ok(result)
}

fn to_agent(row: &sqlx::postgres::PgRow, latest: Option<&(i32, Option<String>)>) -> Agent {
    Agent {
        id: row.get("id"),
        slug: row.get("slug"),
        display_name: row.get("display_name"),
        lifecycle_status: row.get("lifecycle_status"),
        latest_published_version: latest.map(|(version, _)| *version),
        model: latest.and_then(|(_, model)| model.clone()),
    }
}

async fn run(
    pool: &PgPool,
    direction: Direction,
    principal_id: Uuid,
    project_id: Uuid,
    count: i64,
    cursor_text: Option<&str>,
    filter: &AgentFilter,
) -> Result<(Vec<Agent>, bool, i64), RepositoryError> {
    let cursor = cursor_text
        .map(|text| AgentCursor::decode(text, filter))
        .transpose()
        .map_err(cursor_error)?;

    let comparison = match direction {
        Direction::Forward => ">",
        Direction::Backward => "<",
    };
    let order = match direction {
        Direction::Forward => "lower(a.display_name), a.id",
        Direction::Backward => "lower(a.display_name) DESC, a.id DESC",
    };

    let mut where_clause = format!("a.project_id = $1 AND {TENANT_EXISTS}");
    let mut next_param = 3;
    if filter.lifecycle_status().is_some() {
        where_clause.push_str(&format!(" AND a.lifecycle_status = ${next_param}"));
        next_param += 1;
    }
    if filter.search().is_some() {
        where_clause.push_str(&format!(
            " AND lower(a.display_name) LIKE ${next_param} ESCAPE '\\'"
        ));
        next_param += 1;
    }
    if cursor.is_some() {
        where_clause.push_str(&format!(
            " AND (lower(a.display_name), a.id) {comparison} (${next_param}, ${})",
            next_param + 1
        ));
        next_param += 2;
    }
    let limit_param = next_param;

    let list_sql =
        format!("SELECT a.id, a.slug, a.display_name, a.lifecycle_status FROM agents a WHERE {where_clause} ORDER BY {order} LIMIT ${limit_param}");
    let count_sql = format!("SELECT count(*) FROM agents a WHERE {where_clause}");

    let mut list_query = sqlx::query(&list_sql).bind(project_id).bind(principal_id);
    let mut count_query = sqlx::query(&count_sql).bind(project_id).bind(principal_id);
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
        list_query = list_query.bind(cursor.sort_name.clone()).bind(cursor.id);
        count_query = count_query.bind(cursor.sort_name.clone()).bind(cursor.id);
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

    let overflow = rows.len() as i64 > count;
    let bounded_rows = if overflow {
        &rows[..rows.len() - 1]
    } else {
        &rows[..]
    };
    let agent_ids: Vec<Uuid> = bounded_rows.iter().map(|row| row.get("id")).collect();
    let latest = latest_versions(pool, &agent_ids).await?;
    let agents: Vec<Agent> = bounded_rows
        .iter()
        .map(|row| to_agent(row, latest.get(&row.get::<Uuid, _>("id"))))
        .collect();

    Ok((agents, overflow, total_count))
}

fn cursors(agents: &[Agent], filter: &AgentFilter) -> (Option<String>, Option<String>) {
    (
        agents
            .first()
            .map(|agent| AgentCursor::encode(agent, filter)),
        agents
            .last()
            .map(|agent| AgentCursor::encode(agent, filter)),
    )
}

#[async_trait::async_trait]
impl ProjectAgentDirectoryRepository for PgProjectAgentDirectoryRepository {
    async fn find_project(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
    ) -> Result<Option<AgentDirectoryProject>, RepositoryError> {
        let row = sqlx::query(
            "SELECT p.id, p.slug, p.display_name, p.lifecycle_status FROM projects p \
             WHERE p.id = $1 AND EXISTS (\
               SELECT 1 FROM organization_memberships tenant_membership \
               WHERE tenant_membership.organization_id = p.organization_id \
                 AND tenant_membership.principal_id = $2 \
                 AND tenant_membership.started_at <= CURRENT_TIMESTAMP \
                 AND tenant_membership.ended_at IS NULL)",
        )
        .bind(project_id)
        .bind(principal_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;

        Ok(row.map(|row| AgentDirectoryProject {
            id: row.get("id"),
            slug: row.get("slug"),
            display_name: row.get("display_name"),
            lifecycle_status: row.get("lifecycle_status"),
        }))
    }

    async fn find_agents(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        first: i64,
        after: Option<&str>,
        filter: &AgentFilter,
    ) -> Result<AgentPage, RepositoryError> {
        let (agents, has_next_page, total_count) = run(
            &self.pool,
            Direction::Forward,
            principal_id,
            project_id,
            first,
            after,
            filter,
        )
        .await?;
        let (start_cursor, end_cursor) = cursors(&agents, filter);
        Ok(AgentPage {
            agents,
            start_cursor,
            end_cursor,
            has_previous_page: after.is_some(),
            has_next_page,
            total_count,
        })
    }

    async fn find_agents_before(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        last: i64,
        before: Option<&str>,
        filter: &AgentFilter,
    ) -> Result<AgentPage, RepositoryError> {
        let (mut agents, has_previous_page, total_count) = run(
            &self.pool,
            Direction::Backward,
            principal_id,
            project_id,
            last,
            before,
            filter,
        )
        .await?;
        agents.reverse();
        let (start_cursor, end_cursor) = cursors(&agents, filter);
        Ok(AgentPage {
            agents,
            start_cursor,
            end_cursor,
            has_previous_page,
            has_next_page: before.is_some(),
            total_count,
        })
    }
}
