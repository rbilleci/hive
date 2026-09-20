//! Helpers shared by `queries`/`mutations`: the pure content diagnostics, reference resolution
//! against the catalog and reusable resources, read assembly, the locked single-row fetchers, and
//! the draft/audit writes and digests the write commands share.
//!
//! `GSR-PERSISTENCE`: runs through `sea_orm::ConnectionTrait` via
//! `Statement::from_sql_and_values` + `query_one_raw`/`query_all_raw`/`execute_raw`, preserving
//! every SQL string verbatim (same idiom as `capability`/`console`/module 6, `GSR-PHASE-P5`/`-P6`).
//! Every helper takes `db: &impl ConnectionTrait` generically: `mutations.rs` passes a
//! `&DatabaseTransaction` (`ConnectionTrait` covers both, unlike `sqlx`'s executor trait, which is
//! why `capability::tx.rs` once needed a hand-duplicated twin for exactly this reason — see that
//! module's own doc comment, now obsolete for any repository ported this way).

use crate::sql::{json_array, parse_string_array};
use hive_application::configuration::{
    digest, resource_identity, CatalogDefinition, ConfigurationRepositoryError as RepositoryError,
    McpServerConfiguration, ResourceVersion, ReusableResource, TypedReference,
};
use sea_orm::{ConnectionTrait, DbErr, QueryResult, Statement};
use std::sync::LazyLock;
use uuid::Uuid;

pub fn other(error: DbErr) -> RepositoryError {
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
    db: &impl ConnectionTrait,
    reference: &TypedReference,
    environment: Option<&str>,
) -> Result<bool, DbErr> {
    if reference.kind != "model" && reference.kind != "tool" {
        return Ok(false);
    }
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM catalog_definitions definition JOIN catalog_projection_heads head ON head.release_id = definition.release_id AND head.id = 'local' \
         WHERE definition.definition_kind = $1 AND definition.identity = $2 AND definition.version = $3 \
           AND ($4::text IS NULL OR definition.available_environments @> jsonb_build_array($4::text))",
        [
            reference.kind.clone().into(),
            reference.identity.clone().into(),
            reference.version.clone().into(),
            environment.into(),
        ],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
}

