//! Ports `JpaProjectAgentDirectoryRepository`: the tenant-scoped project lookup,
//! the case-insensitive-ordered (`lower(display_name)`) bidirectional agent
//! directory, and the batched "latest published version" lookup keyed by the
//! already-tenant-filtered agent id set (no separate tenant recheck needed there:
//! the id set fed into it is already exactly the visible rows).
//!
//! `GSR-PERSISTENCE`: runs through `sea_orm::ConnectionTrait` via
//! `Statement::from_sql_and_values` + `query_all_raw`/`query_one_raw`, preserving the original SQL
//! text and its dynamic-parameter-count shape verbatim (same idiom as `capability`/`console`/
//! `organization::project_directory`, `GSR-PHASE-P5`). The `= ANY($1)` array bind in
//! `latest_versions` uses a hand-built `Value::Array(ArrayType::Uuid, ...)`, the same construction
//! `capability::locks::lock_deployment_approval_authority_page` needed (sea-query has no blanket
//! `Vec<T> -> Value` conversion for arbitrary bindable types).

use hive_application::agent::{
    Agent, AgentCursor, AgentCursorError, AgentDirectoryProject, AgentFilter, AgentPage,
    ProjectAgentDirectoryRepository, ProjectAgentDirectoryRepositoryError as RepositoryError,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, QueryResult, Statement, Value};
use std::collections::HashMap;
use uuid::Uuid;

pub struct PgProjectAgentDirectoryRepository {
    db: DatabaseConnection,
}

impl PgProjectAgentDirectoryRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
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

fn other(error: DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

async fn latest_versions(
    db: &DatabaseConnection,
    agent_ids: &[Uuid],
) -> Result<HashMap<Uuid, (i32, Option<String>)>, RepositoryError> {
    if agent_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let ids = Value::Array(
        sea_orm::sea_query::ArrayType::Uuid,
        Some(Box::new(
            agent_ids.iter().map(|id| Value::from(*id)).collect(),
        )),
    );
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT v.agent_id, v.version_number, v.canonical_document FROM agent_versions v \
         WHERE v.agent_id = ANY($1) \
           AND v.version_number = (SELECT max(v2.version_number) FROM agent_versions v2 WHERE v2.agent_id = v.agent_id)",
        [ids],
    );
    let rows = db.query_all_raw(statement).await.map_err(other)?;

    let mut result = HashMap::new();
    for row in rows {
        let agent_id: Uuid = row.try_get_by("agent_id").map_err(other)?;
        let version_number: i64 = row.try_get_by("version_number").map_err(other)?;
        let document: serde_json::Value = row.try_get_by("canonical_document").map_err(other)?;
        let model = document
            .get("model")
            .and_then(|m| m.get("reference"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        result.insert(agent_id, (version_number as i32, model));
    }
    Ok(result)
}

fn to_agent(row: &QueryResult, latest: Option<&(i32, Option<String>)>) -> Result<Agent, DbErr> {
    Ok(Agent {
        id: row.try_get_by("id")?,
        slug: row.try_get_by("slug")?,
        display_name: row.try_get_by("display_name")?,
        lifecycle_status: row.try_get_by("lifecycle_status")?,
        latest_published_version: latest.map(|(version, _)| *version),
        model: latest.and_then(|(_, model)| model.clone()),
    })
}

async fn run(
    db: &DatabaseConnection,
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
    // `totalCount` describes the filtered list, not the rows that remain after the cursor.
    let count_sql = format!("SELECT count(*) AS total_count FROM agents a WHERE {where_clause}");
    if cursor.is_some() {
        where_clause.push_str(&format!(
            " AND (lower(a.display_name), a.id) {comparison} (${next_param}, ${})",
            next_param + 1
        ));
        next_param += 2;
    }
    let limit_param = next_param;

    let list_sql = format!(
        "SELECT a.id, a.slug, a.display_name, a.lifecycle_status FROM agents a WHERE {where_clause} ORDER BY {order} LIMIT ${limit_param}"
    );

    let mut list_values: Vec<Value> = vec![project_id.into(), principal_id.into()];
    let mut count_values: Vec<Value> = vec![project_id.into(), principal_id.into()];
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
        list_values.push(cursor.sort_name.clone().into());
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

    let overflow = rows.len() as i64 > count;
    let bounded_rows = if overflow {
        &rows[..rows.len() - 1]
    } else {
        &rows[..]
    };
    let agent_ids: Vec<Uuid> = bounded_rows
        .iter()
        .map(|row| row.try_get_by("id"))
        .collect::<Result<_, DbErr>>()
        .map_err(other)?;
    let latest = latest_versions(db, &agent_ids).await?;
    let agents: Vec<Agent> = bounded_rows
        .iter()
        .zip(&agent_ids)
        .map(|(row, id)| to_agent(row, latest.get(id)))
        .collect::<Result<_, DbErr>>()
        .map_err(other)?;

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
        let statement = Statement::from_sql_and_values(
            self.db.get_database_backend(),
            "SELECT p.id, p.slug, p.display_name, p.lifecycle_status FROM projects p \
             WHERE p.id = $1 AND EXISTS (\
               SELECT 1 FROM organization_memberships tenant_membership \
               WHERE tenant_membership.organization_id = p.organization_id \
                 AND tenant_membership.principal_id = $2 \
                 AND tenant_membership.started_at <= CURRENT_TIMESTAMP \
                 AND tenant_membership.ended_at IS NULL)",
            [project_id.into(), principal_id.into()],
        );
        let row = self.db.query_one_raw(statement).await.map_err(other)?;

        row.map(|row| {
            Ok(AgentDirectoryProject {
                id: row.try_get_by("id")?,
                slug: row.try_get_by("slug")?,
                display_name: row.try_get_by("display_name")?,
                lifecycle_status: row.try_get_by("lifecycle_status")?,
            })
        })
        .transpose()
        .map_err(other)
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
            &self.db,
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
            &self.db,
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
