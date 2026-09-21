//! The four agent draft commands (`createAgentDraft`, `updateAgentDraft`, `validateAgentDraft`,
//! `publishAgentDraft`) on SeaORM entities. Each runs in one transaction: it re-checks the
//! principal's authority with the evaluator's locking checks, locks the agent and its draft,
//! compares the revision, writes, and records its audit rows before it commits. A command answers
//! with the stored `agent_drafts` / `agent_versions` rows themselves; the GraphQL payload exposes
//! them as the same generated types the reads use.
//!
//! Aurora DSQL reports a lost optimistic race as SQLSTATE 40001 at commit, where Postgres would
//! have blocked on the row lock; `command` turns that into the same revision conflict.
//!
//! A refusal returns before `commit`, and the dropped transaction rolls back.
//!
//! A version's content digest is taken over the draft document as Postgres writes a `jsonb` value
//! as text. Every version published so far was digested that way, and republishing an unchanged
//! draft finds its version by that digest, so the text is read with a `CAST` rather than
//! re-serialized here.

use crate::audit::context::request_metadata;
use crate::capability;
use crate::capability::queries::{for_update_of, organization_memberships_of_project};
pub(super) use crate::configuration::rows::catalog_release;
use crate::configuration::rows::resolved;
use crate::entity::enums::{
    AgentAuthoringAuditAction, AgentDraftAuditAction, AgentLifecycleStatus, DraftValidationStatus,
    EvaluationTargetKind,
};
use crate::entity::{
    agent_authoring_audit_events, agent_draft_audit_events, agent_drafts, agent_versions, agents,
    environment_definition_versions, evaluation_target_projections, organization_memberships,
    projects,
};
use crate::retry::is_serialization_failure_db;
use hive_application::agent::canonical_document;
use hive_application::agent::{
    AgentDraftDiagnostic, AgentDraftMutationProblem, AgentDraftMutationResult,
    AgentDraftRepository, AgentDraftRepositoryError as RepositoryError,
};
use hive_application::configuration::{resource_identity, TypedReference};
use sea_orm::sea_query::{Expr, ExprTrait, Func, IntoTableRef, OnConflict};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, JoinType,
    NotSet, QueryFilter, QuerySelect, Set, TransactionTrait, TryInsertResult,
};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use uuid::Uuid;

static VALID_NAME: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Za-z][A-Za-z0-9 _-]{1,80}$").unwrap());

fn valid_name(value: &str) -> bool {
    VALID_NAME.is_match(value)
}

type MutationResult = AgentDraftMutationResult<agent_drafts::Model, agent_versions::Model>;

pub struct PgAgentDraftRepository {
    db: DatabaseConnection,
}

impl PgAgentDraftRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

/// A validation diagnostic as `agent_drafts.validation_diagnostics` stores it.
#[derive(Serialize, Deserialize)]
struct StoredDiagnostic {
    code: String,
    severity: String,
    message: String,
    path: Vec<String>,
}

fn diagnostics_json(diagnostics: &[AgentDraftDiagnostic]) -> serde_json::Value {
    let stored: Vec<StoredDiagnostic> = diagnostics
        .iter()
        .map(|value| StoredDiagnostic {
            code: value.code.clone(),
            severity: value.severity.clone(),
            message: value.message.clone(),
            path: value.path.clone(),
        })
        .collect();
    serde_json::to_value(stored).expect("diagnostics always serialize")
}

