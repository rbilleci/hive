//! Helpers shared by `queries`/`mutations`: the pure content diagnostics, reference resolution
//! against the catalog and reusable resources, read assembly, the locked single-row fetchers, and
//! the draft/audit writes and digests the write commands share.

use crate::sql::{json_array, parse_string_array};
use hive_application::configuration::{
    digest, resource_identity, CatalogDefinition, ConfigurationRepositoryError as RepositoryError,
    McpServerConfiguration, ResourceVersion, ReusableResource, TypedReference,
};
use sqlx::{PgConnection, Row};
use std::sync::LazyLock;
use uuid::Uuid;

pub fn other(error: sqlx::Error) -> RepositoryError {
    RepositoryError::Other(error.into())
}

// --- pure content diagnostics (no DB access) ---

pub static SEVERITY_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"severity:(LOW|MEDIUM|HIGH)").unwrap());
pub static SCOPE_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"scope:[a-z-]+").unwrap());
pub static PROMPT_VARIABLE_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\{\{([^}]*)}}").unwrap());
pub static PROMPT_VARIABLE_NAME_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Za-z][A-Za-z0-9_]{0,63}$").unwrap());
pub static MAX_TOKENS_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"maxTokens:([0-9]+)").unwrap());
pub static PROFILE_ENVIRONMENT_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"environment:(DEVELOPMENT|STAGING|PRODUCTION)").unwrap());

pub fn valid_prompt_variables(content: &str) -> bool {
    let mut end = 0;
    for capture in PROMPT_VARIABLE_PATTERN.captures_iter(content) {
        if !PROMPT_VARIABLE_NAME_PATTERN.is_match(&capture[1]) {
            return false;
        }
        end = capture
            .get(0)
            .expect("capture group 0 always matches")
            .end();
    }
    !content[end..].contains("{{")
}

/// A digit run too long to fit `i64` is treated as unbounded (`false`)
/// instead of reproducing Java's `Integer.parseInt` overflow crash — a
/// deliberate, safer deviation, not a faithful reproduction of a latent bug.
pub fn bounded_max_tokens(content: &str) -> bool {
    let Some(captures) = MAX_TOKENS_PATTERN.captures(content) else {
        return false;
    };
    captures[1]
        .parse::<i64>()
        .is_ok_and(|value| value > 0 && value <= 16384)
}

pub fn profile_environment(content: &str) -> Option<String> {
    PROFILE_ENVIRONMENT_PATTERN
        .captures(content)
        .map(|captures| captures[1].to_string())
}

pub fn diagnostics(kind: &str, content: &str, refs: &[TypedReference]) -> Vec<String> {
    let mut values = Vec::new();
    if content.trim().is_empty() {
        values.push("Content is required.".to_string());
    }
    if kind == "PROMPT" && content.chars().count() > 12000 {
        values.push("Prompt content exceeds the local 12,000 character limit.".to_string());
    }
    if kind == "PROMPT" && !valid_prompt_variables(content) {
        values.push("Prompt variables must use {{identifier}}.".to_string());
    }
    if kind == "POLICY" && (!SEVERITY_PATTERN.is_match(content) || !SCOPE_PATTERN.is_match(content))
    {
        values.push("Policies require explicit severity and scope rules.".to_string());
    }
    if kind == "MODEL_PROFILE" && !refs.iter().any(|reference| reference.kind == "model") {
        values.push("Model profiles require an approved typed model definition.".to_string());
    }
    if kind == "MODEL_PROFILE" && !bounded_max_tokens(content) {
        values.push("Model profiles require bounded maxTokens parameters.".to_string());
    }
    if kind == "MODEL_PROFILE" && profile_environment(content).is_none() {
        values.push("Model profiles require an explicit environment.".to_string());
    }
    values
}

// --- reference resolution against the catalog and reusable resources ---

