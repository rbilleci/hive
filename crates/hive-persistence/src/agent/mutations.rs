//! The four agent draft commands (`createAgentDraft`, `updateAgentDraft`, `validateAgentDraft`,
//! `publishAgentDraft`). Each runs in one transaction: it re-checks the principal's authority with
//! the evaluator's locking checks, locks the agent and its draft, compares the revision, writes,
//! and records its audit rows before it commits. A command answers with the stored `agent_drafts`
//! / `agent_versions` rows themselves; the GraphQL payload exposes them as the same generated
//! types the reads use.
//!
//! Aurora DSQL reports a lost optimistic race as SQLSTATE 40001 at commit, where Postgres would
//! have blocked on the row lock; `update_or_validate` turns that into the same revision conflict.
//!
//! A refusal returns before `commit`, and the dropped transaction rolls back.

use super::rows::{
    authoring_audit, can_create_or_publish, catalog_release, diagnostics, document_text,
    ensure_draft, latest_version_number, legacy_audit, locked_draft, project_agent_version_target,
    stored_digest, update_document, validate_document, version_for_digest, visible_agent,
    AuthoringEvent,
};
use crate::capability;
use crate::entity::enums::{
    AgentAuthoringAuditAction, AgentDraftAuditAction, AgentLifecycleStatus,
};
use crate::entity::{agent_drafts, agent_versions, agents};
use crate::error::repository_error;
use crate::retry;
use hive_application::agent::canonical_document;
use hive_application::agent::{AgentDraftMutationProblem, AgentDraftMutationResult};
use hive_application::configuration::{resource_identity, TypedReference};
use hive_application::RepositoryError;
use sea_orm::sea_query::{Expr, Func, OnConflict};
use sea_orm::{DatabaseConnection, EntityTrait, NotSet, Set, TransactionTrait, TryInsertResult};
use std::sync::LazyLock;
use uuid::Uuid;

pub(super) type MutationResult =
    AgentDraftMutationResult<agent_drafts::Model, agent_versions::Model>;

static VALID_NAME: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Za-z][A-Za-z0-9 _-]{1,80}$").unwrap());

fn valid_name(value: &str) -> bool {
    VALID_NAME.is_match(value)
}

fn refused(problem: AgentDraftMutationProblem) -> Result<MutationResult, RepositoryError> {
    Ok(AgentDraftMutationResult::refused(problem))
}

pub(super) async fn create_draft(
    db: &DatabaseConnection,
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

    let txn = db.begin().await.map_err(repository_error)?;
    let can_create = can_create_or_publish(&txn, principal, project)
        .await
        .map_err(repository_error)?;
    if !can_create
        || !capability::queries::active_project(&txn, project, true)
            .await
            .map_err(repository_error)?
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
    .map_err(repository_error)?;
    if !matches!(inserted, TryInsertResult::Inserted(1)) {
        return refused(AgentDraftMutationProblem::invalid_document());
    }

    ensure_draft(&txn, &agent).await.map_err(repository_error)?;
    let created = locked_draft(&txn, agent.id)
        .await
        .map_err(repository_error)?;
    let digest = stored_digest(&txn, agent.id)
        .await
        .map_err(repository_error)?;
    authoring_audit(
        &txn,
        AuthoringEvent {
            project,
            agent: agent.id,
            principal,
            action: AgentAuthoringAuditAction::Created,
            revision: created.revision,
            version_id: None,
            content_digest: digest,
        },
    )
    .await
    .map_err(repository_error)?;

    txn.commit().await.map_err(repository_error)?;
    Ok(AgentDraftMutationResult::success(created))
}

pub(super) async fn update_draft(
    db: &DatabaseConnection,
    principal: Uuid,
    project: Uuid,
    agent: Uuid,
    expected_revision: i64,
    document: String,
) -> Result<MutationResult, RepositoryError> {
    let Some(canonical) = canonical_document::canonicalize(Some(&document)) else {
        return refused(AgentDraftMutationProblem::invalid_document());
    };
    update_or_validate(
        db,
        principal,
        project,
        agent,
        expected_revision,
        Some(canonical),
    )
    .await
}

pub(super) async fn validate_draft(
    db: &DatabaseConnection,
    principal: Uuid,
    project: Uuid,
    agent: Uuid,
    expected_revision: i64,
) -> Result<MutationResult, RepositoryError> {
    update_or_validate(db, principal, project, agent, expected_revision, None).await
}