fn other(error: DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

fn refused(problem: AgentDraftMutationProblem) -> Result<MutationResult, RepositoryError> {
    Ok(AgentDraftMutationResult::refused(problem))
}

/// A stored JSON document as the text the application's document rules read.
pub(super) fn document_text(document: &serde_json::Value) -> String {
    serde_json::to_string(document).expect("a stored JSON document always serializes")
}

fn document_value(document: &str) -> serde_json::Value {
    serde_json::from_str(document).expect("a canonical document is always valid JSON")
}

/// The agent, when it is in `project` and `principal` is an active member of the project's
/// organization. With `lock`, the agent and membership rows are locked `FOR UPDATE`.
async fn visible_agent(
    db: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
    agent: Uuid,
    lock: bool,
) -> Result<Option<agents::Model>, DbErr> {
    let select = agents::Entity::find()
        .inner_join(projects::Entity)
        .join(JoinType::InnerJoin, organization_memberships_of_project())
        .filter(projects::Column::Id.eq(project))
        .filter(agents::Column::Id.eq(agent))
        .filter(organization_memberships::Column::PrincipalId.eq(principal))
        .filter(
            Expr::col(organization_memberships::Column::StartedAt.as_column_ref())
                .lte(Expr::current_timestamp()),
        )
        .filter(organization_memberships::Column::EndedAt.is_null());
    let select = if lock {
        for_update_of(
            select,
            [
                agents::Entity.into_table_ref(),
                organization_memberships::Entity.into_table_ref(),
            ],
        )
    } else {
        select
    };
    // Every matching row is fetched, so a locking select locks every membership it matches.
    Ok(select.all(db).await?.into_iter().next())
}

/// The draft every agent starts from, as a row that is not stored yet.
pub(super) fn default_draft(agent: &agents::Model) -> agent_drafts::Model {
    agent_drafts::Model {
        document: document_value(&canonical_document::default_document(&agent.display_name)),
        revision: 1,
        validation_status: DraftValidationStatus::NotValidated,
        validation_diagnostics: serde_json::Value::Array(Vec::new()),
        validated_at: None,
        updated_at: chrono::Utc::now().into(),
        agent_id: agent.id,
    }
}

/// Stores the agent's default draft unless it already has one.
async fn ensure_draft(db: &impl ConnectionTrait, agent: &agents::Model) -> Result<(), DbErr> {
    let draft = agent_drafts::ActiveModel {
        agent_id: Set(agent.id),
        document: Set(default_draft(agent).document),
        ..Default::default()
    };
    agent_drafts::Entity::insert(draft)
        .on_conflict(
            OnConflict::column(agent_drafts::Column::AgentId)
                .do_nothing()
                .to_owned(),
        )
        .try_insert()
        .exec_without_returning(db)
        .await?;
    Ok(())
}

/// The agent's draft, locked `FOR UPDATE`. `ensure_draft` ran first, so it exists.
async fn locked_draft(
    db: &impl ConnectionTrait,
    agent: Uuid,
) -> Result<agent_drafts::Model, DbErr> {
    agent_drafts::Entity::find_by_id(agent)
        .lock_exclusive()
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no draft for agent {agent}")))
}

/// The stored draft document as Postgres writes it as text; `None` when no draft is stored.
pub(super) async fn stored_document_text(
    db: &impl ConnectionTrait,
    agent: Uuid,
) -> Result<Option<String>, DbErr> {
    agent_drafts::Entity::find_by_id(agent)
        .select_only()
        .expr_as(
            Expr::col(agent_drafts::Column::Document.as_column_ref()).cast_as("text"),
            "document_text",
        )
        .into_tuple::<String>()
        .one(db)
        .await
}

/// Replaces the document of a draft still at `expected_revision`; `false` when it moved on.
async fn update_document(
    db: &impl ConnectionTrait,
    agent: Uuid,
    expected_revision: i64,
    document: &str,
) -> Result<bool, DbErr> {
    let updated = agent_drafts::Entity::update_many()
        .col_expr(
            agent_drafts::Column::Document,
            Expr::value(document_value(document)),
        )
        .col_expr(
            agent_drafts::Column::Revision,
            Expr::col(agent_drafts::Column::Revision).add(1),
        )
        .col_expr(
            agent_drafts::Column::ValidationStatus,
            Expr::value(DraftValidationStatus::NotValidated.to_value()),
        )
        .col_expr(
            agent_drafts::Column::ValidationDiagnostics,
            Expr::value(serde_json::Value::Array(Vec::new())),
        )
        .col_expr(
            agent_drafts::Column::ValidatedAt,
            Expr::value(Option::<chrono::DateTime<chrono::FixedOffset>>::None),
        )
        .col_expr(agent_drafts::Column::UpdatedAt, Expr::current_timestamp())
        .filter(agent_drafts::Column::AgentId.eq(agent))
        .filter(agent_drafts::Column::Revision.eq(expected_revision))
        .exec(db)
        .await?;
    Ok(updated.rows_affected == 1)
}

/// The document's own diagnostics, plus one when a dependency does not resolve to an exact local
/// catalog or project version.
pub(super) async fn diagnostics(
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
        if !resolved(db, project, &dependencies).await? {
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

/// Records the validation of a draft still at `expected_revision`; `false` when it moved on.
async fn validate_document(
    db: &impl ConnectionTrait,
    project: Uuid,
    agent: Uuid,
    expected_revision: i64,
    document: &str,
) -> Result<bool, DbErr> {
    let computed = diagnostics(db, project, document).await?;
    let valid = computed.iter().all(|value| value.severity != "ERROR");
    let status = if valid {
        DraftValidationStatus::Valid
    } else {
        DraftValidationStatus::Invalid
    };
    let updated = agent_drafts::Entity::update_many()
        .col_expr(
            agent_drafts::Column::Revision,
            Expr::col(agent_drafts::Column::Revision).add(1),
        )
        .col_expr(
            agent_drafts::Column::ValidationStatus,
            Expr::value(status.to_value()),
        )
        .col_expr(
            agent_drafts::Column::ValidationDiagnostics,
            Expr::value(diagnostics_json(&computed)),
        )
        .col_expr(agent_drafts::Column::ValidatedAt, Expr::current_timestamp())
        .col_expr(agent_drafts::Column::UpdatedAt, Expr::current_timestamp())
        .filter(agent_drafts::Column::AgentId.eq(agent))
        .filter(agent_drafts::Column::Revision.eq(expected_revision))
        .exec(db)
        .await?;
    Ok(updated.rows_affected == 1)
}

/// The agent's highest published version number.
async fn latest_version_number(
    db: &impl ConnectionTrait,
    agent: Uuid,
) -> Result<Option<i64>, DbErr> {
    Ok(agent_versions::Entity::find()
        .filter(agent_versions::Column::AgentId.eq(agent))
        .select_only()
        .column_as(agent_versions::Column::VersionNumber.max(), "latest")
        .into_tuple::<Option<i64>>()
        .one(db)
        .await?
        .flatten())
}

/// The agent's newest published version.
pub(super) async fn latest_version(
    db: &impl ConnectionTrait,
    agent: Uuid,
) -> Result<Option<agent_versions::Model>, DbErr> {
    use sea_orm::QueryOrder;
    agent_versions::Entity::find()
        .filter(agent_versions::Column::AgentId.eq(agent))
        .order_by_desc(agent_versions::Column::VersionNumber)
        .one(db)
        .await
}

async fn version_for_digest(
    db: &impl ConnectionTrait,
    agent: Uuid,
    digest: &str,
) -> Result<Option<agent_versions::Model>, DbErr> {
    agent_versions::Entity::find()
        .filter(agent_versions::Column::AgentId.eq(agent))
        .filter(agent_versions::Column::ContentDigest.eq(digest))
        .one(db)
        .await
}

async fn legacy_audit(
    db: &impl ConnectionTrait,
    principal: Uuid,
    action: AgentDraftAuditAction,
    draft: &agent_drafts::Model,
    content_digest: String,
) -> Result<(), DbErr> {
    let metadata = request_metadata();
    let event = agent_draft_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        agent_id: Set(draft.agent_id),
        principal_id: Set(principal),
        action: Set(action),
        revision: Set(draft.revision),
        content_digest: Set(content_digest),
        occurred_at: NotSet,
        request_id: Set(metadata.request_id),
        correlation_id: Set(metadata.correlation_id),
        graphql_operation: Set(metadata.graphql_operation),
        source_ip: Set(metadata.source_ip),
        user_agent: Set(metadata.user_agent),
    };
    agent_draft_audit_events::Entity::insert(event)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn authoring_audit(
    db: &impl ConnectionTrait,
    project: Uuid,
    agent: Uuid,
    principal: Uuid,
    action: AgentAuthoringAuditAction,
    revision: i64,
    version_id: Option<Uuid>,
    content_digest: String,
) -> Result<(), DbErr> {
    let organization_id = capability::queries::project_organization(db, project, false)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no project with id {project}")))?;
    let metadata = request_metadata();
    let event = agent_authoring_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        agent_id: Set(agent),
        project_id: Set(project),
        actor_principal_id: Set(principal),
        action: Set(action),
        revision: Set(Some(revision)),
        version_id: Set(version_id),
        content_digest: Set(content_digest),
        occurred_at: NotSet,
        request_id: Set(metadata.request_id),
        correlation_id: Set(metadata.correlation_id),
        graphql_operation: Set(metadata.graphql_operation),
        source_ip: Set(metadata.source_ip),
        user_agent: Set(metadata.user_agent),
        organization_id: Set(Some(organization_id)),
    };
    agent_authoring_audit_events::Entity::insert(event)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

/// Makes a published version an evaluation target in every environment of its catalog release.
/// Aurora DSQL has no triggers, so publication maintains the projection itself.
async fn project_agent_version_target(
    db: &impl ConnectionTrait,
    agent: &agents::Model,
    version: &agent_versions::Model,
) -> Result<(), DbErr> {
    let environments = environment_definition_versions::Entity::find()
        .filter(
            environment_definition_versions::Column::CatalogReleaseId
                .eq(version.catalog_release_id.clone()),
        )
        .all(db)
        .await?;
    if environments.is_empty() {
        return Ok(());
    }
    let targets =
        environments
            .into_iter()
            .map(|environment| evaluation_target_projections::ActiveModel {
                project_id: Set(agent.project_id),
                target_kind: Set(EvaluationTargetKind::AgentVersion),
                target_id: Set(version.id),
                agent_version_id: Set(version.id),
                environment_definition_version_id: Set(environment.id),
                logical_environment_class: Set(environment.logical_environment_class),
                display_name: Set(agent.display_name.clone()),
            });
    evaluation_target_projections::Entity::insert_many(targets)
        .on_conflict(
            OnConflict::columns([
                evaluation_target_projections::Column::TargetKind,
                evaluation_target_projections::Column::TargetId,
                evaluation_target_projections::Column::EnvironmentDefinitionVersionId,
            ])
            .update_columns([
                evaluation_target_projections::Column::ProjectId,
                evaluation_target_projections::Column::AgentVersionId,
                evaluation_target_projections::Column::LogicalEnvironmentClass,
                evaluation_target_projections::Column::DisplayName,
            ])
            .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
    Ok(())
}

/// `AGENT_DRAFT.CREATE` and `AGENT_DRAFT.PUBLISH` are the same rule: an active project and an
/// editor role in it.
async fn can_create_or_publish(
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
    type Draft = agent_drafts::Model;
    type Version = agent_versions::Model;

    async fn create_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        display_name: String,
        requested_slug: Option<String>,
    ) -> Result<MutationResult, RepositoryError> {
        if !valid_name(&display_name) {
            return refused(AgentDraftMutationProblem::invalid_document());
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
            return refused(AgentDraftMutationProblem::invalid_document());
        }

        let txn = self.db.begin().await.map_err(other)?;
        let can_create = can_create_or_publish(&txn, principal, project)
            .await
            .map_err(other)?;
        if !can_create
            || !capability::queries::active_project(&txn, project, true)
                .await
                .map_err(other)?
        {
            return refused(AgentDraftMutationProblem::forbidden());
        }

        let agent = agents::Model {
            project_id: project,
            slug,
            display_name: display_name.trim().to_string(),
            lifecycle_status: AgentLifecycleStatus::Active,
            id: Uuid::new_v4(),
        };
        // A project's agent slugs are unique without regard to case.
        let inserted = agents::Entity::insert(agents::ActiveModel {
            project_id: Set(agent.project_id),
            slug: Set(agent.slug.clone()),
            display_name: Set(agent.display_name.clone()),
            lifecycle_status: Set(agent.lifecycle_status),
            id: Set(agent.id),
        })
        .on_conflict(
            OnConflict::new()
                .expr(Expr::col(agents::Column::ProjectId))
                .expr(Func::lower(Expr::col(agents::Column::Slug)))
                .do_nothing()
                .to_owned(),
        )
        .try_insert()
        .exec_without_returning(&txn)
        .await
        .map_err(other)?;
        if !matches!(inserted, TryInsertResult::Inserted(1)) {
            return refused(AgentDraftMutationProblem::invalid_document());
        }

        ensure_draft(&txn, &agent).await.map_err(other)?;
        let created = locked_draft(&txn, agent.id).await.map_err(other)?;
        let digest = stored_digest(&txn, agent.id).await.map_err(other)?;
        authoring_audit(
            &txn,
            project,
            agent.id,
            principal,
            AgentAuthoringAuditAction::Created,
            created.revision,
            None,
            digest,
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
    ) -> Result<MutationResult, RepositoryError> {
        let Some(canonical) = canonical_document::canonicalize(Some(&document)) else {
            return refused(AgentDraftMutationProblem::invalid_document());
        };
        self.command(
            principal,
            project,
            agent,
            expected_revision,
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
    ) -> Result<MutationResult, RepositoryError> {
        self.command(principal, project, agent, expected_revision, None)
            .await
    }

    async fn publish_draft(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        expected_revision: i64,
        warnings_acknowledged: bool,
    ) -> Result<MutationResult, RepositoryError> {
        let txn = self.db.begin().await.map_err(other)?;

        let found = visible_agent(&txn, principal, project, agent, true)
            .await
            .map_err(other)?;
        let can_view = capability::queries::project_visible(&txn, principal, project, true)
            .await
            .map_err(other)?;
        let Some(found) = found.filter(|_| can_view) else {
            return refused(AgentDraftMutationProblem::not_found());
        };
        let can_publish = can_create_or_publish(&txn, principal, project)
            .await
            .map_err(other)?;
        if found.lifecycle_status != AgentLifecycleStatus::Active
            || !can_publish
            || !capability::queries::active_project(&txn, project, true)
                .await
                .map_err(other)?
        {
            return refused(AgentDraftMutationProblem::forbidden());
        }
        ensure_draft(&txn, &found).await.map_err(other)?;
        let draft = locked_draft(&txn, agent).await.map_err(other)?;
        if draft.revision != expected_revision {
            return refused(AgentDraftMutationProblem::conflict(
                agent,
                expected_revision,
                draft.revision,
            ));
        }
        let document = document_text(&draft.document);
        let computed = diagnostics(&txn, project, &document).await.map_err(other)?;
        if computed.iter().any(|value| value.severity == "ERROR") {
            return refused(AgentDraftMutationProblem::invalid_draft());
        }
        if !warnings_acknowledged && computed.iter().any(|value| value.severity == "WARNING") {
            return refused(AgentDraftMutationProblem::warning_acknowledgement_required());
        }
        let Some(release) = catalog_release(&txn).await.map_err(other)? else {
            return refused(AgentDraftMutationProblem::invalid_draft());
        };
        let digest = stored_digest(&txn, agent).await.map_err(other)?;
        if let Some(existing) = version_for_digest(&txn, agent, &digest)
            .await
            .map_err(other)?
        {
            txn.commit().await.map_err(other)?;
            return Ok(AgentDraftMutationResult::published(draft, existing));
        }

        let number = latest_version_number(&txn, agent)
            .await
            .map_err(other)?
            .unwrap_or(0)
            + 1;
        let dependencies: Vec<String> = canonical_document::dependencies(&document)
            .iter()
            .map(TypedReference::value)
            .collect();
        let version = agent_versions::Entity::insert(agent_versions::ActiveModel {
            agent_id: Set(agent),
            version_number: Set(number),
            canonical_document: Set(draft.document.clone()),
            content_digest: Set(digest.clone()),
            dependency_versions: Set(serde_json::json!(dependencies)),
            catalog_release_id: Set(release.id),
            catalog_release_digest: Set(release.source_digest),
            published_by: Set(principal),
            published_at: NotSet,
            id: Set(Uuid::new_v4()),
        })
        .exec_with_returning(&txn)
        .await
        .map_err(other)?;
        project_agent_version_target(&txn, &found, &version)
            .await
            .map_err(other)?;
        // The publication event has always carried the digest of the version's digest text, not
        // the version's digest itself; the audit history is kept comparable.
        authoring_audit(
            &txn,
            project,
            agent,
            principal,
            AgentAuthoringAuditAction::Published,
            draft.revision,
            Some(version.id),
            canonical_document::digest(&digest),
        )
        .await
        .map_err(other)?;
        let published_draft = locked_draft(&txn, agent).await.map_err(other)?;

        txn.commit().await.map_err(other)?;
        Ok(AgentDraftMutationResult::published(
            published_draft,
            version,
        ))
    }
}

/// The digest of the agent's stored draft document.
async fn stored_digest(db: &impl ConnectionTrait, agent: Uuid) -> Result<String, DbErr> {
    let text = stored_document_text(db, agent)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no draft for agent {agent}")))?;
    Ok(canonical_document::digest(&text))
}

impl PgAgentDraftRepository {
    /// `updateAgentDraft` when `replacement` holds the new canonical document, otherwise
    /// `validateAgentDraft`.
    async fn command(
        &self,
        principal: Uuid,
        project: Uuid,
        agent: Uuid,
        expected_revision: i64,
        replacement: Option<String>,
    ) -> Result<MutationResult, RepositoryError> {
        let txn = self.db.begin().await.map_err(other)?;

        let found = visible_agent(&txn, principal, project, agent, true)
            .await
            .map_err(other)?;
        let can_view = capability::queries::project_visible(&txn, principal, project, true)
            .await
            .map_err(other)?;
        let Some(found) = found.filter(|_| can_view) else {
            return refused(AgentDraftMutationProblem::not_found());
        };
        let can_update = capability::queries::legacy_or_developer(&txn, principal, project, true)
            .await
            .map_err(other)?;
        if found.lifecycle_status != AgentLifecycleStatus::Active
            || !can_update
            || !capability::queries::active_project(&txn, project, true)
                .await
                .map_err(other)?
        {
            return refused(AgentDraftMutationProblem::forbidden());
        }
        ensure_draft(&txn, &found).await.map_err(other)?;
        let current = locked_draft(&txn, agent).await.map_err(other)?;
        if current.revision != expected_revision {
            return refused(AgentDraftMutationProblem::conflict(
                agent,
                expected_revision,
                current.revision,
            ));
        }
        let (applied, legacy_action, authoring_action) = match &replacement {
            Some(document) => (
                update_document(&txn, agent, expected_revision, document)
                    .await
                    .map_err(other)?,
                AgentDraftAuditAction::Updated,
                AgentAuthoringAuditAction::Saved,
            ),
            None => (
                validate_document(
                    &txn,
                    project,
                    agent,
                    expected_revision,
                    &document_text(&current.document),
                )
                .await
                .map_err(other)?,
                AgentDraftAuditAction::Validated,
                AgentAuthoringAuditAction::Validated,
            ),
        };
        let updated = locked_draft(&txn, agent).await.map_err(other)?;
        if !applied {
            return refused(AgentDraftMutationProblem::conflict(
                agent,
                expected_revision,
                updated.revision,
            ));
        }
        let digest = stored_digest(&txn, agent).await.map_err(other)?;
        legacy_audit(&txn, principal, legacy_action, &updated, digest.clone())
            .await
            .map_err(other)?;
        authoring_audit(
            &txn,
            project,
            agent,
            principal,
            authoring_action,
            updated.revision,
            None,
            digest,
        )
        .await
        .map_err(other)?;

        match txn.commit().await {
            Ok(()) => Ok(AgentDraftMutationResult::success(updated)),
            Err(error) if is_serialization_failure_db(&error) => {
                let retry = self.db.begin().await.map_err(other)?;
                let raced = agent_drafts::Entity::find_by_id(agent)
                    .one(&retry)
                    .await
                    .map_err(other)?
                    .map_or(1, |draft| draft.revision);
                refused(AgentDraftMutationProblem::conflict(
                    agent,
                    expected_revision,
                    raced,
                ))
            }
            Err(error) => Err(other(error)),
        }
    }
}
