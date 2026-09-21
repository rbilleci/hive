//! What the configuration commands and the computed fields share, on SeaORM entities: the content
//! diagnostics, reference resolution against the local catalog and the project's published
//! resources, the reverse dependency lookups, the locked row reads, and the draft and audit
//! writes.
//!
//! `dependencies`, `diagnostics` and `available_environments` are JSON arrays of strings (Aurora
//! DSQL has no array type), so membership is a `jsonb` containment test (`@>`, sea-query's
//! `PgExpr::contains`) against a one-element array.
//!
//! Every helper takes `db: &impl ConnectionTrait`: a command passes its transaction, a computed
//! field the connection.

use crate::audit::context::request_metadata;
use crate::entity::enums::{
    CatalogDefinitionKind, ReusableResourceKind, ReusableResourceValidationStatus,
};
use crate::entity::{
    catalog_definitions, catalog_projection_heads, catalog_releases, configuration_audit_events,
    project_tool_connections, reusable_resource_drafts, reusable_resource_versions,
    reusable_resources,
};
use hive_application::configuration::{digest, resource_identity, TypedReference};
use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, NotSet, QueryFilter,
    QueryOrder, QuerySelect, QueryTrait, RelationTrait, Set,
};
use sea_orm::{JoinType, JsonValue};
use std::sync::LazyLock;
use uuid::Uuid;

/// The catalog projection this service reads.
pub(crate) const LOCAL_CATALOG_HEAD: &str = "local";

