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

/// The body of one reusable-resource draft revision. The submitted text, the canonical document
/// rendered from it and that document's digest are three adjacent strings, so they are named:
/// a transposed pair would compile and store a digest as the content.
pub struct DraftBody<'a> {
    pub content: &'a str,
    pub resource_document: &'a str,
    pub resource_digest: &'a str,
    pub deps: &'a [TypedReference],
    pub diagnostics: &'a [String],
}

pub async fn insert_draft(
    db: &impl ConnectionTrait,
    id: Uuid,
    revision: i64,
    body: &DraftBody<'_>,
) -> Result<(), DbErr> {
    let dependency_values: Vec<String> = body.deps.iter().map(TypedReference::value).collect();
    let status = if body.diagnostics.is_empty() {
        ReusableResourceValidationStatus::Unvalidated
    } else {
        ReusableResourceValidationStatus::Invalid
    };
    let draft = reusable_resource_drafts::ActiveModel {
        content: Set(body.content.to_string()),
        canonical_document: Set(serde_json::from_str(body.resource_document)
            .expect("a canonical configuration document is always valid JSON")),
        content_digest: Set(body.resource_digest.to_string()),
        dependencies: Set(serde_json::json!(dependency_values)),
        validation_status: Set(status),
        diagnostics: Set(serde_json::json!(body.diagnostics)),
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

#[cfg(test)]
mod content_rule_tests {
    use super::{
        bounded_max_tokens, diagnostics, first_binding, mcp_digest, profile_environment, strings,
        typed, valid_prompt_variables,
    };
    use crate::entity::enums::ReusableResourceKind;
    use hive_application::configuration::TypedReference;
    use serde_json::json;

    fn model_reference() -> TypedReference {
        TypedReference::parse("model:local-small@v1").expect("a typed model reference")
    }

    #[test]
    fn a_stored_string_array_drops_every_member_that_is_not_a_string() {
        assert_eq!(strings(&json!(["a", "b"])), vec!["a", "b"]);
        assert_eq!(strings(&json!(["a", 1, null, {}])), vec!["a"]);
        assert!(strings(&json!([])).is_empty());
        assert!(strings(&json!(null)).is_empty());
        assert!(strings(&json!("a")).is_empty());
    }

    /// Every `{{…}}` must name an identifier, and — the case a regex sweep alone misses — an
    /// unterminated `{{` after the last well-formed capture must still refuse.
    #[test]
    fn prompt_variables_must_all_be_identifiers_and_none_may_be_left_open() {
        assert!(valid_prompt_variables(""));
        assert!(valid_prompt_variables("no variables here"));
        assert!(valid_prompt_variables("Hello {{name}} and {{Other_1}}."));
        assert!(!valid_prompt_variables("Hello {{}}."));
        assert!(!valid_prompt_variables("Hello {{ name }}."));
        assert!(!valid_prompt_variables("Hello {{1st}}."));
        assert!(!valid_prompt_variables("Hello {{name"));
        assert!(!valid_prompt_variables("Hello {{name}} and {{unclosed"));
        assert!(!valid_prompt_variables(&format!(
            "{{{{{}}}}}",
            "n".repeat(65)
        )));
        assert!(valid_prompt_variables(&format!(
            "{{{{{}}}}}",
            "n".repeat(64)
        )));
    }

    /// The bound is inclusive at 16384 and excludes zero, and a digit run too long for `i64` is
    /// treated as unbounded rather than wrapping to a value inside the range.
    #[test]
    fn max_tokens_must_be_present_and_within_the_bound() {
        assert!(!bounded_max_tokens("no declaration"));
        assert!(!bounded_max_tokens("maxTokens:0"));
        assert!(bounded_max_tokens("maxTokens:1"));
        assert!(bounded_max_tokens("maxTokens:16384"));
        assert!(!bounded_max_tokens("maxTokens:16385"));
        assert!(!bounded_max_tokens(&format!(
            "maxTokens:{}",
            "9".repeat(30)
        )));
        // The pattern requires a digit immediately after the colon, so a signed value does not
        // declare maxTokens at all.
        assert!(!bounded_max_tokens("maxTokens:-5"));
    }

    #[test]
    fn a_profile_environment_is_one_of_the_three_classes_or_absent() {
        assert_eq!(
            profile_environment("environment:PRODUCTION"),
            Some("PRODUCTION".to_string())
        );
        assert_eq!(
            profile_environment("a environment:STAGING b environment:PRODUCTION"),
            Some("STAGING".to_string()),
            "the first declaration wins"
        );
        assert_eq!(profile_environment("environment:production"), None);
        assert_eq!(profile_environment("environment:QA"), None);
        assert_eq!(profile_environment(""), None);
    }

    /// Empty content is the one diagnostic every kind raises.
    #[test]
    fn empty_content_is_refused_for_every_kind() {
        for kind in [
            ReusableResourceKind::Prompt,
            ReusableResourceKind::Policy,
            ReusableResourceKind::ModelProfile,
        ] {
            assert!(
                diagnostics(kind, "   \n\t ", &[]).contains(&"Content is required.".to_string()),
                "{kind:?}"
            );
        }
    }

    /// The prompt limit counts characters, not bytes, so a multi-byte prompt is measured as the
    /// author wrote it.
    #[test]
    fn the_prompt_length_limit_counts_characters() {
        let kind = ReusableResourceKind::Prompt;
        let limit = "Prompt content exceeds the local 12,000 character limit.".to_string();
        assert!(!diagnostics(kind, &"é".repeat(12000), &[]).contains(&limit));
        assert!(diagnostics(kind, &"é".repeat(12001), &[]).contains(&limit));
    }

    /// A policy needs both a severity and a scope; either one alone raises the same single
    /// diagnostic.
    #[test]
    fn a_policy_needs_both_a_severity_and_a_scope() {
        let kind = ReusableResourceKind::Policy;
        let required = "Policies require explicit severity and scope rules.".to_string();
        assert!(diagnostics(kind, "severity:HIGH", &[]).contains(&required));
        assert!(diagnostics(kind, "scope:project", &[]).contains(&required));
        assert!(!diagnostics(kind, "severity:HIGH scope:project", &[]).contains(&required));
        assert!(diagnostics(kind, "severity:CRITICAL scope:project", &[]).contains(&required));
    }

    /// A complete model profile raises nothing; each of its three requirements is answered by its
    /// own input, so dropping one leaves exactly one diagnostic.
    #[test]
    fn a_model_profile_needs_a_model_reference_bounded_tokens_and_an_environment() {
        let kind = ReusableResourceKind::ModelProfile;
        let content = "maxTokens:2048 environment:PRODUCTION";
        let refs = [model_reference()];
        assert!(diagnostics(kind, content, &refs).is_empty());
        assert_eq!(
            diagnostics(kind, content, &[]),
            vec!["Model profiles require an approved typed model definition.".to_string()]
        );
        assert_eq!(
            diagnostics(kind, "environment:PRODUCTION", &refs),
            vec!["Model profiles require bounded maxTokens parameters.".to_string()]
        );
        assert_eq!(
            diagnostics(kind, "maxTokens:2048", &refs),
            vec!["Model profiles require an explicit environment.".to_string()]
        );
    }

    /// A prompt's rules do not run against a policy, and a policy's do not run against a prompt.
    #[test]
    fn each_kinds_rules_run_only_for_that_kind() {
        assert!(
            diagnostics(
                ReusableResourceKind::Policy,
                "Hello {{}}. severity:HIGH scope:project",
                &[]
            )
            .is_empty(),
            "the prompt-variable rule does not run for a policy"
        );
        assert!(
            diagnostics(ReusableResourceKind::Prompt, "no severity, no scope", &[]).is_empty(),
            "the policy rule does not run for a prompt"
        );
    }

    /// A dependency string that does not parse is dropped rather than refused, so one malformed
    /// stored row does not take the whole resource's reference set with it.
    #[test]
    fn unparseable_dependency_strings_are_dropped() {
        let parsed = typed(&[
            "model:local-small@v1".to_string(),
            "not a reference".to_string(),
            "model:local-small@v1@v2".to_string(),
            ":missing-kind@v1".to_string(),
            // A resource kind additionally requires a canonical identity and a `vN` version.
            "prompt:Not-Canonical@v1".to_string(),
            "prompt:greeting@1".to_string(),
            String::new(),
        ]);
        assert_eq!(parsed.len(), 1, "{:?}", parsed);
        assert_eq!(parsed[0].value(), "model:local-small@v1");
    }

    /// An unbound connection still has a binding value, because the column is `NOT NULL` and the
    /// console renders the sentinel rather than an empty cell.
    #[test]
    fn no_binding_answers_the_unbound_sentinel() {
        assert_eq!(first_binding(&[]), "redacted://local/unbound");
        assert_eq!(
            first_binding(&["redacted://local/a".to_string(), "b".to_string()]),
            "redacted://local/a"
        );
    }

    /// The digest covers thirteen fields joined by `|`. Each one must change it, and the joins
    /// must not let a value migrate between two adjacent list fields unnoticed.
    #[test]
    fn every_mcp_field_changes_the_digest() {
        let definition = model_reference();
        let base = mcp_digest(
            "server",
            "name",
            &definition,
            "PRODUCTION",
            true,
            "STDIO",
            Some("run"),
            &["--flag".to_string()],
            None,
            &["binding".to_string()],
            &["tool".to_string()],
            &["resource".to_string()],
            &["prompt".to_string()],
        );
        let variants = [
            mcp_digest(
                "other",
                "name",
                &definition,
                "PRODUCTION",
                true,
                "STDIO",
                Some("run"),
                &["--flag".to_string()],
                None,
                &["binding".to_string()],
                &["tool".to_string()],
                &["resource".to_string()],
                &["prompt".to_string()],
            ),
            mcp_digest(
                "server",
                "other",
                &definition,
                "PRODUCTION",
                true,
                "STDIO",
                Some("run"),
                &["--flag".to_string()],
                None,
                &["binding".to_string()],
                &["tool".to_string()],
                &["resource".to_string()],
                &["prompt".to_string()],
            ),
            mcp_digest(
                "server",
                "name",
                &definition,
                "STAGING",
                true,
                "STDIO",
                Some("run"),
                &["--flag".to_string()],
                None,
                &["binding".to_string()],
                &["tool".to_string()],
                &["resource".to_string()],
                &["prompt".to_string()],
            ),
            mcp_digest(
                "server",
                "name",
                &definition,
                "PRODUCTION",
                false,
                "STDIO",
                Some("run"),
                &["--flag".to_string()],
                None,
                &["binding".to_string()],
                &["tool".to_string()],
                &["resource".to_string()],
                &["prompt".to_string()],
            ),
            mcp_digest(
                "server",
                "name",
                &definition,
                "PRODUCTION",
                true,
                "REMOTE",
                Some("run"),
                &["--flag".to_string()],
                None,
                &["binding".to_string()],
                &["tool".to_string()],
                &["resource".to_string()],
                &["prompt".to_string()],
            ),
            mcp_digest(
                "server",
                "name",
                &definition,
                "PRODUCTION",
                true,
                "STDIO",
                None,
                &["--flag".to_string()],
                None,
                &["binding".to_string()],
                &["tool".to_string()],
                &["resource".to_string()],
                &["prompt".to_string()],
            ),
            mcp_digest(
                "server",
                "name",
                &definition,
                "PRODUCTION",
                true,
                "STDIO",
                Some("run"),
                &[],
                None,
                &["binding".to_string()],
                &["tool".to_string()],
                &["resource".to_string()],
                &["prompt".to_string()],
            ),
            mcp_digest(
                "server",
                "name",
                &definition,
                "PRODUCTION",
                true,
                "STDIO",
                Some("run"),
                &["--flag".to_string()],
                Some("https://example.test"),
                &["binding".to_string()],
                &["tool".to_string()],
                &["resource".to_string()],
                &["prompt".to_string()],
            ),
            mcp_digest(
                "server",
                "name",
                &definition,
                "PRODUCTION",
                true,
                "STDIO",
                Some("run"),
                &["--flag".to_string()],
                None,
                &[],
                &["tool".to_string()],
                &["resource".to_string()],
                &["prompt".to_string()],
            ),
            mcp_digest(
                "server",
                "name",
                &definition,
                "PRODUCTION",
                true,
                "STDIO",
                Some("run"),
                &["--flag".to_string()],
                None,
                &["binding".to_string()],
                &[],
                &["resource".to_string()],
                &["prompt".to_string()],
            ),
            mcp_digest(
                "server",
                "name",
                &definition,
                "PRODUCTION",
                true,
                "STDIO",
                Some("run"),
                &["--flag".to_string()],
                None,
                &["binding".to_string()],
                &["tool".to_string()],
                &[],
                &["prompt".to_string()],
            ),
            mcp_digest(
                "server",
                "name",
                &definition,
                "PRODUCTION",
                true,
                "STDIO",
                Some("run"),
                &["--flag".to_string()],
                None,
                &["binding".to_string()],
                &["tool".to_string()],
                &["resource".to_string()],
                &[],
            ),
        ];
        for (index, variant) in variants.iter().enumerate() {
            assert_ne!(&base, variant, "field {index} does not change the digest");
        }
        assert_eq!(
            variants
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            variants.len(),
            "two single-field changes collide"
        );
    }

    /// `None` is its own value, distinct from the literal text `null` a caller could send.
    #[test]
    fn an_absent_command_is_not_the_literal_text_null() {
        let definition = model_reference();
        let absent = mcp_digest(
            "s",
            "n",
            &definition,
            "PRODUCTION",
            true,
            "STDIO",
            None,
            &[],
            None,
            &[],
            &[],
            &[],
            &[],
        );
        let literal = mcp_digest(
            "s",
            "n",
            &definition,
            "PRODUCTION",
            true,
            "STDIO",
            Some("null"),
            &[],
            None,
            &[],
            &[],
            &[],
            &[],
        );
        assert_eq!(
            absent, literal,
            "the sentinel for an absent command is the text `null`"
        );
    }
}
