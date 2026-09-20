//! Ports `PostgresAgentDraftRepository` in full: `findDraft`, `createDraft`,
//! `updateDraft`/`validateDraft` (the shared `command()`), `publishDraft`,
//! `reviewDraft`, `versions`, `version`, `compareVersions`, and every helper
//! they call, including the DSQL SQLSTATE 40001 commit-race handling in
//! `command()` and the `evaluation_target_projections` maintenance
//! `publishDraft` performs in place of the DSQL-incompatible trigger V039
//! declared (`PostgresEvaluationTargetProjection.projectAgentVersionTarget`).
//!
//! `GSR-PERSISTENCE`: every write command opens a `sea_orm::DatabaseTransaction` via
//! `TransactionTrait::begin`, and every locked capability re-check calls
//! `capability::has_capability`/`capability::queries::*` directly with `lock: true` and `&txn` —
//! the same pattern `configuration`/`administration` established (`GSR-PHASE-P6`). Every shared
//! helper takes `db: &impl ConnectionTrait` generically, unlike the `sqlx` original's concrete
//! `&mut PgConnection` (that concreteness was a `sqlx`-specific workaround the module's own doc
//! comment explained; `sea_orm`'s `ConnectionTrait` does not hit the same lifetime friction, the
//! same finding every module since `capability` itself has made).
//!
//! An early return before `txn.commit()` relies on `DatabaseTransaction`'s `Drop` implementation
//! to roll back, rather than an explicit `txn.rollback().await` at every refusal branch.

use crate::capability;
use crate::sql::{is_serialization_failure_db, json_array, parse_string_array};
use hive_application::agent::canonical_document;
use hive_application::agent::{
    AgentDraft, AgentDraftDiagnostic, AgentDraftMutationProblem, AgentDraftMutationResult,
    AgentDraftRepository, AgentDraftRepositoryError as RepositoryError, AgentDraftReview,
    AgentVersion, AgentVersionComparison,
};
use hive_application::configuration::{resource_identity, TypedReference};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use uuid::Uuid;

static VALID_NAME: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Za-z][A-Za-z0-9 _-]{1,80}$").unwrap());

fn valid_name(value: &str) -> bool {
    VALID_NAME.is_match(value)
}

pub struct PgAgentDraftRepository {
    db: DatabaseConnection,
}

impl PgAgentDraftRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[derive(Debug, Clone)]
struct Target {
    agent_id: Uuid,
    slug: String,
    display_name: String,
    lifecycle_status: String,
    can_update: bool,
    can_publish: bool,
}

struct CatalogRelease {
    id: String,
    digest: String,
}

#[derive(Serialize, Deserialize)]
struct RawDiagnostic {
    code: String,
    severity: String,
    message: String,
    path: Vec<String>,
}

fn parse_diagnostics(json: &str) -> Vec<AgentDraftDiagnostic> {
    let raw: Vec<RawDiagnostic> =
        serde_json::from_str(json).expect("stored validation_diagnostics is always valid JSON");
    raw.into_iter()
        .map(|value| AgentDraftDiagnostic {
            code: value.code,
            severity: value.severity,
            message: value.message,
            path: value.path,
        })
        .collect()
}

fn diagnostics_json(diagnostics: &[AgentDraftDiagnostic]) -> String {
    let raw: Vec<RawDiagnostic> = diagnostics
        .iter()
        .map(|value| RawDiagnostic {
            code: value.code.clone(),
            severity: value.severity.clone(),
            message: value.message.clone(),
            path: value.path.clone(),
        })
        .collect();
    serde_json::to_string(&raw).expect("diagnostics always serialize")
}

fn other(error: DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

async fn visible_target(
    db: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
    agent: Uuid,
    lock: bool,
) -> Result<Option<Target>, DbErr> {
    let sql = format!(
        "SELECT agent.id AS agent_id, agent.slug, agent.display_name, agent.lifecycle_status \
         FROM agents agent \
         INNER JOIN projects project ON project.id = agent.project_id \
         INNER JOIN organization_memberships membership ON membership.organization_id = project.organization_id \
         WHERE project.id = $1 AND agent.id = $2 AND membership.principal_id = $3 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL{}",
        if lock { " FOR UPDATE OF agent, membership" } else { "" }
    );
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        &sql,
        [project.into(), agent.into(), principal.into()],
    );
    let Some(row) = db.query_one_raw(statement).await? else {
        return Ok(None);
    };
    Ok(Some(Target {
        agent_id: row.try_get_by("agent_id")?,
        slug: row.try_get_by("slug")?,
        display_name: row.try_get_by("display_name")?,
        lifecycle_status: row.try_get_by("lifecycle_status")?,
        can_update: false,
        can_publish: false,
    }))
}