pub async fn resource_version_exists(
    db: &impl ConnectionTrait,
    project: Uuid,
    reference: &TypedReference,
) -> Result<bool, DbErr> {
    if !reference.reusable_resource() {
        return Ok(false);
    }
    let Some(resource_kind) = resource_identity::resource_kind(&reference.kind) else {
        return Ok(false);
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM reusable_resources resource JOIN reusable_resource_versions versioned ON versioned.resource_id = resource.id \
         WHERE resource.project_id = $1 AND resource.resource_kind = $2 AND resource.identity = $3 AND versioned.version = $4",
        [
            project.into(),
            resource_kind.into(),
            reference.identity.clone().into(),
            reference.reusable_resource_version().into(),
        ],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
}

pub async fn resolved(
    db: &impl ConnectionTrait,
    project: Uuid,
    refs: &[TypedReference],
) -> Result<bool, DbErr> {
    for reference in refs {
        if !catalog_definition(db, reference, None).await?
            && !resource_version_exists(db, project, reference).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub async fn models_available(
    db: &impl ConnectionTrait,
    refs: &[TypedReference],
    environment: Option<&str>,
) -> Result<bool, DbErr> {
    let Some(environment) = environment else {
        return Ok(false);
    };
    for reference in refs {
        if reference.kind == "model"
            && !catalog_definition(db, reference, Some(environment)).await?
        {
            return Ok(false);
        }
    }
    Ok(refs.iter().any(|reference| reference.kind == "model"))
}

// --- read assembly ---

pub async fn dependents(
    db: &impl ConnectionTrait,
    project: Uuid,
    identity: &str,
    version: &str,
) -> Result<Vec<String>, DbErr> {
    let reference = format!("tool:{identity}@{version}");
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT resource.name FROM reusable_resources resource JOIN reusable_resource_versions versioned ON versioned.resource_id = resource.id \
         WHERE resource.project_id = $1 AND versioned.dependencies @> jsonb_build_array($2) ORDER BY resource.name",
        [project.into(), reference.into()],
    );
    let rows = db.query_all_raw(statement).await?;
    rows.iter().map(|row| row.try_get_by("name")).collect()
}

pub async fn resource_dependents(
    db: &impl ConnectionTrait,
    project: Uuid,
    kind: &str,
    identity: &str,
    version: i64,
) -> Result<Vec<String>, DbErr> {
    let Some(reference) = resource_identity::reference(kind, identity, version) else {
        return Ok(Vec::new());
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT DISTINCT dependent.name FROM reusable_resources dependent \
           LEFT JOIN reusable_resource_drafts draft ON draft.resource_id = dependent.id AND draft.revision = dependent.current_draft_revision \
           LEFT JOIN reusable_resource_versions published ON published.resource_id = dependent.id \
         WHERE dependent.project_id = $1 AND (draft.dependencies @> jsonb_build_array($2) OR published.dependencies @> jsonb_build_array($2)) \
         ORDER BY dependent.name",
        [project.into(), reference.into()],
    );
    let rows = db.query_all_raw(statement).await?;
    rows.iter().map(|row| row.try_get_by("name")).collect()
}

pub async fn versions(db: &impl ConnectionTrait, id: Uuid) -> Result<Vec<ResourceVersion>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT version, content_digest, canonical_document::text, dependencies::text, published_at, published_by \
         FROM reusable_resource_versions WHERE resource_id = $1 ORDER BY version DESC",
        [id.into()],
    );
    let rows = db.query_all_raw(statement).await?;
    rows.iter()
        .map(|row| {
            let dependencies_json: String = row.try_get_by("dependencies")?;
            Ok(ResourceVersion {
                version: row.try_get_by("version")?,
                content_digest: row.try_get_by("content_digest")?,
                canonical_document: row.try_get_by("canonical_document")?,
                dependencies: parse_string_array(&dependencies_json),
                published_at: row.try_get_by("published_at")?,
                published_by: row.try_get_by::<Uuid, _>("published_by")?.to_string(),
            })
        })
        .collect()
}

pub async fn resource(
    db: &impl ConnectionTrait,
    project: Uuid,
    id: Uuid,
) -> Result<Option<ReusableResource>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT resource.resource_kind, resource.name, resource.identity, resource.current_draft_revision, \
                resource.current_published_version, resource.lifecycle_status, draft.content, draft.content_digest, \
                draft.dependencies::text, draft.validation_status, draft.diagnostics::text \
         FROM reusable_resources resource JOIN reusable_resource_drafts draft \
           ON draft.resource_id = resource.id AND draft.revision = resource.current_draft_revision \
         WHERE resource.project_id = $1 AND resource.id = $2",
        [project.into(), id.into()],
    );
    let Some(row) = db.query_one_raw(statement).await? else {
        return Ok(None);
    };
    let kind: String = row.try_get_by("resource_kind")?;
    let identity: String = row.try_get_by("identity")?;
    let published_version: Option<i64> = row.try_get_by("current_published_version")?;
    let draft_dependencies_json: String = row.try_get_by("dependencies")?;
    let diagnostics_json: String = row.try_get_by("diagnostics")?;
    let dependent_resources = match published_version {
        Some(version) => resource_dependents(db, project, &kind, &identity, version).await?,
        None => Vec::new(),
    };
    Ok(Some(ReusableResource {
        id,
        project_id: project,
        kind,
        name: row.try_get_by("name")?,
        identity,
        draft_revision: row.try_get_by("current_draft_revision")?,
        draft_content: row.try_get_by("content")?,
        draft_digest: row.try_get_by("content_digest")?,
        draft_dependencies: parse_string_array(&draft_dependencies_json),
        validation_status: row.try_get_by("validation_status")?,
        diagnostics: parse_string_array(&diagnostics_json),
        published_version,
        versions: versions(db, id).await?,
        dependent_resources,
        lifecycle_status: row.try_get_by("lifecycle_status")?,
    }))
}

