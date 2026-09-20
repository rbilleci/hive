//! Ports the read-only `ConfigurationRepository` methods: `catalog`, `resources`, `resource`,
//! and `mcpServers`.

use super::rows::{self, MCP_SERVER_COLUMNS};
use hive_application::configuration::{
    CatalogRelease, ConfigurationRepositoryError as RepositoryError, McpServerConfiguration,
    ReusableResource,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub async fn catalog(
    pool: &PgPool,
    principal: Uuid,
    organization: Uuid,
) -> Result<Option<CatalogRelease>, RepositoryError> {
    if !crate::capability::has_capability(
        pool,
        principal,
        crate::capability::CATALOG_VIEW,
        crate::capability::Scope::Organization(organization),
        false,
    )
    .await
    .map_err(rows::other)?
    {
        return Ok(None);
    }
    let mut conn = pool.acquire().await.map_err(rows::other)?;
    let row = sqlx::query("SELECT release.id, release.source, release.source_digest, release.released_at FROM catalog_releases release JOIN catalog_projection_heads head ON head.release_id = release.id WHERE head.id = 'local'")
        .fetch_optional(&mut *conn)
        .await
        .map_err(rows::other)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id: String = row.get(0);
    let released_at: chrono::DateTime<chrono::Utc> = row.get(3);
    Ok(Some(CatalogRelease {
        source: row.get(1),
        source_digest: row.get(2),
        released_at: hive_domain::java_offset_date_time_string(released_at),
        definitions: rows::definitions(&mut conn, &id)
            .await
            .map_err(rows::other)?,
        environments: rows::environments(&mut conn, &id)
            .await
            .map_err(rows::other)?,
        id,
    }))
}

pub async fn resources(
    pool: &PgPool,
    principal: Uuid,
    project: Uuid,
    kind: Option<String>,
) -> Result<Option<Vec<ReusableResource>>, RepositoryError> {
    if !crate::capability::has_capability(
        pool,
        principal,
        crate::capability::CONFIGURATION_VIEW,
        crate::capability::Scope::Project(project),
        false,
    )
    .await
    .map_err(rows::other)?
    {
        return Ok(None);
    }
    let mut conn = pool.acquire().await.map_err(rows::other)?;
    let ids: Vec<(Uuid,)> = match kind.as_deref().filter(|value| !value.trim().is_empty()) {
        Some(kind) => sqlx::query_as("SELECT resource.id FROM reusable_resources resource WHERE resource.project_id = $1 AND resource.resource_kind = $2 ORDER BY resource.identity, resource.id")
            .bind(project)
            .bind(kind)
            .fetch_all(&mut *conn)
            .await
            .map_err(rows::other)?,
        None => sqlx::query_as("SELECT resource.id FROM reusable_resources resource WHERE resource.project_id = $1 ORDER BY resource.identity, resource.id")
            .bind(project)
            .fetch_all(&mut *conn)
            .await
            .map_err(rows::other)?,
    };
    let mut result = Vec::with_capacity(ids.len());
    for (id,) in ids {
        if let Some(value) = rows::resource(&mut conn, project, id)
            .await
            .map_err(rows::other)?
        {
            result.push(value);
        }
    }
    Ok(Some(result))
}

pub async fn resource(
    pool: &PgPool,
    principal: Uuid,
    project: Uuid,
    resource_id: Uuid,
) -> Result<Option<ReusableResource>, RepositoryError> {
    if !crate::capability::has_capability(
        pool,
        principal,
        crate::capability::CONFIGURATION_VIEW,
        crate::capability::Scope::Project(project),
        false,
    )
    .await
    .map_err(rows::other)?
    {
        return Ok(None);
    }
    let mut conn = pool.acquire().await.map_err(rows::other)?;
    rows::resource(&mut conn, project, resource_id)
        .await
        .map_err(rows::other)
}

pub async fn mcp_servers(
    pool: &PgPool,
    principal: Uuid,
    project: Uuid,
) -> Result<Option<Vec<McpServerConfiguration>>, RepositoryError> {
    if !crate::capability::has_capability(
        pool,
        principal,
        crate::capability::CONFIGURATION_VIEW,
        crate::capability::Scope::Project(project),
        false,
    )
    .await
    .map_err(rows::other)?
    {
        return Ok(None);
    }
    let mut conn = pool.acquire().await.map_err(rows::other)?;
    let sql = format!("SELECT {MCP_SERVER_COLUMNS} FROM project_tool_connections WHERE project_id = $1 ORDER BY name, id");
    let rows = sqlx::query(&sql)
        .bind(project)
        .fetch_all(&mut *conn)
        .await
        .map_err(rows::other)?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push(
            rows::mcp_server_from_row(&mut conn, project, row)
                .await
                .map_err(rows::other)?,
        );
    }
    Ok(Some(result))
}