async fn ensure_draft(db: &impl ConnectionTrait, target: &Target) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO agent_drafts (agent_id, document) VALUES ($1, $2::jsonb) ON CONFLICT (agent_id) DO NOTHING",
        [
            target.agent_id.into(),
            canonical_document::default_document(&target.display_name).into(),
        ],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

async fn latest_version_number_or_null(
    db: &impl ConnectionTrait,
    agent: Uuid,
) -> Result<Option<i64>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT MAX(version_number) AS max_version_number FROM agent_versions WHERE agent_id = $1",
        [agent.into()],
    );
    db.query_one_raw(statement)
        .await?
        .expect("MAX(...) always returns exactly one row")
        .try_get_by("max_version_number")
}

async fn load_draft(
    db: &impl ConnectionTrait,
    target: &Target,
    lock: bool,
) -> Result<AgentDraft, DbErr> {
    let sql = format!(
        "SELECT document::text, revision, validation_status, validation_diagnostics::text, validated_at FROM agent_drafts WHERE agent_id = $1{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [target.agent_id.into()]);
    let row = db.query_one_raw(statement).await?;
    let latest_version = latest_version_number_or_null(db, target.agent_id).await?;
    Ok(match row {
        Some(row) => {
            let diagnostics_text: String = row.try_get_by("validation_diagnostics")?;
            AgentDraft {
                agent_id: target.agent_id,
                slug: target.slug.clone(),
                display_name: target.display_name.clone(),
                lifecycle_status: target.lifecycle_status.clone(),
                document: row.try_get_by("document")?,
                revision: row.try_get_by("revision")?,
                validation_status: row.try_get_by("validation_status")?,
                validation_diagnostics: parse_diagnostics(&diagnostics_text),
                validated_at: row.try_get_by("validated_at")?,
                can_update: target.can_update,
                can_publish: target.can_publish,
                latest_version,
            }
        }
        None => AgentDraft {
            agent_id: target.agent_id,
            slug: target.slug.clone(),
            display_name: target.display_name.clone(),
            lifecycle_status: target.lifecycle_status.clone(),
            document: canonical_document::default_document(&target.display_name),
            revision: 1,
            validation_status: "NOT_VALIDATED".to_string(),
            validation_diagnostics: Vec::new(),
            validated_at: None,
            can_update: target.can_update,
            can_publish: target.can_publish,
            latest_version,
        },
    })
}

async fn update_document(
    db: &impl ConnectionTrait,
    agent: Uuid,
    document: &str,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE agent_drafts SET document = $1::jsonb, revision = revision + 1, validation_status = 'NOT_VALIDATED', \
           validation_diagnostics = '[]'::jsonb, validated_at = NULL, updated_at = CURRENT_TIMESTAMP WHERE agent_id = $2",
        [document.into(), agent.into()],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

async fn catalog_reference(
    db: &impl ConnectionTrait,
    reference: &TypedReference,
) -> Result<bool, DbErr> {
    if reference.kind != "model" && reference.kind != "tool" {
        return Ok(false);
    }
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM catalog_definitions definition JOIN catalog_projection_heads head \
           ON head.release_id = definition.release_id AND head.id = 'local' \
         WHERE definition.definition_kind = $1 AND definition.identity = $2 AND definition.version = $3",
        [
            reference.kind.clone().into(),
            reference.identity.clone().into(),
            reference.version.clone().into(),
        ],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
}

async fn resource_reference(
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

async fn dependencies_resolved(
    db: &impl ConnectionTrait,
    project: Uuid,
    references: &[TypedReference],
) -> Result<bool, DbErr> {
    for reference in references {
        if !catalog_reference(db, reference).await?
            && !resource_reference(db, project, reference).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn diagnostics(
    db: &impl ConnectionTrait,
    project: Uuid,
    document: &str,
) -> Result<Vec<AgentDraftDiagnostic>, DbErr> {
    let mut values = canonical_document::validate(document);
    let malformed = values.iter().any(|value| {
        value.code == "DEPENDENCIES_MALFORMED" || value.code == "DEPENDENCY_REFERENCE_INVALID"
    });
    if !malformed {
        let dependencies = canonical_document::dependencies(document);
        if !dependencies_resolved(db, project, &dependencies).await? {
            values.push(AgentDraftDiagnostic {
                code: "DEPENDENCY_UNRESOLVED".to_string(),
                severity: "ERROR".to_string(),
                message:
                    "Every dependency must resolve to an exact local catalog or project version."
                        .to_string(),
                path: vec!["review".to_string(), "dependencies".to_string()],
            });
        }
    }
    Ok(values)
}

async fn validate_document(
    db: &impl ConnectionTrait,
    project: Uuid,
    agent: Uuid,
    document: &str,
) -> Result<(), DbErr> {
    let computed = diagnostics(db, project, document).await?;
    let errors = computed
        .iter()
        .filter(|value| value.severity == "ERROR")
        .count();
    let status = if errors == 0 { "VALID" } else { "INVALID" };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE agent_drafts SET revision = revision + 1, validation_status = $1, validation_diagnostics = $2::jsonb, \
           validated_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE agent_id = $3",
        [status.into(), diagnostics_json(&computed).into(), agent.into()],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

async fn catalog_release(db: &impl ConnectionTrait) -> Result<Option<CatalogRelease>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT release.id, release.source_digest FROM catalog_releases release JOIN catalog_projection_heads head ON head.release_id = release.id WHERE head.id = 'local'",
        [],
    );
    let Some(row) = db.query_one_raw(statement).await? else {
        return Ok(None);
    };
    Ok(Some(CatalogRelease {
        id: row.try_get_by("id")?,
        digest: row.try_get_by("source_digest")?,
    }))
}

async fn next_version_number(db: &impl ConnectionTrait, agent: Uuid) -> Result<i64, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT COALESCE(MAX(version_number), 0) + 1 AS next_version_number FROM agent_versions WHERE agent_id = $1",
        [agent.into()],
    );
    db.query_one_raw(statement)
        .await?
        .expect("COALESCE(...) always returns exactly one row")
        .try_get_by("next_version_number")
}

async fn latest_version_document(
    db: &impl ConnectionTrait,
    agent: Uuid,
) -> Result<Option<String>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT canonical_document::text FROM agent_versions WHERE agent_id = $1 ORDER BY version_number DESC LIMIT 1",
        [agent.into()],
    );
    db.query_one_raw(statement)
        .await?
        .map(|row| row.try_get_by("canonical_document"))
        .transpose()
}

