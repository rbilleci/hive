//! The five definition-authoring commands: creating a definition with its first draft, updating,
//! validating and duplicating a draft, and publishing one as an immutable version.
//!
//! Every draft write goes through `replace_draft`, which carries the expected revision in its
//! `WHERE` clause, so a lost race writes nothing and reports the stored revision.

use super::{
    audit_definition, document_json, idempotent_mutation, receipt, valid_key, valid_slug,
    validation_status, MutationResult, ReceiptSubject,
};
use crate::capability::tx;
use crate::entity::enums::EvaluationCommandAction;
use crate::entity::{
    evaluation_definition_drafts, evaluation_definition_versions, evaluation_definitions,
};
use crate::evaluation::queries::{
    can, definition, draft, next_version_number, project_active, version, version_for_digest,
};
use crate::evaluation::rows::{
    definition_project, diagnostics_json, document_text, version_project,
};
use crate::{guard, retry};
use hive_application::configuration::canonical::digest as sha256;
use hive_application::evaluation::document;
use hive_application::evaluation::{EvaluationMutationResult, EvaluationProblem};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, NotSet,
    QueryFilter, Set, TransactionTrait,
};
use uuid::Uuid;

async fn insert_draft(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
    document_source: &str,
    diagnostics: &[document::EvaluationDiagnostic],
    based_on: Option<Uuid>,
) -> Result<(), DbErr> {
    evaluation_definition_drafts::Entity::insert(evaluation_definition_drafts::ActiveModel {
        definition_id: Set(definition_id),
        canonical_document: Set(document_json(document_source)),
        revision: NotSet,
        validation_status: Set(validation_status(diagnostics)),
        diagnostics: Set(diagnostics_json(diagnostics)),
        based_on_version_id: Set(based_on),
        updated_at: NotSet,
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

/// The guarded draft write every draft command ends in: the revision is in the `WHERE` clause and
/// is incremented in place, so a lost race writes nothing and reports the stored revision.
async fn replace_draft(
    db: &impl ConnectionTrait,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    document_text_value: &str,
    based_on: Option<Uuid>,
    action: &str,
) -> Result<MutationResult, DbErr> {
    use evaluation_definition_drafts::Column;
    let diagnostics = document::validate(document_text_value);
    let updated = guard::bump(
        db,
        evaluation_definition_drafts::Entity::update_many()
            .col_expr(
                Column::CanonicalDocument,
                Expr::value(document_json(document_text_value)),
            )
            .col_expr(
                Column::ValidationStatus,
                Expr::value(validation_status(&diagnostics).to_value()),
            )
            .col_expr(
                Column::Diagnostics,
                Expr::value(diagnostics_json(&diagnostics)),
            )
            .col_expr(Column::BasedOnVersionId, Expr::value(based_on))
            .col_expr(Column::UpdatedAt, Expr::current_timestamp())
            .filter(Column::DefinitionId.eq(definition_id)),
        Column::Revision,
        expected_revision,
    )
    .await?;
    if !updated {
        let current = draft(db, definition_id, true).await?;
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::conflict(definition_id, expected_revision, current.revision),
        ));
    }
    audit_definition(db, definition_id, principal, action).await?;
    let value = definition(db, principal, definition_id, true)
        .await?
        .ok_or_else(|| {
            DbErr::RecordNotFound(format!("no evaluation definition with id {definition_id}"))
        })?;
    Ok(EvaluationMutationResult::definition(value))
}

pub async fn create_definition(
    db: &DatabaseConnection,
    principal: Uuid,
    project: Uuid,
    slug: &str,
    document_source: Option<&str>,
    key: &str,
) -> Result<MutationResult, DbErr> {
    let source = document_source
        .map(str::to_string)
        .unwrap_or_else(document::default_document);
    let Some(canonical) = document::canonicalize(&source) else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    };
    if !valid_key(key) || !valid_slug(slug) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!("{slug}|{canonical}"));
    let txn = db.begin().await?;
    let result = create_definition_tx(
        &txn,
        principal,
        project,
        slug,
        &canonical,
        key,
        &fingerprint,
    )
    .await;
    match result {
        Ok(value) => {
            txn.commit().await?;
            Ok(value)
        }
        Err(error) if retry::is_unique_violation_db(&error) => {
            // A 23505 here is either the receipt's own unique constraint (an idempotency replay)
            // or the definitions table's `(project_id, slug)` constraint (a genuinely new slug
            // collision, which is a validation refusal). Try the replay path first; finding no
            // receipt means it was the slug collision.
            let retry = db.begin().await?;
            let replay = idempotent_mutation(
                &retry,
                project,
                principal,
                EvaluationCommandAction::Create,
                key,
                &fingerprint,
            )
            .await?;
            retry.commit().await?;
            match replay {
                Some(value) => Ok(value),
                None => Ok(EvaluationMutationResult::refused(
                    EvaluationProblem::validation(),
                )),
            }
        }
        Err(error) => Err(error),
    }
}

