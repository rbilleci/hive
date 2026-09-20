//! Ports `PostgresAgentDraftRepository` in full: `findDraft`, `createDraft`,
//! `updateDraft`/`validateDraft` (the shared `command()`), `publishDraft`,
//! `reviewDraft`, `versions`, `version`, `compareVersions`, and every helper
//! they call, including the DSQL SQLSTATE 40001 commit-race handling in
//! `command()` and the `evaluation_target_projections` maintenance
//! `publishDraft` performs in place of the DSQL-incompatible trigger V039
//! declared (`PostgresEvaluationTargetProjection.projectAgentVersionTarget`).
//!
//! Every write command re-evaluates the current server capability inside the
//! same transaction it mutates in, via `capability::tx`'s locked primitives —
//! see that module's doc comment. Read-only methods call the existing pool-
//! based, unlocked `capability::has_capability` directly instead, matching
//! Java's `lock = false` calls from `findDraft`/`reviewDraft`/`versions`/
//! `version`/`compareVersions`.
//!
//! Every shared helper takes a concrete `&mut PgConnection`, not a generic
//! executor: a `PoolConnection`/`Transaction` both `DerefMut` to
//! `PgConnection`, so `&mut acquired_connection` and `&mut tx` both coerce at
//! the call site, and a helper needing more than one query against the same
//! connection just reborrows (`&mut *conn`) rather than requiring `Clone` —
//! `&mut PgConnection` isn't `Clone`, so a generic `E: PgExecutor<'e> + Clone`
//! parameter (tried first here) fails for every write path. This is the same
//! conclusion `administration` reached: genericizing over the executor type
//! hits lifetime/trait friction that inlining a concrete connection type
//! avoids.
//!
//! An early return before `tx.commit()` relies on `sqlx::Transaction`'s
//! `Drop` implementation to roll back rather than an explicit
//! `tx.rollback().await` at every refusal branch, the same convention
//! `administration` uses: every refusal in this file is checked before any
//! write happens in that same command, so a rollback and Java's own
//! unconditional commit-on-any-return are observationally identical (no data
//! was written either way; only lock release differs, which nothing outside
//! the transaction can observe).

use crate::capability::tx;
use crate::sql::{is_serialization_failure, json_array, parse_string_array};
use hive_application::agent::canonical_document;
use hive_application::agent::{
    AgentDraft, AgentDraftDiagnostic, AgentDraftMutationProblem, AgentDraftMutationResult,
    AgentDraftRepository, AgentDraftRepositoryError as RepositoryError, AgentDraftReview,
    AgentVersion, AgentVersionComparison,
};
use hive_application::configuration::{resource_identity, TypedReference};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, Row};
use std::sync::LazyLock;
use uuid::Uuid;

static VALID_NAME: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Za-z][A-Za-z0-9 _-]{1,80}$").unwrap());

fn valid_name(value: &str) -> bool {
    VALID_NAME.is_match(value)
}

pub struct PgAgentDraftRepository {
    pool: PgPool,
}

impl PgAgentDraftRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
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

fn other(error: sqlx::Error) -> RepositoryError {
    RepositoryError::Other(error.into())
}

async fn visible_target(
    conn: &mut PgConnection,
    principal: Uuid,
    project: Uuid,
    agent: Uuid,
    lock: bool,
) -> Result<Option<Target>, sqlx::Error> {
    let sql = format!(
        "SELECT agent.id AS agent_id, agent.slug, agent.display_name, agent.lifecycle_status \
         FROM agents agent \
         INNER JOIN projects project ON project.id = agent.project_id \
         INNER JOIN organization_memberships membership ON membership.organization_id = project.organization_id \
         WHERE project.id = $1 AND agent.id = $2 AND membership.principal_id = $3 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL{}",
        if lock { " FOR UPDATE OF agent, membership" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(project)
        .bind(agent)
        .bind(principal)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|row| Target {
        agent_id: row.get("agent_id"),
        slug: row.get("slug"),
        display_name: row.get("display_name"),
        lifecycle_status: row.get("lifecycle_status"),
        can_update: false,
        can_publish: false,
    }))
}