async fn version_row(
    db: &impl ConnectionTrait,
    agent: Uuid,
    version_id: Uuid,
) -> Result<Option<AgentVersion>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT version.id, version.version_number, agent.slug, agent.display_name, version.canonical_document::text, \
           version.content_digest, version.dependency_versions::text, version.catalog_release_id, version.catalog_release_digest, \
           version.published_by, version.published_at \
         FROM agent_versions version JOIN agents agent ON agent.id = version.agent_id WHERE version.agent_id = $1 AND version.id = $2",
        [agent.into(), version_id.into()],
    );
    let Some(row) = db.query_one_raw(statement).await? else {
        return Ok(None);
    };
    let dependency_versions: String = row.try_get_by("dependency_versions")?;
    Ok(Some(AgentVersion {
        id: row.try_get_by("id")?,
        agent_id: agent,
        number: row.try_get_by("version_number")?,
        slug: row.try_get_by("slug")?,
        display_name: row.try_get_by("display_name")?,
        canonical_document: row.try_get_by("canonical_document")?,
        content_digest: row.try_get_by("content_digest")?,
        dependencies: parse_string_array(&dependency_versions),
        catalog_release_id: row.try_get_by("catalog_release_id")?,
        catalog_release_digest: row.try_get_by("catalog_release_digest")?,
        published_by: row.try_get_by("published_by")?,
        published_at: row.try_get_by("published_at")?,
    }))
}