async fn create_definition_tx(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
    slug: &str,
    canonical: &str,
    key: &str,
    fingerprint: &str,
) -> Result<MutationResult, DbErr> {
    if !can(
        txn,
        principal,
        tx::EVALUATION_DEFINITION_VIEW,
        project,
        true,
    )
    .await?
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    if !can(
        txn,
        principal,
        tx::EVALUATION_DEFINITION_AUTHOR,
        project,
        true,
    )
    .await?
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::forbidden(),
        ));
    }
    if !project_active(txn, project).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    if let Some(replay) = idempotent_mutation(
        txn,
        project,
        principal,
        EvaluationCommandAction::Create,
        key,
        fingerprint,
    )
    .await?
    {
        return Ok(replay);
    }
    let id = Uuid::new_v4();
    evaluation_definitions::Entity::insert(evaluation_definitions::ActiveModel {
        id: Set(id),
        project_id: Set(project),
        slug: Set(slug.to_string()),
        created_by: Set(principal),
        lifecycle_status: NotSet,
        created_at: NotSet,
    })
    .exec_without_returning(txn)
    .await?;
    let diagnostics = document::validate(canonical);
    insert_draft(txn, id, canonical, &diagnostics, None).await?;
    audit_definition(txn, id, principal, "CREATED").await?;
    receipt(
        txn,
        project,
        principal,
        EvaluationCommandAction::Create,
        key,
        fingerprint,
        ReceiptSubject {
            definition_id: Some(id),
            ..Default::default()
        },
    )
    .await?;
    let value = definition(txn, principal, id, true)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no evaluation definition with id {id}")))?;
    Ok(EvaluationMutationResult::definition(value))
}

#[allow(clippy::too_many_arguments)]
async fn draft_command(
    db: &DatabaseConnection,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    capability: &str,
    replacement: Option<&str>,
    action: &str,
    receipt_action: EvaluationCommandAction,
    key: &str,
    fingerprint: &str,
) -> Result<MutationResult, DbErr> {
    let txn = db.begin().await?;
    let result = draft_command_tx(
        &txn,
        principal,
        definition_id,
        expected_revision,
        capability,
        replacement,
        action,
        receipt_action,
        key,
        fingerprint,
    )
    .await;
    match result {
        Ok(value) => {
            txn.commit().await?;
            Ok(value)
        }
        Err(error) if retry::is_serialization_failure_db(&error) => {
            let current =
                retry::reread(db, async |retry| draft(retry, definition_id, false).await).await?;
            Ok(EvaluationMutationResult::refused(
                EvaluationProblem::conflict(definition_id, expected_revision, current.revision),
            ))
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn draft_command_tx(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    capability: &str,
    replacement: Option<&str>,
    action: &str,
    receipt_action: EvaluationCommandAction,
    key: &str,
    fingerprint: &str,
) -> Result<MutationResult, DbErr> {
    let Some(project) = definition_project(txn, definition_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if !can(
        txn,
        principal,
        tx::EVALUATION_DEFINITION_VIEW,
        project,
        true,
    )
    .await?
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    if !can(txn, principal, capability, project, true).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::forbidden(),
        ));
    }
    if !project_active(txn, project).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    if let Some(replay) =
        idempotent_mutation(txn, project, principal, receipt_action, key, fingerprint).await?
    {
        return Ok(replay);
    }
    if definition(txn, principal, definition_id, true)
        .await?
        .is_none()
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    let current_draft = draft(txn, definition_id, true).await?;
    if current_draft.revision != expected_revision {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::conflict(definition_id, expected_revision, current_draft.revision),
        ));
    }
    // A validation keeps the stored document and its `based_on_version_id`; a save replaces both.
    let (replacement_document, based_on) = match replacement {
        Some(document_source) => (document_source.to_string(), None),
        None => (
            document_text(&current_draft.canonical_document),
            current_draft.based_on_version_id,
        ),
    };
    let result = replace_draft(
        txn,
        principal,
        definition_id,
        expected_revision,
        &replacement_document,
        based_on,
        action,
    )
    .await?;
    if result.problem.is_none() {
        receipt(
            txn,
            project,
            principal,
            receipt_action,
            key,
            fingerprint,
            ReceiptSubject {
                definition_id: Some(definition_id),
                ..Default::default()
            },
        )
        .await?;
    }
    Ok(result)
}

