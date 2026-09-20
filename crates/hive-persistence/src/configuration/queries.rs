//! Ports the read-only `ConfigurationRepository` methods: `catalog`, `resources`, `resource`,
//! and `mcpServers`.
//!
//! `GSR-PERSISTENCE`: runs through `sea_orm::ConnectionTrait` via
//! `Statement::from_sql_and_values` + `query_one_raw`/`query_all_raw`, preserving every SQL string
//! verbatim (same idiom as `capability`/`console`/module 6, `GSR-PHASE-P5`/`-P6`).

use super::rows::{self, MCP_SERVER_COLUMNS};
use hive_application::configuration::{
    CatalogRelease, ConfigurationRepositoryError as RepositoryError, McpServerConfiguration,
    ReusableResource,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use uuid::Uuid;

pub async fn catalog(
    db: &DatabaseConnection,
    principal: Uuid,
    organization: Uuid,
) -> Result<Option<CatalogRelease>, RepositoryError> {
    if !crate::capability::has_capability(
        db,
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT release.id, release.source, release.source_digest, release.released_at FROM catalog_releases release JOIN catalog_projection_heads head ON head.release_id = release.id WHERE head.id = 'local'",
        [],
    );
    let Some(row) = db.query_one_raw(statement).await.map_err(rows::other)? else {
        return Ok(None);
    };
    let id: String = row.try_get_by("id").map_err(rows::other)?;
    let released_at: chrono::DateTime<chrono::Utc> =
        row.try_get_by("released_at").map_err(rows::other)?;
    Ok(Some(CatalogRelease {
        source: row.try_get_by("source").map_err(rows::other)?,
        source_digest: row.try_get_by("source_digest").map_err(rows::other)?,
        released_at: hive_domain::java_offset_date_time_string(released_at),
        definitions: rows::definitions(db, &id).await.map_err(rows::other)?,
        environments: rows::environments(db, &id).await.map_err(rows::other)?,
        id,
    }))
}

pub async fn resources(
    db: &DatabaseConnection,
    principal: Uuid,
    project: Uuid,
    kind: Option<String>,
) -> Result<Option<Vec<ReusableResource>>, RepositoryError> {
    if !crate::capability::has_capability(
        db,
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
    let backend = db.get_database_backend();
    let statement = match kind.as_deref().filter(|value| !value.trim().is_empty()) {
        Some(kind) => Statement::from_sql_and_values(
            backend,
            "SELECT resource.id FROM reusable_resources resource WHERE resource.project_id = $1 AND resource.resource_kind = $2 ORDER BY resource.identity, resource.id",
            [project.into(), kind.into()],
        ),
        None => Statement::from_sql_and_values(
            backend,
            "SELECT resource.id FROM reusable_resources resource WHERE resource.project_id = $1 ORDER BY resource.identity, resource.id",
            [project.into()],
        ),
    };
    let rows_found = db.query_all_raw(statement).await.map_err(rows::other)?;
    let mut result = Vec::with_capacity(rows_found.len());
    for row in rows_found {
        let id: Uuid = row.try_get_by("id").map_err(rows::other)?;
        if let Some(value) = rows::resource(db, project, id).await.map_err(rows::other)? {
            result.push(value);
        }
    }
    Ok(Some(result))
}

pub async fn resource(
    db: &DatabaseConnection,
    principal: Uuid,
    project: Uuid,
    resource_id: Uuid,
) -> Result<Option<ReusableResource>, RepositoryError> {
    if !crate::capability::has_capability(
        db,
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
    rows::resource(db, project, resource_id)
        .await
        .map_err(rows::other)
}

pub async fn mcp_servers(
    db: &DatabaseConnection,
    principal: Uuid,
    project: Uuid,
) -> Result<Option<Vec<McpServerConfiguration>>, RepositoryError> {
    if !crate::capability::has_capability(
        db,
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
    let sql = format!(
        "SELECT {MCP_SERVER_COLUMNS} FROM project_tool_connections WHERE project_id = $1 ORDER BY name, id"
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [project.into()]);
    let rows_found = db.query_all_raw(statement).await.map_err(rows::other)?;
    let mut result = Vec::with_capacity(rows_found.len());
    for row in &rows_found {
        result.push(
            rows::mcp_server_from_row(db, project, row)
                .await
                .map_err(rows::other)?,
        );
    }
    Ok(Some(result))
}