async fn version_for_digest(
    db: &impl ConnectionTrait,
    agent: Uuid,
    digest: &str,
) -> Result<Option<AgentVersion>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id FROM agent_versions WHERE agent_id = $1 AND content_digest = $2",
        [agent.into(), digest.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => version_row(db, agent, row.try_get_by("id")?).await,
        None => Ok(None),
    }
}

async fn organization_of(db: &impl ConnectionTrait, project: Uuid) -> Result<Uuid, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT organization_id FROM projects WHERE id = $1",
        [project.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => row.try_get_by("organization_id"),
        None => Err(DbErr::RecordNotFound(format!(
            "no project with id {project}"
        ))),
    }
}

async fn legacy_audit(
    db: &impl ConnectionTrait,
    agent: Uuid,
    principal: Uuid,
    action: &str,
    revision: i64,
    document: &str,
) -> Result<(), DbErr> {
    let mut values: Vec<sea_orm::Value> = vec![
        Uuid::new_v4().into(),
        agent.into(),
        principal.into(),
        action.into(),
        revision.into(),
        canonical_document::digest(document).into(),
    ];
    values.extend(crate::audit::context::audit_metadata_values());
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO agent_draft_audit_events (id, agent_id, principal_id, action, revision, content_digest, \
           request_id, correlation_id, graphql_operation, source_ip, user_agent) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        values,
    );
    db.execute_raw(statement).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn authoring_audit(
    db: &impl ConnectionTrait,
    project: Uuid,
    agent: Uuid,
    principal: Uuid,
    action: &str,
    revision: Option<i64>,
    version_id: Option<Uuid>,
    document: &str,
) -> Result<(), DbErr> {
    let organization_id = organization_of(db, project).await?;
    let mut values: Vec<sea_orm::Value> = vec![
        Uuid::new_v4().into(),
        agent.into(),
        project.into(),
        principal.into(),
        action.into(),
        revision.into(),
        version_id.into(),
        canonical_document::digest(document).into(),
    ];
    values.extend(crate::audit::context::audit_metadata_values());
    values.push(organization_id.into());
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO agent_authoring_audit_events \
           (id, agent_id, project_id, actor_principal_id, action, revision, version_id, content_digest, \
            request_id, correlation_id, graphql_operation, source_ip, user_agent, organization_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        values,
    );
    db.execute_raw(statement).await?;
    Ok(())
}