pub async fn mcp_server_from_row(
    db: &impl ConnectionTrait,
    project: Uuid,
    row: &QueryResult,
) -> Result<McpServerConfiguration, DbErr> {
    let id: Uuid = row.try_get_by("id")?;
    let lifecycle_status: String = row.try_get_by("lifecycle_status")?;
    let enabled: bool = row.try_get_by("enabled")?;
    let transport_type: Option<String> = row.try_get_by("transport_type")?;
    let command: Option<String> = row.try_get_by("stdio_command")?;
    let remote_url: Option<String> = row.try_get_by("remote_url")?;
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
    let definition_identity: String = row.try_get_by("definition_identity")?;
    let definition_version: String = row.try_get_by("definition_version")?;
    let dependent_resources =
        dependents(db, project, &definition_identity, &definition_version).await?;
    let arguments_json: String = row.try_get_by("stdio_arguments")?;
    let bindings_json: String = row.try_get_by("redacted_bindings")?;
    let tools_json: String = row.try_get_by("declared_tools")?;
    let resources_json: String = row.try_get_by("declared_resources")?;
    let prompts_json: String = row.try_get_by("declared_prompts")?;
    Ok(McpServerConfiguration {
        id,
        project_id: project,
        server_id: row.try_get_by("server_id")?,
        name: row.try_get_by("name")?,
        definition_identity,
        definition_version,
        environment: row.try_get_by("environment")?,
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
        revision: row.try_get_by("revision")?,
        dependent_resources,
    })
}

pub const MCP_SERVER_COLUMNS: &str = "id, server_id, name, definition_identity, definition_version, environment, enabled, \
     transport_type, stdio_command, stdio_arguments::text, remote_url, redacted_bindings::text, \
     declared_tools::text, declared_resources::text, declared_prompts::text, lifecycle_status, revision";

pub async fn mcp_server(
    db: &impl ConnectionTrait,
    project: Uuid,
    id: Uuid,
) -> Result<McpServerConfiguration, DbErr> {
    let sql = format!(
        "SELECT {MCP_SERVER_COLUMNS} FROM project_tool_connections WHERE project_id = $1 AND id = $2"
    );
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        &sql,
        [project.into(), id.into()],
    );
    let row = db
        .query_one_raw(statement)
        .await?
        .expect("the caller has already confirmed this row exists (locked or just inserted)");
    mcp_server_from_row(db, project, &row).await
}

pub async fn definitions(
    db: &impl ConnectionTrait,
    release: &str,
) -> Result<Vec<CatalogDefinition>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT identity, version, definition_kind, display_name, content_digest, available_environments::text \
         FROM catalog_definitions WHERE release_id = $1 ORDER BY definition_kind, identity, version",
        [release.into()],
    );
    let rows = db.query_all_raw(statement).await?;
    rows.iter()
        .map(|row| {
            let environments_json: String = row.try_get_by("available_environments")?;
            Ok(CatalogDefinition {
                identity: row.try_get_by("identity")?,
                version: row.try_get_by("version")?,
                kind: row.try_get_by("definition_kind")?,
                display_name: row.try_get_by("display_name")?,
                content_digest: row.try_get_by("content_digest")?,
                available_environments: parse_string_array(&environments_json),
            })
        })
        .collect()
}

pub async fn environments(db: &impl ConnectionTrait, release: &str) -> Result<Vec<String>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT environment FROM catalog_environments WHERE release_id = $1 ORDER BY environment",
        [release.into()],
    );
    let rows = db.query_all_raw(statement).await?;
    rows.iter()
        .map(|row| row.try_get_by("environment"))
        .collect()
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
    db: &impl ConnectionTrait,
    project: Uuid,
    id: Uuid,
) -> Result<Option<LockedResource>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT resource_kind, name, identity, current_draft_revision, current_published_version, lifecycle_status FROM reusable_resources WHERE project_id = $1 AND id = $2 FOR UPDATE",
        [project.into(), id.into()],
    );
    let Some(row) = db.query_one_raw(statement).await? else {
        return Ok(None);
    };
    Ok(Some(LockedResource {
        kind: row.try_get_by("resource_kind")?,
        name: row.try_get_by("name")?,
        identity: row.try_get_by("identity")?,
        draft_revision: row.try_get_by("current_draft_revision")?,
        published_version: row.try_get_by("current_published_version")?,
        lifecycle: row.try_get_by("lifecycle_status")?,
    }))
}