pub(super) async fn publish_draft(
    db: &DatabaseConnection,
    principal: Uuid,
    project: Uuid,
    agent: Uuid,
    expected_revision: i64,
    warnings_acknowledged: bool,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(repository_error)?;

    let found = visible_agent(&txn, principal, project, agent, true)
        .await
        .map_err(repository_error)?;
    let can_view = capability::queries::project_visible(&txn, principal, project, true)
        .await
        .map_err(repository_error)?;
    let Some(found) = found.filter(|_| can_view) else {
        return refused(AgentDraftMutationProblem::not_found());
    };
    let can_publish = can_create_or_publish(&txn, principal, project)
        .await
        .map_err(repository_error)?;
    if found.lifecycle_status != AgentLifecycleStatus::Active
        || !can_publish
        || !capability::queries::active_project(&txn, project, true)
            .await
            .map_err(repository_error)?
    {
        return refused(AgentDraftMutationProblem::forbidden());
    }
    ensure_draft(&txn, &found).await.map_err(repository_error)?;
    let draft = locked_draft(&txn, agent).await.map_err(repository_error)?;
    if draft.revision != expected_revision {
        return refused(AgentDraftMutationProblem::conflict(
            agent,
            expected_revision,
            draft.revision,
        ));
    }
    let document = document_text(&draft.document);
    let computed = diagnostics(&txn, project, &document)
        .await
        .map_err(repository_error)?;
    if computed.iter().any(|value| value.severity == "ERROR") {
        return refused(AgentDraftMutationProblem::invalid_draft());
    }
    if !warnings_acknowledged && computed.iter().any(|value| value.severity == "WARNING") {
        return refused(AgentDraftMutationProblem::warning_acknowledgement_required());
    }
    let Some(release) = catalog_release(&txn).await.map_err(repository_error)? else {
        return refused(AgentDraftMutationProblem::invalid_draft());
    };
    let digest = stored_digest(&txn, agent).await.map_err(repository_error)?;
    if let Some(existing) = version_for_digest(&txn, agent, &digest)
        .await
        .map_err(repository_error)?
    {
        txn.commit().await.map_err(repository_error)?;
        return Ok(AgentDraftMutationResult::published(draft, existing));
    }

    let number = latest_version_number(&txn, agent)
        .await
        .map_err(repository_error)?
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
    .map_err(repository_error)?;
    project_agent_version_target(&txn, &found, &version)
        .await
        .map_err(repository_error)?;
    // The publication event has always carried the digest of the version's digest text, not
    // the version's digest itself; the audit history is kept comparable.
    authoring_audit(
        &txn,
        AuthoringEvent {
            project,
            agent,
            principal,
            action: AgentAuthoringAuditAction::Published,
            revision: draft.revision,
            version_id: Some(version.id),
            content_digest: canonical_document::digest(&digest),
        },
    )
    .await
    .map_err(repository_error)?;
    let published_draft = locked_draft(&txn, agent).await.map_err(repository_error)?;

    txn.commit().await.map_err(repository_error)?;
    Ok(AgentDraftMutationResult::published(
        published_draft,
        version,
    ))
}

/// `updateAgentDraft` when `replacement` holds the new canonical document, otherwise
/// `validateAgentDraft`.
async fn update_or_validate(
    db: &DatabaseConnection,
    principal: Uuid,
    project: Uuid,
    agent: Uuid,
    expected_revision: i64,
    replacement: Option<String>,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(repository_error)?;

    let found = visible_agent(&txn, principal, project, agent, true)
        .await
        .map_err(repository_error)?;
    let can_view = capability::queries::project_visible(&txn, principal, project, true)
        .await
        .map_err(repository_error)?;
    let Some(found) = found.filter(|_| can_view) else {
        return refused(AgentDraftMutationProblem::not_found());
    };
    let can_update = capability::queries::legacy_or_developer(&txn, principal, project, true)
        .await
        .map_err(repository_error)?;
    if found.lifecycle_status != AgentLifecycleStatus::Active
        || !can_update
        || !capability::queries::active_project(&txn, project, true)
            .await
            .map_err(repository_error)?
    {
        return refused(AgentDraftMutationProblem::forbidden());
    }
    ensure_draft(&txn, &found).await.map_err(repository_error)?;
    let current = locked_draft(&txn, agent).await.map_err(repository_error)?;
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
                .map_err(repository_error)?,
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
            .map_err(repository_error)?,
            AgentDraftAuditAction::Validated,
            AgentAuthoringAuditAction::Validated,
        ),
    };
    let updated = locked_draft(&txn, agent).await.map_err(repository_error)?;
    if !applied {
        return refused(AgentDraftMutationProblem::conflict(
            agent,
            expected_revision,
            updated.revision,
        ));
    }
    let digest = stored_digest(&txn, agent).await.map_err(repository_error)?;
    legacy_audit(&txn, principal, legacy_action, &updated, digest.clone())
        .await
        .map_err(repository_error)?;
    authoring_audit(
        &txn,
        AuthoringEvent {
            project,
            agent,
            principal,
            action: authoring_action,
            revision: updated.revision,
            version_id: None,
            content_digest: digest,
        },
    )
    .await
    .map_err(repository_error)?;

    let raced = retry::committed(db, txn, async |retry| {
        Ok(agent_drafts::Entity::find_by_id(agent)
            .one(retry)
            .await?
            .map_or(1, |draft| draft.revision))
    })
    .await
    .map_err(repository_error)?;
    match raced {
        None => Ok(AgentDraftMutationResult::success(updated)),
        Some(revision) => refused(AgentDraftMutationProblem::conflict(
            agent,
            expected_revision,
            revision,
        )),
    }
}

#[cfg(test)]
mod draft_rule_tests {
    use super::valid_name;

    /// The name must start with a letter and be at least two characters, which is what stops a
    /// single character or a leading digit from becoming an agent's display name.
    #[test]
    fn an_agent_name_starts_with_a_letter_and_is_two_to_eighty_one_characters() {
        assert!(!valid_name(""));
        assert!(!valid_name("A"));
        assert!(valid_name("Ab"));
        assert!(valid_name("Feedback Triage_agent-2"));
        assert!(!valid_name("1st Agent"));
        assert!(!valid_name(" Leading space"));
        assert!(!valid_name("Trailing newline\n"));
        assert!(!valid_name("Punctuation!"));
        assert!(valid_name(&format!("A{}", "b".repeat(80))));
        assert!(!valid_name(&format!("A{}", "b".repeat(81))));
    }

    /// The name is matched as bytes, so an accented character costs more than one of the
    /// eighty-one the pattern allows — and is not a permitted character at all.
    #[test]
    fn an_agent_name_admits_only_ascii() {
        assert!(!valid_name("Café"));
        assert!(!valid_name("Ünicode"));
    }
}