async fn project_agent_version_target(
    db: &impl ConnectionTrait,
    version_id: Uuid,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO evaluation_target_projections (project_id, target_kind, target_id, agent_version_id, \
             environment_definition_version_id, logical_environment_class, display_name) \
         SELECT agent.project_id, 'AGENT_VERSION', versioned.id, versioned.id, environment.id, \
             environment.logical_environment_class, agent.display_name \
         FROM agent_versions versioned \
           JOIN agents agent ON agent.id = versioned.agent_id \
           JOIN environment_definition_versions environment ON environment.catalog_release_id = versioned.catalog_release_id \
         WHERE versioned.id = $1 \
         ON CONFLICT (target_kind, target_id, environment_definition_version_id) DO UPDATE \
           SET project_id = EXCLUDED.project_id, agent_version_id = EXCLUDED.agent_version_id, \
               logical_environment_class = EXCLUDED.logical_environment_class, display_name = EXCLUDED.display_name",
        [version_id.into()],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

/// `AGENT_DRAFT.CREATE` and `AGENT_DRAFT.PUBLISH` route to the identical
/// `hasCapability` branch (`active_project && legacy_or_developer`) — Java
/// computes the same boolean under two different capability names, so this is
/// the one function `canCreate` and `canPublish` both call.
async fn tx_agent_draft_create_or_publish(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
) -> Result<bool, DbErr> {
    Ok(
        capability::queries::active_project(txn, project, true).await?
            && capability::queries::legacy_or_developer(txn, principal, project, true).await?,
    )
}

#[async_trait::async_trait]
impl AgentDraftRepository for PgAgentDraftRepository {
    async fn find_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
    ) -> Result<Option<AgentDraft>, RepositoryError> {
        let db = &self.db;
        let Some(found) = visible_target(db, principal, project, agent, false)
            .await
            .map_err(other)?
        else {
            return Ok(None);
        };
        if !capability::has_capability(
            db,
            principal,
            capability::AGENT_VIEW,
            capability::Scope::Project(project),
            false,
        )
        .await
        .map_err(other)?
        {
            return Ok(None);
        }
        let active = found.lifecycle_status == "ACTIVE";
        let can_update = active
            && capability::has_capability(
                db,
                principal,
                capability::AGENT_DRAFT_UPDATE,
                capability::Scope::Project(project),
                false,
            )
            .await
            .map_err(other)?;
        let can_publish = active
            && capability::has_capability(
                db,
                principal,
                capability::AGENT_DRAFT_PUBLISH,
                capability::Scope::Project(project),
                false,
            )
            .await
            .map_err(other)?;
        let target = Target {
            can_update,
            can_publish,
            ..found
        };
        Ok(Some(load_draft(db, &target, false).await.map_err(other)?))
    }

    async fn create_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        display_name: String,
        requested_slug: Option<String>,
    ) -> Result<AgentDraftMutationResult, RepositoryError> {
        if !valid_name(&display_name) {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::invalid_document(),
            ));
        }
        let slug = match requested_slug
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(explicit) => explicit.to_string(),
            None => resource_identity::from_display_name(&display_name).unwrap_or_default(),
        };
        if !resource_identity::is_canonical(&slug) {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::invalid_document(),
            ));
        }

        let txn = self.db.begin().await.map_err(other)?;
        let can_create = tx_agent_draft_create_or_publish(&txn, principal, project)
            .await
            .map_err(other)?;
        if !can_create
            || !capability::queries::active_project(&txn, project, true)
                .await
                .map_err(other)?
        {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::forbidden(),
            ));
        }

        let agent = Uuid::new_v4();
        let insert_statement = Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ($1, $2, $3, $4, 'ACTIVE') ON CONFLICT (project_id, LOWER(slug)) DO NOTHING",
            [
                agent.into(),
                project.into(),
                slug.clone().into(),
                display_name.trim().into(),
            ],
        );
        let inserted = txn.execute_raw(insert_statement).await.map_err(other)?;
        if inserted.rows_affected() == 0 {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::invalid_document(),
            ));
        }

        let can_publish = tx_agent_draft_create_or_publish(&txn, principal, project)
            .await
            .map_err(other)?;
        let target = Target {
            agent_id: agent,
            slug: slug.clone(),
            display_name: display_name.trim().to_string(),
            lifecycle_status: "ACTIVE".to_string(),
            can_update: true,
            can_publish,
        };
        ensure_draft(&txn, &target).await.map_err(other)?;
        let created = load_draft(&txn, &target, true).await.map_err(other)?;
        authoring_audit(
            &txn,
            project,
            agent,
            principal,
            "CREATED",
            Some(created.revision),
            None,
            &created.document,
        )
        .await
        .map_err(other)?;

        txn.commit().await.map_err(other)?;
        Ok(AgentDraftMutationResult::success(created))
    }

    async fn update_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        expected_revision: i64,
        document: String,
    ) -> Result<AgentDraftMutationResult, RepositoryError> {
        let Some(canonical) = canonical_document::canonicalize(Some(&document)) else {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::invalid_document(),
            ));
        };
        self.command(
            principal,
            project,
            agent,
            expected_revision,
            "UPDATED",
            Some(canonical),
        )
        .await
    }

    async fn validate_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        expected_revision: i64,
    ) -> Result<AgentDraftMutationResult, RepositoryError> {
        self.command(
            principal,
            project,
            agent,
            expected_revision,
            "VALIDATED",
            None,
        )
        .await
    }

    async fn publish_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        expected_revision: i64,
        warnings_acknowledged: bool,
    ) -> Result<AgentDraftMutationResult, RepositoryError> {
        let txn = self.db.begin().await.map_err(other)?;

        let found = visible_target(&txn, principal, project, agent, true)
            .await
            .map_err(other)?;
        let can_view = capability::queries::project_visible(&txn, principal, project, true)
            .await
            .map_err(other)?;
        let Some(found) = found.filter(|_| can_view) else {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::not_found(),
            ));
        };
        let can_publish_now = tx_agent_draft_create_or_publish(&txn, principal, project)
            .await
            .map_err(other)?;
        if found.lifecycle_status != "ACTIVE"
            || !can_publish_now
            || !capability::queries::active_project(&txn, project, true)
                .await
                .map_err(other)?
        {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::forbidden(),
            ));
        }
        let can_update = capability::queries::legacy_or_developer(&txn, principal, project, true)
            .await
            .map_err(other)?;
        let target = Target {
            can_update,
            can_publish: true,
            ..found
        };
        ensure_draft(&txn, &target).await.map_err(other)?;
        let draft = load_draft(&txn, &target, true).await.map_err(other)?;
        if draft.revision != expected_revision {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::conflict(agent, expected_revision, draft.revision),
            ));
        }
        let computed_diagnostics = diagnostics(&txn, project, &draft.document)
            .await
            .map_err(other)?;
        if computed_diagnostics
            .iter()
            .any(|value| value.severity == "ERROR")
        {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::invalid_draft(),
            ));
        }
        if !warnings_acknowledged
            && computed_diagnostics
                .iter()
                .any(|value| value.severity == "WARNING")
        {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::warning_acknowledgement_required(),
            ));
        }
        let Some(release) = catalog_release(&txn).await.map_err(other)? else {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::invalid_draft(),
            ));
        };
        let digest = canonical_document::digest(&draft.document);
        if let Some(existing) = version_for_digest(&txn, agent, &digest)
            .await
            .map_err(other)?
        {
            txn.commit().await.map_err(other)?;
            return Ok(AgentDraftMutationResult::published(draft, existing));
        }

        let number = next_version_number(&txn, agent).await.map_err(other)?;
        let version_id = Uuid::new_v4();
        let dependencies: Vec<String> = canonical_document::dependencies(&draft.document)
            .iter()
            .map(TypedReference::value)
            .collect();
        let insert_statement = Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_versions (id, agent_id, version_number, canonical_document, content_digest, dependency_versions, \
                 catalog_release_id, catalog_release_digest, published_by) \
             VALUES ($1, $2, $3, $4::jsonb, $5, $6::jsonb, $7, $8, $9)",
            [
                version_id.into(),
                agent.into(),
                number.into(),
                draft.document.clone().into(),
                digest.clone().into(),
                json_array(&dependencies).into(),
                release.id.clone().into(),
                release.digest.clone().into(),
                principal.into(),
            ],
        );
        txn.execute_raw(insert_statement).await.map_err(other)?;
        project_agent_version_target(&txn, version_id)
            .await
            .map_err(other)?;
        let version = version_row(&txn, agent, version_id)
            .await
            .map_err(other)?
            .expect("the version row just inserted is visible in the same transaction");
        authoring_audit(
            &txn,
            project,
            agent,
            principal,
            "PUBLISHED",
            Some(draft.revision),
            Some(version_id),
            &digest,
        )
        .await
        .map_err(other)?;
        let published_draft = load_draft(&txn, &target, true).await.map_err(other)?;

        txn.commit().await.map_err(other)?;
        Ok(AgentDraftMutationResult::published(
            published_draft,
            version,
        ))
    }

    async fn review_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
    ) -> Result<Option<AgentDraftReview>, RepositoryError> {
        let db = &self.db;
        let Some(found) = visible_target(db, principal, project, agent, false)
            .await
            .map_err(other)?
        else {
            return Ok(None);
        };
        if !capability::has_capability(
            db,
            principal,
            capability::AGENT_VIEW,
            capability::Scope::Project(project),
            false,
        )
        .await
        .map_err(other)?
        {
            return Ok(None);
        }
        let can_update = capability::has_capability(
            db,
            principal,
            capability::AGENT_DRAFT_UPDATE,
            capability::Scope::Project(project),
            false,
        )
        .await
        .map_err(other)?;
        let can_publish = capability::has_capability(
            db,
            principal,
            capability::AGENT_DRAFT_PUBLISH,
            capability::Scope::Project(project),
            false,
        )
        .await
        .map_err(other)?;
        let target = Target {
            can_update,
            can_publish,
            ..found
        };
        let draft = load_draft(db, &target, false).await.map_err(other)?;
        let release = catalog_release(db).await.map_err(other)?;
        let prior = match latest_version_document(db, agent).await.map_err(other)? {
            Some(document) => document,
            None => canonical_document::default_document(&target.display_name),
        };
        Ok(Some(AgentDraftReview {
            content_digest: canonical_document::digest(&draft.document),
            dependencies: canonical_document::dependencies(&draft.document)
                .iter()
                .map(TypedReference::value)
                .collect(),
            catalog_release_id: release
                .as_ref()
                .map(|value| value.id.clone())
                .unwrap_or_else(|| "UNAVAILABLE".to_string()),
            catalog_release_digest: release.map(|value| value.digest).unwrap_or_default(),
            changed_sections: canonical_document::changed_sections(&prior, &draft.document),
            diagnostics: diagnostics(db, project, &draft.document)
                .await
                .map_err(other)?,
            canonical_document: draft.document.clone(),
            draft,
        }))
    }

    async fn compare_versions(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        from_version_id: Uuid,
        to_version_id: Uuid,
    ) -> Result<Option<AgentVersionComparison>, RepositoryError> {
        let db = &self.db;
        if visible_target(db, principal, project, agent, false)
            .await
            .map_err(other)?
            .is_none()
            || !capability::has_capability(
                db,
                principal,
                capability::AGENT_VIEW,
                capability::Scope::Project(project),
                false,
            )
            .await
            .map_err(other)?
        {
            return Ok(None);
        }
        let from = version_row(db, agent, from_version_id)
            .await
            .map_err(other)?;
        let to = version_row(db, agent, to_version_id).await.map_err(other)?;
        match (from, to) {
            (Some(from), Some(to)) => {
                let changed_sections = canonical_document::changed_sections(
                    &from.canonical_document,
                    &to.canonical_document,
                );
                Ok(Some(AgentVersionComparison {
                    from,
                    to,
                    changed_sections,
                }))
            }
            _ => Ok(None),
        }
    }
}