pub async fn update_draft(
    db: &DatabaseConnection,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    document_source: &str,
    key: &str,
) -> Result<MutationResult, DbErr> {
    let Some(canonical) = document::canonicalize(document_source) else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    };
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!("{definition_id}|{expected_revision}|{canonical}"));
    draft_command(
        db,
        principal,
        definition_id,
        expected_revision,
        tx::EVALUATION_DEFINITION_AUTHOR,
        Some(&canonical),
        "SAVED",
        EvaluationCommandAction::Update,
        key,
        &fingerprint,
    )
    .await
}

pub async fn validate_draft(
    db: &DatabaseConnection,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    key: &str,
) -> Result<MutationResult, DbErr> {
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!("{definition_id}|{expected_revision}"));
    draft_command(
        db,
        principal,
        definition_id,
        expected_revision,
        tx::EVALUATION_DEFINITION_AUTHOR,
        None,
        "VALIDATED",
        EvaluationCommandAction::Validate,
        key,
        &fingerprint,
    )
    .await
}

pub async fn duplicate_version(
    db: &DatabaseConnection,
    principal: Uuid,
    version_id: Uuid,
    expected_revision: i64,
    key: &str,
) -> Result<MutationResult, DbErr> {
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!("{version_id}|{expected_revision}"));
    let txn = db.begin().await?;
    let result = duplicate_version_tx(
        &txn,
        principal,
        version_id,
        expected_revision,
        key,
        &fingerprint,
    )
    .await;
    match result {
        Ok(value) => {
            txn.commit().await?;
            Ok(value)
        }
        Err(error) if retry::is_serialization_failure_db(&error) => {
            // The source version names the definition whose draft the conflict reports, so a
            // version that is itself gone leaves nothing to report a revision against.
            let raced = retry::reread(db, async |retry| {
                let Some(source) = version(retry, version_id).await? else {
                    return Ok(None);
                };
                let current = draft(retry, source.definition_id, false).await?;
                Ok(Some((source.definition_id, current.revision)))
            })
            .await?;
            let Some((definition_id, revision)) = raced else {
                return Ok(EvaluationMutationResult::refused(
                    EvaluationProblem::not_found(),
                ));
            };
            Ok(EvaluationMutationResult::refused(
                EvaluationProblem::conflict(definition_id, expected_revision, revision),
            ))
        }
        Err(error) => Err(error),
    }
}

async fn duplicate_version_tx(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    version_id: Uuid,
    expected_revision: i64,
    key: &str,
    fingerprint: &str,
) -> Result<MutationResult, DbErr> {
    let Some(source) = version(txn, version_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    let Some(project) = version_project(txn, version_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if !can(
        txn,
        principal,
        tx::EVALUATION_DEFINITION_VIEW,
        project,
        true,
    )
    .await?
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    if !can(
        txn,
        principal,
        tx::EVALUATION_DEFINITION_AUTHOR,
        project,
        true,
    )
    .await?
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::forbidden(),
        ));
    }
    if !project_active(txn, project).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    if let Some(replay) = idempotent_mutation(
        txn,
        project,
        principal,
        EvaluationCommandAction::Duplicate,
        key,
        fingerprint,
    )
    .await?
    {
        return Ok(replay);
    }
    let result = replace_draft(
        txn,
        principal,
        source.definition_id,
        expected_revision,
        &document_text(&source.canonical_document),
        Some(source.id),
        "DUPLICATED",
    )
    .await?;
    if result.problem.is_none() {
        receipt(
            txn,
            project,
            principal,
            EvaluationCommandAction::Duplicate,
            key,
            fingerprint,
            ReceiptSubject {
                definition_id: Some(source.definition_id),
                ..Default::default()
            },
        )
        .await?;
    }
    Ok(result)
}