pub async fn catalog_definition(
    conn: &mut PgConnection,
    reference: &TypedReference,
    environment: Option<&str>,
) -> Result<bool, sqlx::Error> {
    if reference.kind != "model" && reference.kind != "tool" {
        return Ok(false);
    }
    Ok(sqlx::query(
        "SELECT 1 FROM catalog_definitions definition JOIN catalog_projection_heads head ON head.release_id = definition.release_id AND head.id = 'local' \
         WHERE definition.definition_kind = $1 AND definition.identity = $2 AND definition.version = $3 \
           AND ($4::text IS NULL OR definition.available_environments @> jsonb_build_array($4::text))",
    )
    .bind(&reference.kind)
    .bind(&reference.identity)
    .bind(&reference.version)
    .bind(environment)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

pub async fn resource_version_exists(
    conn: &mut PgConnection,
    project: Uuid,
    reference: &TypedReference,
) -> Result<bool, sqlx::Error> {
    if !reference.reusable_resource() {
        return Ok(false);
    }
    let Some(resource_kind) = resource_identity::resource_kind(&reference.kind) else {
        return Ok(false);
    };
    Ok(sqlx::query(
        "SELECT 1 FROM reusable_resources resource JOIN reusable_resource_versions versioned ON versioned.resource_id = resource.id \
         WHERE resource.project_id = $1 AND resource.resource_kind = $2 AND resource.identity = $3 AND versioned.version = $4",
    )
    .bind(project)
    .bind(resource_kind)
    .bind(&reference.identity)
    .bind(reference.reusable_resource_version())
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

pub async fn resolved(
    conn: &mut PgConnection,
    project: Uuid,
    refs: &[TypedReference],
) -> Result<bool, sqlx::Error> {
    for reference in refs {
        if !catalog_definition(conn, reference, None).await?
            && !resource_version_exists(conn, project, reference).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub async fn models_available(
    conn: &mut PgConnection,
    refs: &[TypedReference],
    environment: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let Some(environment) = environment else {
        return Ok(false);
    };
    for reference in refs {
        if reference.kind == "model"
            && !catalog_definition(conn, reference, Some(environment)).await?
        {
            return Ok(false);
        }
    }
    Ok(refs.iter().any(|reference| reference.kind == "model"))
}

// --- read assembly ---

pub async fn dependents(
    conn: &mut PgConnection,
    project: Uuid,
    identity: &str,
    version: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let reference = format!("tool:{identity}@{version}");
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT resource.name FROM reusable_resources resource JOIN reusable_resource_versions versioned ON versioned.resource_id = resource.id \
         WHERE resource.project_id = $1 AND versioned.dependencies @> jsonb_build_array($2) ORDER BY resource.name",
    )
    .bind(project)
    .bind(reference)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}

pub async fn resource_dependents(
    conn: &mut PgConnection,
    project: Uuid,
    kind: &str,
    identity: &str,
    version: i64,
) -> Result<Vec<String>, sqlx::Error> {
    let Some(reference) = resource_identity::reference(kind, identity, version) else {
        return Ok(Vec::new());
    };
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT dependent.name FROM reusable_resources dependent \
           LEFT JOIN reusable_resource_drafts draft ON draft.resource_id = dependent.id AND draft.revision = dependent.current_draft_revision \
           LEFT JOIN reusable_resource_versions published ON published.resource_id = dependent.id \
         WHERE dependent.project_id = $1 AND (draft.dependencies @> jsonb_build_array($2) OR published.dependencies @> jsonb_build_array($2)) \
         ORDER BY dependent.name",
    )
    .bind(project)
    .bind(&reference)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}

pub async fn versions(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Vec<ResourceVersion>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT version, content_digest, canonical_document::text, dependencies::text, published_at, published_by \
         FROM reusable_resource_versions WHERE resource_id = $1 ORDER BY version DESC",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let dependencies_json: String = row.get(3);
            ResourceVersion {
                version: row.get(0),
                content_digest: row.get(1),
                canonical_document: row.get(2),
                dependencies: parse_string_array(&dependencies_json),
                published_at: row.get(4),
                published_by: row.get::<Uuid, _>(5).to_string(),
            }
        })
        .collect())
}

pub async fn resource(
    conn: &mut PgConnection,
    project: Uuid,
    id: Uuid,
) -> Result<Option<ReusableResource>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT resource.resource_kind, resource.name, resource.identity, resource.current_draft_revision, \
                resource.current_published_version, resource.lifecycle_status, draft.content, draft.content_digest, \
                draft.dependencies::text, draft.validation_status, draft.diagnostics::text \
         FROM reusable_resources resource JOIN reusable_resource_drafts draft \
           ON draft.resource_id = resource.id AND draft.revision = resource.current_draft_revision \
         WHERE resource.project_id = $1 AND resource.id = $2",
    )
    .bind(project)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let kind: String = row.get(0);
    let identity: String = row.get(2);
    let published_version: Option<i64> = row.get(4);
    let draft_dependencies_json: String = row.get(8);
    let diagnostics_json: String = row.get(10);
    let dependent_resources = match published_version {
        Some(version) => resource_dependents(conn, project, &kind, &identity, version).await?,
        None => Vec::new(),
    };
    Ok(Some(ReusableResource {
        id,
        project_id: project,
        kind,
        name: row.get(1),
        identity,
        draft_revision: row.get(3),
        draft_content: row.get(6),
        draft_digest: row.get(7),
        draft_dependencies: parse_string_array(&draft_dependencies_json),
        validation_status: row.get(9),
        diagnostics: parse_string_array(&diagnostics_json),
        published_version,
        versions: versions(conn, id).await?,
        dependent_resources,
        lifecycle_status: row.get(5),
    }))
}