async fn ensure_draft(conn: &mut PgConnection, target: &Target) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO agent_drafts (agent_id, document) VALUES ($1, $2::jsonb) ON CONFLICT (agent_id) DO NOTHING")
        .bind(target.agent_id)
        .bind(canonical_document::default_document(&target.display_name))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn latest_version_number_or_null(
    conn: &mut PgConnection,
    agent: Uuid,
) -> Result<Option<i64>, sqlx::Error> {
    let row: (Option<i64>,) =
        sqlx::query_as("SELECT MAX(version_number) FROM agent_versions WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(&mut *conn)
            .await?;
    Ok(row.0)
}

async fn load_draft(
    conn: &mut PgConnection,
    target: &Target,
    lock: bool,
) -> Result<AgentDraft, sqlx::Error> {
    let sql = format!(
        "SELECT document::text, revision, validation_status, validation_diagnostics::text, validated_at FROM agent_drafts WHERE agent_id = $1{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(target.agent_id)
        .fetch_optional(&mut *conn)
        .await?;
    let latest_version = latest_version_number_or_null(conn, target.agent_id).await?;
    Ok(match row {
        Some(row) => {
            let diagnostics_text: String = row.get(3);
            AgentDraft {
                agent_id: target.agent_id,
                slug: target.slug.clone(),
                display_name: target.display_name.clone(),
                lifecycle_status: target.lifecycle_status.clone(),
                document: row.get(0),
                revision: row.get(1),
                validation_status: row.get(2),
                validation_diagnostics: parse_diagnostics(&diagnostics_text),
                validated_at: row.get(4),
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
    conn: &mut PgConnection,
    agent: Uuid,
    document: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE agent_drafts SET document = $1::jsonb, revision = revision + 1, validation_status = 'NOT_VALIDATED', \
           validation_diagnostics = '[]'::jsonb, validated_at = NULL, updated_at = CURRENT_TIMESTAMP WHERE agent_id = $2",
    )
    .bind(document)
    .bind(agent)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn catalog_reference(
    conn: &mut PgConnection,
    reference: &TypedReference,
) -> Result<bool, sqlx::Error> {
    if reference.kind != "model" && reference.kind != "tool" {
        return Ok(false);
    }
    Ok(sqlx::query(
        "SELECT 1 FROM catalog_definitions definition JOIN catalog_projection_heads head \
           ON head.release_id = definition.release_id AND head.id = 'local' \
         WHERE definition.definition_kind = $1 AND definition.identity = $2 AND definition.version = $3",
    )
    .bind(&reference.kind)
    .bind(&reference.identity)
    .bind(&reference.version)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

async fn resource_reference(
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

async fn dependencies_resolved(
    conn: &mut PgConnection,
    project: Uuid,
    references: &[TypedReference],
) -> Result<bool, sqlx::Error> {
    for reference in references {
        if !catalog_reference(conn, reference).await?
            && !resource_reference(conn, project, reference).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn diagnostics(
    conn: &mut PgConnection,
    project: Uuid,
    document: &str,
) -> Result<Vec<AgentDraftDiagnostic>, sqlx::Error> {
    let mut values = canonical_document::validate(document);
    let malformed = values.iter().any(|value| {
        value.code == "DEPENDENCIES_MALFORMED" || value.code == "DEPENDENCY_REFERENCE_INVALID"
    });
    if !malformed {
        let dependencies = canonical_document::dependencies(document);
        if !dependencies_resolved(conn, project, &dependencies).await? {
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
    conn: &mut PgConnection,
    project: Uuid,
    agent: Uuid,
    document: &str,
) -> Result<(), sqlx::Error> {
    let computed = diagnostics(conn, project, document).await?;
    let errors = computed
        .iter()
        .filter(|value| value.severity == "ERROR")
        .count();
    let status = if errors == 0 { "VALID" } else { "INVALID" };
    sqlx::query(
        "UPDATE agent_drafts SET revision = revision + 1, validation_status = $1, validation_diagnostics = $2::jsonb, \
           validated_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE agent_id = $3",
    )
    .bind(status)
    .bind(diagnostics_json(&computed))
    .bind(agent)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn catalog_release(conn: &mut PgConnection) -> Result<Option<CatalogRelease>, sqlx::Error> {
    let row = sqlx::query("SELECT release.id, release.source_digest FROM catalog_releases release JOIN catalog_projection_heads head ON head.release_id = release.id WHERE head.id = 'local'")
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|row| CatalogRelease {
        id: row.get(0),
        digest: row.get(1),
    }))
}

async fn next_version_number(conn: &mut PgConnection, agent: Uuid) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COALESCE(MAX(version_number), 0) + 1 FROM agent_versions WHERE agent_id = $1",
    )
    .bind(agent)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.0)
}

async fn latest_version_document(
    conn: &mut PgConnection,
    agent: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT canonical_document::text FROM agent_versions WHERE agent_id = $1 ORDER BY version_number DESC LIMIT 1").bind(agent).fetch_optional(&mut *conn).await?;
    Ok(row.map(|(document,)| document))
}

async fn version_row(
    conn: &mut PgConnection,
    agent: Uuid,
    version_id: Uuid,
) -> Result<Option<AgentVersion>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT version.id, version.version_number, agent.slug, agent.display_name, version.canonical_document::text, \
           version.content_digest, version.dependency_versions::text, version.catalog_release_id, version.catalog_release_digest, \
           version.published_by, version.published_at \
         FROM agent_versions version JOIN agents agent ON agent.id = version.agent_id WHERE version.agent_id = $1 AND version.id = $2",
    )
    .bind(agent)
    .bind(version_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| {
        let dependency_versions: String = row.get(6);
        AgentVersion {
            id: row.get(0),
            agent_id: agent,
            number: row.get(1),
            slug: row.get(2),
            display_name: row.get(3),
            canonical_document: row.get(4),
            content_digest: row.get(5),
            dependencies: parse_string_array(&dependency_versions),
            catalog_release_id: row.get(7),
            catalog_release_digest: row.get(8),
            published_by: row.get(9),
            published_at: row.get(10),
        }
    }))
}

async fn version_for_digest(
    conn: &mut PgConnection,
    agent: Uuid,
    digest: &str,
) -> Result<Option<AgentVersion>, sqlx::Error> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM agent_versions WHERE agent_id = $1 AND content_digest = $2")
            .bind(agent)
            .bind(digest)
            .fetch_optional(&mut *conn)
            .await?;
    match row {
        Some((id,)) => version_row(conn, agent, id).await,
        None => Ok(None),
    }
}

async fn organization_of(conn: &mut PgConnection, project: Uuid) -> Result<Uuid, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT organization_id FROM projects WHERE id = $1")
        .bind(project)
        .fetch_optional(&mut *conn)
        .await?;
    row.map(|(id,)| id).ok_or(sqlx::Error::RowNotFound)
}

async fn legacy_audit(
    conn: &mut PgConnection,
    agent: Uuid,
    principal: Uuid,
    action: &str,
    revision: i64,
    document: &str,
) -> Result<(), sqlx::Error> {
    crate::audit::bind_audit_metadata(
        sqlx::query(
            "INSERT INTO agent_draft_audit_events (id, agent_id, principal_id, action, revision, content_digest, \
               request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(Uuid::new_v4())
        .bind(agent)
        .bind(principal)
        .bind(action)
        .bind(revision)
        .bind(canonical_document::digest(document)),
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn authoring_audit(
    conn: &mut PgConnection,
    project: Uuid,
    agent: Uuid,
    principal: Uuid,
    action: &str,
    revision: Option<i64>,
    version_id: Option<Uuid>,
    document: &str,
) -> Result<(), sqlx::Error> {
    let organization_id = organization_of(conn, project).await?;
    crate::audit::bind_audit_metadata(
        sqlx::query(
            "INSERT INTO agent_authoring_audit_events \
               (id, agent_id, project_id, actor_principal_id, action, revision, version_id, content_digest, \
                request_id, correlation_id, graphql_operation, source_ip, user_agent, organization_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(Uuid::new_v4())
        .bind(agent)
        .bind(project)
        .bind(principal)
        .bind(action)
        .bind(revision)
        .bind(version_id)
        .bind(canonical_document::digest(document)),
    )
    .bind(organization_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn project_agent_version_target(
    conn: &mut PgConnection,
    version_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
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
    )
    .bind(version_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// `AGENT_DRAFT.CREATE` and `AGENT_DRAFT.PUBLISH` route to the identical
/// `hasCapability` branch (`active_project && legacy_or_developer`) — Java
/// computes the same boolean under two different capability names, so this is
/// the one function `canCreate` and `canPublish` both call.
async fn tx_agent_draft_create_or_publish(
    conn: &mut PgConnection,
    principal: Uuid,
    project: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(tx::active_project(conn, project).await?
        && tx::legacy_or_developer(conn, principal, project).await?)
}

#[async_trait::async_trait]
impl AgentDraftRepository for PgAgentDraftRepository {
    async fn find_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
    ) -> Result<Option<AgentDraft>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        let Some(found) = visible_target(&mut conn, principal, project, agent, false)
            .await
            .map_err(other)?
        else {
            return Ok(None);
        };
        if !crate::capability::has_capability(
            &self.pool,
            principal,
            crate::capability::AGENT_VIEW,
            crate::capability::Scope::Project(project),
            false,
        )
        .await
        .map_err(other)?
        {
            return Ok(None);
        }
        let active = found.lifecycle_status == "ACTIVE";
        let can_update = active
            && crate::capability::has_capability(
                &self.pool,
                principal,
                crate::capability::AGENT_DRAFT_UPDATE,
                crate::capability::Scope::Project(project),
                false,
            )
            .await
            .map_err(other)?;
        let can_publish = active
            && crate::capability::has_capability(
                &self.pool,
                principal,
                crate::capability::AGENT_DRAFT_PUBLISH,
                crate::capability::Scope::Project(project),
                false,
            )
            .await
            .map_err(other)?;
        let target = Target {
            can_update,
            can_publish,
            ..found
        };
        Ok(Some(
            load_draft(&mut conn, &target, false).await.map_err(other)?,
        ))
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

        let mut tx = self.pool.begin().await.map_err(other)?;
        let can_create = tx_agent_draft_create_or_publish(&mut tx, principal, project)
            .await
            .map_err(other)?;
        if !can_create || !tx::active_project(&mut tx, project).await.map_err(other)? {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::forbidden(),
            ));
        }

        let agent = Uuid::new_v4();
        let inserted = sqlx::query("INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ($1, $2, $3, $4, 'ACTIVE') ON CONFLICT (project_id, LOWER(slug)) DO NOTHING")
            .bind(agent)
            .bind(project)
            .bind(&slug)
            .bind(display_name.trim())
            .execute(&mut *tx)
            .await
            .map_err(other)?;
        if inserted.rows_affected() == 0 {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::invalid_document(),
            ));
        }

        let can_publish = tx_agent_draft_create_or_publish(&mut tx, principal, project)
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
        ensure_draft(&mut tx, &target).await.map_err(other)?;
        let created = load_draft(&mut tx, &target, true).await.map_err(other)?;
        authoring_audit(
            &mut tx,
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

        tx.commit().await.map_err(other)?;
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
        let mut tx = self.pool.begin().await.map_err(other)?;

        let found = visible_target(&mut tx, principal, project, agent, true)
            .await
            .map_err(other)?;
        let can_view = tx::project_visible(&mut tx, principal, project)
            .await
            .map_err(other)?;
        let Some(found) = found.filter(|_| can_view) else {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::not_found(),
            ));
        };
        let can_publish_now = tx_agent_draft_create_or_publish(&mut tx, principal, project)
            .await
            .map_err(other)?;
        if found.lifecycle_status != "ACTIVE"
            || !can_publish_now
            || !tx::active_project(&mut tx, project).await.map_err(other)?
        {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::forbidden(),
            ));
        }
        let can_update = tx::legacy_or_developer(&mut tx, principal, project)
            .await
            .map_err(other)?;
        let target = Target {
            can_update,
            can_publish: true,
            ..found
        };
        ensure_draft(&mut tx, &target).await.map_err(other)?;
        let draft = load_draft(&mut tx, &target, true).await.map_err(other)?;
        if draft.revision != expected_revision {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::conflict(agent, expected_revision, draft.revision),
            ));
        }
        let computed_diagnostics = diagnostics(&mut tx, project, &draft.document)
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
        let Some(release) = catalog_release(&mut tx).await.map_err(other)? else {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::invalid_draft(),
            ));
        };
        let digest = canonical_document::digest(&draft.document);
        if let Some(existing) = version_for_digest(&mut tx, agent, &digest)
            .await
            .map_err(other)?
        {
            tx.commit().await.map_err(other)?;
            return Ok(AgentDraftMutationResult::published(draft, existing));
        }

        let number = next_version_number(&mut tx, agent).await.map_err(other)?;
        let version_id = Uuid::new_v4();
        let dependencies: Vec<String> = canonical_document::dependencies(&draft.document)
            .iter()
            .map(TypedReference::value)
            .collect();
        sqlx::query(
            "INSERT INTO agent_versions (id, agent_id, version_number, canonical_document, content_digest, dependency_versions, \
                 catalog_release_id, catalog_release_digest, published_by) \
             VALUES ($1, $2, $3, $4::jsonb, $5, $6::jsonb, $7, $8, $9)",
        )
        .bind(version_id)
        .bind(agent)
        .bind(number)
        .bind(&draft.document)
        .bind(&digest)
        .bind(json_array(&dependencies))
        .bind(&release.id)
        .bind(&release.digest)
        .bind(principal)
        .execute(&mut *tx)
        .await
        .map_err(other)?;
        project_agent_version_target(&mut tx, version_id)
            .await
            .map_err(other)?;
        let version = version_row(&mut tx, agent, version_id)
            .await
            .map_err(other)?
            .expect("the version row just inserted is visible in the same transaction");
        authoring_audit(
            &mut tx,
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
        let published_draft = load_draft(&mut tx, &target, true).await.map_err(other)?;

        tx.commit().await.map_err(other)?;
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
        let mut conn = self.pool.acquire().await.map_err(other)?;
        let Some(found) = visible_target(&mut conn, principal, project, agent, false)
            .await
            .map_err(other)?
        else {
            return Ok(None);
        };
        if !crate::capability::has_capability(
            &self.pool,
            principal,
            crate::capability::AGENT_VIEW,
            crate::capability::Scope::Project(project),
            false,
        )
        .await
        .map_err(other)?
        {
            return Ok(None);
        }
        let can_update = crate::capability::has_capability(
            &self.pool,
            principal,
            crate::capability::AGENT_DRAFT_UPDATE,
            crate::capability::Scope::Project(project),
            false,
        )
        .await
        .map_err(other)?;
        let can_publish = crate::capability::has_capability(
            &self.pool,
            principal,
            crate::capability::AGENT_DRAFT_PUBLISH,
            crate::capability::Scope::Project(project),
            false,
        )
        .await
        .map_err(other)?;
        let target = Target {
            can_update,
            can_publish,
            ..found
        };
        let draft = load_draft(&mut conn, &target, false).await.map_err(other)?;
        let release = catalog_release(&mut conn).await.map_err(other)?;
        let prior = match latest_version_document(&mut conn, agent)
            .await
            .map_err(other)?
        {
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
            diagnostics: diagnostics(&mut conn, project, &draft.document)
                .await
                .map_err(other)?,
            canonical_document: draft.document.clone(),
            draft,
        }))
    }

    async fn versions(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
    ) -> Result<Option<Vec<AgentVersion>>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        if visible_target(&mut conn, principal, project, agent, false)
            .await
            .map_err(other)?
            .is_none()
            || !crate::capability::has_capability(
                &self.pool,
                principal,
                crate::capability::AGENT_VIEW,
                crate::capability::Scope::Project(project),
                false,
            )
            .await
            .map_err(other)?
        {
            return Ok(None);
        }
        let ids: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM agent_versions WHERE agent_id = $1 ORDER BY version_number DESC",
        )
        .bind(agent)
        .fetch_all(&mut *conn)
        .await
        .map_err(other)?;
        let mut values = Vec::with_capacity(ids.len());
        for (id,) in ids {
            if let Some(version) = version_row(&mut conn, agent, id).await.map_err(other)? {
                values.push(version);
            }
        }
        Ok(Some(values))
    }

    async fn version(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        version_id: Uuid,
    ) -> Result<Option<AgentVersion>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        if visible_target(&mut conn, principal, project, agent, false)
            .await
            .map_err(other)?
            .is_none()
            || !crate::capability::has_capability(
                &self.pool,
                principal,
                crate::capability::AGENT_VIEW,
                crate::capability::Scope::Project(project),
                false,
            )
            .await
            .map_err(other)?
        {
            return Ok(None);
        }
        version_row(&mut conn, agent, version_id)
            .await
            .map_err(other)
    }

    async fn compare_versions(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        from_version_id: Uuid,
        to_version_id: Uuid,
    ) -> Result<Option<AgentVersionComparison>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        if visible_target(&mut conn, principal, project, agent, false)
            .await
            .map_err(other)?
            .is_none()
            || !crate::capability::has_capability(
                &self.pool,
                principal,
                crate::capability::AGENT_VIEW,
                crate::capability::Scope::Project(project),
                false,
            )
            .await
            .map_err(other)?
        {
            return Ok(None);
        }
        let from = version_row(&mut conn, agent, from_version_id)
            .await
            .map_err(other)?;
        let to = version_row(&mut conn, agent, to_version_id)
            .await
            .map_err(other)?;
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
        let mut tx = self.pool.begin().await.map_err(other)?;

        let found = visible_target(&mut tx, principal, project, agent, true)
            .await
            .map_err(other)?;
        let can_view = tx::project_visible(&mut tx, principal, project)
            .await
            .map_err(other)?;
        let Some(found) = found.filter(|_| can_view) else {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::not_found(),
            ));
        };
        let can_update = tx::legacy_or_developer(&mut tx, principal, project)
            .await
            .map_err(other)?;
        if found.lifecycle_status != "ACTIVE"
            || !can_update
            || !tx::active_project(&mut tx, project).await.map_err(other)?
        {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::forbidden(),
            ));
        }
        let can_publish = tx_agent_draft_create_or_publish(&mut tx, principal, project)
            .await
            .map_err(other)?;
        let target = Target {
            can_update: true,
            can_publish,
            ..found
        };
        ensure_draft(&mut tx, &target).await.map_err(other)?;
        let current = load_draft(&mut tx, &target, true).await.map_err(other)?;
        if current.revision != expected_revision {
            return Ok(AgentDraftMutationResult::refused(
                AgentDraftMutationProblem::conflict(agent, expected_revision, current.revision),
            ));
        }
        match &replacement {
            None => validate_document(&mut tx, project, agent, &current.document)
                .await
                .map_err(other)?,
            Some(document) => update_document(&mut tx, agent, document)
                .await
                .map_err(other)?,
        }
        let updated = load_draft(&mut tx, &target, true).await.map_err(other)?;
        legacy_audit(
            &mut tx,
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
            &mut tx,
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

        match tx.commit().await {
            Ok(()) => Ok(result),
            Err(error) if is_serialization_failure(&error) => {
                let mut retry = self.pool.begin().await.map_err(other)?;
                let raced = load_draft(&mut retry, &target, false)
                    .await
                    .map_err(other)?;
                Ok(AgentDraftMutationResult::refused(
                    AgentDraftMutationProblem::conflict(agent, expected_revision, raced.revision),
                ))
            }
            Err(error) => Err(other(error)),
        }
    }
}