pub async fn publish_draft(
    db: &DatabaseConnection,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    key: &str,
) -> Result<MutationResult, DbErr> {
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!("{definition_id}|{expected_revision}"));
    let txn = db.begin().await?;
    let result = publish_draft_tx(
        &txn,
        principal,
        definition_id,
        expected_revision,
        key,
        &fingerprint,
    )
    .await;
    match result {
        Ok(value) => {
            txn.commit().await?;
            Ok(value)
        }
        Err(error) if retry::is_serialization_failure_db(&error) => {
            let current =
                retry::reread(db, async |retry| draft(retry, definition_id, false).await).await?;
            Ok(EvaluationMutationResult::refused(
                EvaluationProblem::conflict(definition_id, expected_revision, current.revision),
            ))
        }
        Err(error) => Err(error),
    }
}

async fn publish_draft_tx(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    key: &str,
    fingerprint: &str,
) -> Result<MutationResult, DbErr> {
    let Some(project) = definition_project(txn, definition_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if !can(
        txn,
        principal,
        tx::EVALUATION_DEFINITION_VIEW,
        project,
        true,
    )
    .await?
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    if !can(
        txn,
        principal,
        tx::EVALUATION_DEFINITION_PUBLISH,
        project,
        true,
    )
    .await?
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::forbidden(),
        ));
    }
    if !project_active(txn, project).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    if let Some(replay) = idempotent_mutation(
        txn,
        project,
        principal,
        EvaluationCommandAction::Publish,
        key,
        fingerprint,
    )
    .await?
    {
        return Ok(replay);
    }
    let Some(current_definition) = definition(txn, principal, definition_id, true).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    let current_draft = draft(txn, definition_id, true).await?;
    if current_draft.revision != expected_revision {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::conflict(definition_id, expected_revision, current_draft.revision),
        ));
    }
    let canonical = document_text(&current_draft.canonical_document);
    let diagnostics = document::validate(&canonical);
    if diagnostics.iter().any(|value| value.severity == "ERROR") {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let digest = document::digest(&canonical);
    if let Some(existing) = version_for_digest(txn, definition_id, &digest).await? {
        return Ok(EvaluationMutationResult::version(
            current_definition,
            existing,
        ));
    }
    let number = next_version_number(txn, definition_id).await?;
    let id = Uuid::new_v4();
    evaluation_definition_versions::Entity::insert(evaluation_definition_versions::ActiveModel {
        id: Set(id),
        definition_id: Set(definition_id),
        version_number: Set(number),
        canonical_document: Set(current_draft.canonical_document.clone()),
        content_digest: Set(digest),
        based_on_version_id: Set(current_draft.based_on_version_id),
        published_by: Set(principal),
        published_at: NotSet,
    })
    .exec_without_returning(txn)
    .await?;
    audit_definition(txn, definition_id, principal, "PUBLISHED").await?;
    receipt(
        txn,
        project,
        principal,
        EvaluationCommandAction::Publish,
        key,
        fingerprint,
        ReceiptSubject {
            version_id: Some(id),
            ..Default::default()
        },
    )
    .await?;
    let owner = definition(txn, principal, definition_id, true)
        .await?
        .ok_or_else(|| {
            DbErr::RecordNotFound(format!("no evaluation definition with id {definition_id}"))
        })?;
    let value = version(txn, id).await?.ok_or_else(|| {
        DbErr::RecordNotFound(format!("no evaluation definition version with id {id}"))
    })?;
    Ok(EvaluationMutationResult::version(owner, value))
}