pub async fn mcp_server_from_row(
    conn: &mut PgConnection,
    project: Uuid,
    row: sqlx::postgres::PgRow,
) -> Result<McpServerConfiguration, sqlx::Error> {
    let id: Uuid = row.get(0);
    let lifecycle_status: String = row.get(15);
    let enabled: bool = row.get(6);
    let transport_type: Option<String> = row.get(7);
    let command: Option<String> = row.get(8);
    let remote_url: Option<String> = row.get(10);
    let status = if lifecycle_status == "ARCHIVED" {
        "ARCHIVED".to_string()
    } else if !enabled {
        "DISABLED".to_string()
    } else if transport_type.is_none()
        || (transport_type.as_deref() == Some("STDIO")
            && command.as_deref().unwrap_or("").trim().is_empty())
        || (transport_type.as_deref() == Some("REMOTE")
            && remote_url.as_deref().unwrap_or("").trim().is_empty())
    {
        "INCOMPLETE".to_string()
    } else {
        "NOT_CHECKED".to_string()
    };
    let definition_identity: String = row.get(3);
    let definition_version: String = row.get(4);
    let dependent_resources =
        dependents(conn, project, &definition_identity, &definition_version).await?;
    let arguments_json: String = row.get(9);
    let bindings_json: String = row.get(11);
    let tools_json: String = row.get(12);
    let resources_json: String = row.get(13);
    let prompts_json: String = row.get(14);
    Ok(McpServerConfiguration {
        id,
        project_id: project,
        server_id: row.get(1),
        name: row.get(2),
        definition_identity,
        definition_version,
        environment: row.get(5),
        enabled,
        transport_type,
        command,
        arguments: parse_string_array(&arguments_json),
        remote_url,
        redacted_bindings: parse_string_array(&bindings_json),
        tools: parse_string_array(&tools_json),
        resources: parse_string_array(&resources_json),
        prompts: parse_string_array(&prompts_json),
        lifecycle_status,
        status,
        revision: row.get(16),
        dependent_resources,
    })
}

pub const MCP_SERVER_COLUMNS: &str = "id, server_id, name, definition_identity, definition_version, environment, enabled, \
     transport_type, stdio_command, stdio_arguments::text, remote_url, redacted_bindings::text, \
     declared_tools::text, declared_resources::text, declared_prompts::text, lifecycle_status, revision";

pub async fn mcp_server(
    conn: &mut PgConnection,
    project: Uuid,
    id: Uuid,
) -> Result<McpServerConfiguration, sqlx::Error> {
    let sql = format!("SELECT {MCP_SERVER_COLUMNS} FROM project_tool_connections WHERE project_id = $1 AND id = $2");
    let row = sqlx::query(&sql)
        .bind(project)
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    mcp_server_from_row(conn, project, row).await
}

pub async fn definitions(
    conn: &mut PgConnection,
    release: &str,
) -> Result<Vec<CatalogDefinition>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT identity, version, definition_kind, display_name, content_digest, available_environments::text \
         FROM catalog_definitions WHERE release_id = $1 ORDER BY definition_kind, identity, version",
    )
    .bind(release)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let environments_json: String = row.get(5);
            CatalogDefinition {
                identity: row.get(0),
                version: row.get(1),
                kind: row.get(2),
                display_name: row.get(3),
                content_digest: row.get(4),
                available_environments: parse_string_array(&environments_json),
            }
        })
        .collect())
}

pub async fn environments(
    conn: &mut PgConnection,
    release: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT environment FROM catalog_environments WHERE release_id = $1 ORDER BY environment",
    )
    .bind(release)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(|(environment,)| environment).collect())
}

// --- locked reads and writes ---

pub struct LockedResource {
    pub kind: String,
    pub name: String,
    pub identity: String,
    pub draft_revision: i64,
    pub published_version: Option<i64>,
    pub lifecycle: String,
}

pub async fn locked_resource(
    conn: &mut PgConnection,
    project: Uuid,
    id: Uuid,
) -> Result<Option<LockedResource>, sqlx::Error> {
    let row = sqlx::query("SELECT resource_kind, name, identity, current_draft_revision, current_published_version, lifecycle_status FROM reusable_resources WHERE project_id = $1 AND id = $2 FOR UPDATE")
        .bind(project)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|row| LockedResource {
        kind: row.get(0),
        name: row.get(1),
        identity: row.get(2),
        draft_revision: row.get(3),
        published_version: row.get(4),
        lifecycle: row.get(5),
    }))
}

pub struct LockedTool {
    pub revision: i64,
    pub server_id: String,
}