/// A stored JSON array of strings.
pub fn strings(value: &JsonValue) -> Vec<String> {
    value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The one-element JSON array a containment test looks for.
fn containing(value: &str) -> Expr {
    Expr::value(serde_json::json!([value]))
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

/// A digit run too long to fit `i64` is treated as unbounded (`false`).
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

pub fn diagnostics(
    kind: ReusableResourceKind,
    content: &str,
    refs: &[TypedReference],
) -> Vec<String> {
    let prompt = kind == ReusableResourceKind::Prompt;
    let policy = kind == ReusableResourceKind::Policy;
    let profile = kind == ReusableResourceKind::ModelProfile;
    let mut values = Vec::new();
    if content.trim().is_empty() {
        values.push("Content is required.".to_string());
    }
    if prompt && content.chars().count() > 12000 {
        values.push("Prompt content exceeds the local 12,000 character limit.".to_string());
    }
    if prompt && !valid_prompt_variables(content) {
        values.push("Prompt variables must use {{identifier}}.".to_string());
    }
    if policy && (!SEVERITY_PATTERN.is_match(content) || !SCOPE_PATTERN.is_match(content)) {
        values.push("Policies require explicit severity and scope rules.".to_string());
    }
    if profile && !refs.iter().any(|reference| reference.kind == "model") {
        values.push("Model profiles require an approved typed model definition.".to_string());
    }
    if profile && !bounded_max_tokens(content) {
        values.push("Model profiles require bounded maxTokens parameters.".to_string());
    }
    if profile && profile_environment(content).is_none() {
        values.push("Model profiles require an explicit environment.".to_string());
    }
    values
}

/// The content diagnostics, plus one when a model profile names a model that the catalog does not
/// offer in the profile's environment.
pub async fn resource_diagnostics(
    db: &impl ConnectionTrait,
    kind: ReusableResourceKind,
    content: &str,
    refs: &[TypedReference],
) -> Result<Vec<String>, DbErr> {
    let mut values = diagnostics(kind, content, refs);
    if kind == ReusableResourceKind::ModelProfile
        && !models_available(db, refs, profile_environment(content).as_deref()).await?
    {
        values.push("A selected model is unavailable in the requested environment.".to_string());
    }
    Ok(values)
}

// --- reference resolution against the catalog and reusable resources ---

/// The catalog release the local projection points at.
pub(crate) async fn catalog_release(
    db: &impl ConnectionTrait,
) -> Result<Option<catalog_releases::Model>, DbErr> {
    catalog_releases::Entity::find()
        .join(
            JoinType::InnerJoin,
            catalog_releases::Relation::CatalogProjectionHeads.def(),
        )
        .filter(catalog_projection_heads::Column::Id.eq(LOCAL_CATALOG_HEAD))
        .one(db)
        .await
}

/// Whether the local catalog release has this exact model or tool definition; with
/// `environment`, only when the definition is available there.
pub(crate) async fn catalog_definition(
    db: &impl ConnectionTrait,
    reference: &TypedReference,
    environment: Option<&str>,
) -> Result<bool, DbErr> {
    let Ok(kind) = CatalogDefinitionKind::try_from_value(&reference.kind) else {
        return Ok(false);
    };
    let mut select = catalog_definitions::Entity::find()
        .join(
            JoinType::InnerJoin,
            catalog_definitions::Relation::CatalogReleases.def(),
        )
        .join(
            JoinType::InnerJoin,
            catalog_releases::Relation::CatalogProjectionHeads.def(),
        )
        .filter(catalog_projection_heads::Column::Id.eq(LOCAL_CATALOG_HEAD))
        .filter(catalog_definitions::Column::DefinitionKind.eq(kind))
        .filter(catalog_definitions::Column::Identity.eq(reference.identity.clone()))
        .filter(catalog_definitions::Column::Version.eq(reference.version.clone()));
    if let Some(environment) = environment {
        select = select.filter(
            Expr::col(catalog_definitions::Column::AvailableEnvironments.as_column_ref())
                .contains(containing(environment)),
        );
    }
    Ok(select.one(db).await?.is_some())
}

/// Whether the project has published this exact version of the referenced resource.
pub(crate) async fn resource_version_exists(
    db: &impl ConnectionTrait,
    project: Uuid,
    reference: &TypedReference,
) -> Result<bool, DbErr> {
    if !reference.reusable_resource() {
        return Ok(false);
    }
    let Some(kind) = resource_identity::resource_kind(&reference.kind)
        .and_then(|kind| ReusableResourceKind::try_from_value(&kind.to_string()).ok())
    else {
        return Ok(false);
    };
    let version = reusable_resource_versions::Entity::find()
        .inner_join(reusable_resources::Entity)
        .filter(reusable_resources::Column::ProjectId.eq(project))
        .filter(reusable_resources::Column::ResourceKind.eq(kind))
        .filter(reusable_resources::Column::Identity.eq(reference.identity.clone()))
        .filter(
            reusable_resource_versions::Column::Version.eq(reference.reusable_resource_version()),
        )
        .one(db)
        .await?;
    Ok(version.is_some())
}

/// Whether every reference is an exact local catalog definition or an exact published version of
/// one of the project's resources.
pub(crate) async fn resolved(
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

// --- reverse dependency lookups ---

/// The names of the project's resources with a published version that depends on this tool
/// definition, by name; one entry per such version.
pub async fn tool_dependents(
    db: &impl ConnectionTrait,
    project: Uuid,
    identity: &str,
    version: &str,
) -> Result<Vec<String>, DbErr> {
    let reference = format!("tool:{identity}@{version}");
    reusable_resource_versions::Entity::find()
        .select_only()
        .column(reusable_resources::Column::Name)
        .inner_join(reusable_resources::Entity)
        .filter(reusable_resources::Column::ProjectId.eq(project))
        .filter(
            Expr::col(reusable_resource_versions::Column::Dependencies.as_column_ref())
                .contains(containing(&reference)),
        )
        .order_by_asc(reusable_resources::Column::Name)
        .into_tuple::<String>()
        .all(db)
        .await
}

/// The distinct names of the project's resources whose current draft or any published version
/// depends on this exact version of a resource, by name.
pub async fn resource_dependents(
    db: &impl ConnectionTrait,
    resource: &reusable_resources::Model,
    version: i64,
) -> Result<Vec<String>, DbErr> {
    let kind = resource.resource_kind.to_value();
    let Some(reference) = resource_identity::reference(&kind, &resource.identity, version) else {
        return Ok(Vec::new());
    };
    let drafting = reusable_resource_drafts::Entity::find()
        .select_only()
        .column(reusable_resource_drafts::Column::ResourceId)
        .inner_join(reusable_resources::Entity)
        .filter(
            Expr::col(reusable_resource_drafts::Column::Revision.as_column_ref())
                .equals(reusable_resources::Column::CurrentDraftRevision.as_column_ref()),
        )
        .filter(
            Expr::col(reusable_resource_drafts::Column::Dependencies.as_column_ref())
                .contains(containing(&reference)),
        )
        .into_query();
    let published = reusable_resource_versions::Entity::find()
        .select_only()
        .column(reusable_resource_versions::Column::ResourceId)
        .filter(
            Expr::col(reusable_resource_versions::Column::Dependencies.as_column_ref())
                .contains(containing(&reference)),
        )
        .into_query();
    let mut names = reusable_resources::Entity::find()
        .select_only()
        .column(reusable_resources::Column::Name)
        .filter(reusable_resources::Column::ProjectId.eq(resource.project_id))
        .filter(
            Condition::any()
                .add(reusable_resources::Column::Id.in_subquery(drafting))
                .add(reusable_resources::Column::Id.in_subquery(published)),
        )
        .order_by_asc(reusable_resources::Column::Name)
        .into_tuple::<String>()
        .all(db)
        .await?;
    names.dedup();
    Ok(names)
}

// --- locked reads and writes ---

/// The project's resource, locked `FOR UPDATE`.
pub async fn locked_resource(
    db: &impl ConnectionTrait,
    project: Uuid,
    id: Uuid,
) -> Result<Option<reusable_resources::Model>, DbErr> {
    reusable_resources::Entity::find_by_id(id)
        .filter(reusable_resources::Column::ProjectId.eq(project))
        .lock_exclusive()
        .one(db)
        .await
}

/// The resource as stored, for a command's answer.
pub async fn stored_resource(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<reusable_resources::Model, DbErr> {
    reusable_resources::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no reusable resource with id {id}")))
}

/// The project's MCP server row, locked `FOR UPDATE`.
pub async fn locked_tool(
    db: &impl ConnectionTrait,
    project: Uuid,
    id: Uuid,
) -> Result<Option<project_tool_connections::Model>, DbErr> {
    project_tool_connections::Entity::find_by_id(id)
        .filter(project_tool_connections::Column::ProjectId.eq(project))
        .lock_exclusive()
        .one(db)
        .await
}

/// The MCP server row as stored, for a command's answer.
pub async fn stored_tool(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<project_tool_connections::Model, DbErr> {
    project_tool_connections::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no MCP server with id {id}")))
}

/// One revision of a resource's draft. The caller holds the lock on the resource.
pub async fn draft_row(
    db: &impl ConnectionTrait,
    id: Uuid,
    revision: i64,
) -> Result<reusable_resource_drafts::Model, DbErr> {
    reusable_resource_drafts::Entity::find_by_id((id, revision))
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no draft {revision} of resource {id}")))
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
    Ok(
        reusable_resource_versions::Entity::find_by_id((id, version))
            .one(db)
            .await?
            .map(|version| version.content_digest),
    )
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
        ReusableResourceValidationStatus::Unvalidated
    } else {
        ReusableResourceValidationStatus::Invalid
    };
    let draft = reusable_resource_drafts::ActiveModel {
        content: Set(content.to_string()),
        canonical_document: Set(serde_json::from_str(resource_document)
            .expect("a canonical configuration document is always valid JSON")),
        content_digest: Set(resource_digest.to_string()),
        dependencies: Set(serde_json::json!(dependency_values)),
        validation_status: Set(status),
        diagnostics: Set(serde_json::json!(diagnostics)),
        created_at: NotSet,
        resource_id: Set(id),
        revision: Set(revision),
    };
    reusable_resource_drafts::Entity::insert(draft)
        .exec_without_returning(db)
        .await?;
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
    let metadata = request_metadata();
    let event = configuration_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        actor_principal_id: Set(actor),
        project_id: Set(project),
        action: Set(action.to_string()),
        subject_id: Set(subject),
        content_digest: Set(content_digest.to_string()),
        detail: Set(detail.to_string()),
        occurred_at: NotSet,
        request_id: Set(metadata.request_id),
        correlation_id: Set(metadata.correlation_id),
        graphql_operation: Set(metadata.graphql_operation),
        source_ip: Set(metadata.source_ip),
        user_agent: Set(metadata.user_agent),
    };
    configuration_audit_events::Entity::insert(event)
        .exec_without_returning(db)
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

pub fn is_unique_violation(error: &DbErr) -> bool {
    crate::retry::is_unique_violation_db(error)
}