pub struct LockedTool {
    pub revision: i64,
    pub server_id: String,
}

pub async fn locked_tool(
    db: &impl ConnectionTrait,
    project: Uuid,
    id: Uuid,
) -> Result<Option<LockedTool>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT revision, server_id FROM project_tool_connections WHERE project_id = $1 AND id = $2 FOR UPDATE",
        [project.into(), id.into()],
    );
    let Some(row) = db.query_one_raw(statement).await? else {
        return Ok(None);
    };
    Ok(Some(LockedTool {
        revision: row.try_get_by("revision")?,
        server_id: row.try_get_by("server_id")?,
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
    db: &impl ConnectionTrait,
    id: Uuid,
    revision: i64,
) -> Result<DraftRow, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT content, canonical_document::text, content_digest, dependencies::text, validation_status FROM reusable_resource_drafts WHERE resource_id = $1 AND revision = $2",
        [id.into(), revision.into()],
    );
    let row = db
        .query_one_raw(statement)
        .await?
        .expect("the caller has already locked this exact (id, revision) row");
    let dependencies_json: String = row.try_get_by("dependencies")?;
    Ok(DraftRow {
        content: row.try_get_by("content")?,
        document: row.try_get_by("canonical_document")?,
        digest: row.try_get_by("content_digest")?,
        dependencies: parse_string_array(&dependencies_json),
        validation: row.try_get_by("validation_status")?,
    })
}

pub fn typed(values: &[String]) -> Vec<TypedReference> {
    values
        .iter()
        .filter_map(|value| TypedReference::parse(value))
        .collect()
}

pub async fn published_digest(
    db: &impl ConnectionTrait,
    id: Uuid,
    version: i64,
) -> Result<Option<String>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT content_digest FROM reusable_resource_versions WHERE resource_id = $1 AND version = $2",
        [id.into(), version.into()],
    );
    db.query_one_raw(statement)
        .await?
        .map(|row| row.try_get_by("content_digest"))
        .transpose()
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_draft(
    db: &impl ConnectionTrait,
    id: Uuid,
    revision: i64,
    content: &str,
    resource_document: &str,
    resource_digest: &str,
    deps: &[TypedReference],
    diagnostics: &[String],
) -> Result<(), DbErr> {
    let dependency_values: Vec<String> = deps.iter().map(TypedReference::value).collect();
    let status = if diagnostics.is_empty() {
        "UNVALIDATED"
    } else {
        "INVALID"
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO reusable_resource_drafts (resource_id, revision, content, canonical_document, content_digest, dependencies, validation_status, diagnostics) \
         VALUES ($1, $2, $3, $4::jsonb, $5, $6::jsonb, $7, $8::jsonb)",
        [
            id.into(),
            revision.into(),
            content.into(),
            resource_document.into(),
            resource_digest.into(),
            json_array(&dependency_values).into(),
            status.into(),
            json_array(diagnostics).into(),
        ],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

pub async fn audit(
    db: &impl ConnectionTrait,
    actor: Uuid,
    project: Uuid,
    action: &str,
    subject: Uuid,
    content_digest: &str,
    detail: &str,
) -> Result<(), DbErr> {
    let mut values: Vec<sea_orm::Value> = vec![
        Uuid::new_v4().into(),
        actor.into(),
        project.into(),
        action.into(),
        subject.into(),
        content_digest.into(),
        detail.into(),
    ];
    values.extend(crate::audit::context::audit_metadata_values());
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO configuration_audit_events (id, actor_principal_id, project_id, action, subject_id, content_digest, detail, \
             request_id, correlation_id, graphql_operation, source_ip, user_agent) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        values,
    );
    db.execute_raw(statement).await?;
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

pub fn is_unique_violation(error: &DbErr) -> bool {
    crate::sql::is_unique_violation_db(error)
}