pub async fn locked_tool(
    conn: &mut PgConnection,
    project: Uuid,
    id: Uuid,
) -> Result<Option<LockedTool>, sqlx::Error> {
    let row = sqlx::query("SELECT revision, server_id FROM project_tool_connections WHERE project_id = $1 AND id = $2 FOR UPDATE").bind(project).bind(id).fetch_optional(&mut *conn).await?;
    Ok(row.map(|row| LockedTool {
        revision: row.get(0),
        server_id: row.get(1),
    }))
}

pub struct DraftRow {
    pub content: String,
    pub document: String,
    pub digest: String,
    pub dependencies: Vec<String>,
    pub validation: String,
}

pub async fn draft_row(
    conn: &mut PgConnection,
    id: Uuid,
    revision: i64,
) -> Result<DraftRow, sqlx::Error> {
    let row = sqlx::query("SELECT content, canonical_document::text, content_digest, dependencies::text, validation_status FROM reusable_resource_drafts WHERE resource_id = $1 AND revision = $2")
        .bind(id)
        .bind(revision)
        .fetch_one(&mut *conn)
        .await?;
    let dependencies_json: String = row.get(3);
    Ok(DraftRow {
        content: row.get(0),
        document: row.get(1),
        digest: row.get(2),
        dependencies: parse_string_array(&dependencies_json),
        validation: row.get(4),
    })
}

pub fn typed(values: &[String]) -> Vec<TypedReference> {
    values
        .iter()
        .filter_map(|value| TypedReference::parse(value))
        .collect()
}

pub async fn published_digest(
    conn: &mut PgConnection,
    id: Uuid,
    version: i64,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as("SELECT content_digest FROM reusable_resource_versions WHERE resource_id = $1 AND version = $2").bind(id).bind(version).fetch_optional(&mut *conn).await?;
    Ok(row.map(|(value,)| value))
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_draft(
    conn: &mut PgConnection,
    id: Uuid,
    revision: i64,
    content: &str,
    resource_document: &str,
    resource_digest: &str,
    deps: &[TypedReference],
    diagnostics: &[String],
) -> Result<(), sqlx::Error> {
    let dependency_values: Vec<String> = deps.iter().map(TypedReference::value).collect();
    let status = if diagnostics.is_empty() {
        "UNVALIDATED"
    } else {
        "INVALID"
    };
    sqlx::query(
        "INSERT INTO reusable_resource_drafts (resource_id, revision, content, canonical_document, content_digest, dependencies, validation_status, diagnostics) \
         VALUES ($1, $2, $3, $4::jsonb, $5, $6::jsonb, $7, $8::jsonb)",
    )
    .bind(id)
    .bind(revision)
    .bind(content)
    .bind(resource_document)
    .bind(resource_digest)
    .bind(json_array(&dependency_values))
    .bind(status)
    .bind(json_array(diagnostics))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn audit(
    conn: &mut PgConnection,
    actor: Uuid,
    project: Uuid,
    action: &str,
    subject: Uuid,
    content_digest: &str,
    detail: &str,
) -> Result<(), sqlx::Error> {
    crate::audit::bind_audit_metadata(
        sqlx::query(
            "INSERT INTO configuration_audit_events (id, actor_principal_id, project_id, action, subject_id, content_digest, detail, \
                 request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(Uuid::new_v4())
        .bind(actor)
        .bind(project)
        .bind(action)
        .bind(subject)
        .bind(content_digest)
        .bind(detail),
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub fn first_binding(values: &[String]) -> String {
    values
        .first()
        .cloned()
        .unwrap_or_else(|| "redacted://local/unbound".to_string())
}

#[allow(clippy::too_many_arguments)]
pub fn mcp_digest(
    server_id: &str,
    name: &str,
    definition: &TypedReference,
    environment: &str,
    enabled: bool,
    transport: &str,
    command: Option<&str>,
    arguments: &[String],
    remote_url: Option<&str>,
    bindings: &[String],
    tools: &[String],
    resources: &[String],
    prompts: &[String],
) -> String {
    let command_text = command
        .map(str::to_string)
        .unwrap_or_else(|| "null".to_string());
    let remote_url_text = remote_url
        .map(str::to_string)
        .unwrap_or_else(|| "null".to_string());
    let joined = [
        server_id,
        name,
        &definition.value(),
        environment,
        &enabled.to_string(),
        transport,
        &command_text,
        &arguments.join(","),
        &remote_url_text,
        &bindings.join(","),
        &tools.join(","),
        &resources.join(","),
        &prompts.join(","),
    ]
    .join("|");
    digest(&joined)
}

pub fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db_error) if db_error.code().as_deref() == Some("23505"))
}