impl PgAgentDraftRepository {
    async fn command(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        expected_revision: i64,
        action: &str,
        replacement: Option<String>,
    ) -> Result<AgentDraftMutationResult, RepositoryError> {
        let txn = self.db.begin().await.map_err(other)?;

        let found = visible_target(&txn, principal, project, agent, true)
            .await
            .map_err(other)?;
        let can_view = capability::queries::project_visible(&txn, principal, project, true)
            .await
            .map_err(other)?;
        let Some(found) = found.filter(|_| can_view) else {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::not_found(),
            ));
        };
        let can_update = capability::queries::legacy_or_developer(&txn, principal, project, true)
            .await
            .map_err(other)?;
        if found.lifecycle_status != "ACTIVE"
            || !can_update
            || !capability::queries::active_project(&txn, project, true)
                .await
                .map_err(other)?
        {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::forbidden(),
            ));
        }
        let can_publish = tx_agent_draft_create_or_publish(&txn, principal, project)
            .await
            .map_err(other)?;
        let target = Target {
            can_update: true,
            can_publish,
            ..found
        };
        ensure_draft(&txn, &target).await.map_err(other)?;
        let current = load_draft(&txn, &target, true).await.map_err(other)?;
        if current.revision != expected_revision {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::conflict(agent, expected_revision, current.revision),
            ));
        }
        match &replacement {
            None => validate_document(&txn, project, agent, &current.document)
                .await
                .map_err(other)?,
            Some(document) => update_document(&txn, agent, document)
                .await
                .map_err(other)?,
        }
        let updated = load_draft(&txn, &target, true).await.map_err(other)?;
        legacy_audit(
            &txn,
            agent,
            principal,
            action,
            updated.revision,
            &updated.document,
        )
        .await
        .map_err(other)?;
        let authoring_action = if action == "UPDATED" {
            "SAVED"
        } else {
            "VALIDATED"
        };
        authoring_audit(
            &txn,
            project,
            agent,
            principal,
            authoring_action,
            Some(updated.revision),
            None,
            &updated.document,
        )
        .await
        .map_err(other)?;
        let result = AgentDraftMutationResult::success(updated);

        match txn.commit().await {
            Ok(()) => Ok(result),
            Err(error) if is_serialization_failure_db(&error) => {
                let retry = self.db.begin().await.map_err(other)?;
                let raced = load_draft(&retry, &target, false).await.map_err(other)?;
                Ok(AgentDraftMutationResult::refused(
                    AgentDraftMutationProblem::conflict(agent, expected_revision, raced.revision),
                ))
            }
            Err(error) => Err(other(error)),
        }
    }
}
