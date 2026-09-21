//! The eight evaluation commands (`createEvaluationDefinition`,
//! `updateEvaluationDefinitionDraft`, `validateEvaluationDefinitionDraft`,
//! `duplicateEvaluationDefinitionVersionToDraft`, `publishEvaluationDefinitionDraft`,
//! `runEvaluation`, `cancelEvaluation`, `rerunEvaluation`) on SeaORM entities, with their shared
//! idempotency-receipt machinery.
//!
//! Each runs in one transaction: it re-checks the principal's authority with the evaluator's
//! locking checks, takes its row locks, compares the expected revision or generation, writes with
//! that revision or generation in the `WHERE` clause, records its audit row, and stores a command
//! receipt before it commits. A command answers with the stored `evaluation_definitions`,
//! `evaluation_definition_versions` or `evaluation_runs` row itself; the GraphQL payload exposes
//! it as the same generated type the reads use.
//!
//! A unique violation on the receipt insert is the idempotency replay (the caller retries the
//! lookup); a fingerprint mismatch on an existing receipt is returned as a refusal.

use hive_application::configuration::canonical::digest as sha256;
use hive_application::evaluation::document;
use hive_application::evaluation::{
    EvaluationMutationResult, EvaluationProblem, EvaluationRunStatus,
};
use sea_orm::sea_query::{Expr, ExprTrait, OnConflict};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, NotSet,
    QueryFilter, Set, TransactionTrait,
};
use uuid::Uuid;

use super::queries::{
    self, can, definition, draft, next_version_number, project_active, run, version,
    version_for_digest,
};
use super::rows::{definition_project, diagnostics_json, document_text, version_project, Target};
use crate::capability::tx;
use crate::entity::enums::{
    DraftValidationStatus, EvaluationCommandAction, EvaluationLifecycleStatus,
    EvaluationOutboxEventType, EvaluationOutboxStatus, EvaluationOutcomeCategory,
    EvaluationTargetKind,
};
use crate::entity::{
    evaluation_audit_events, evaluation_case_runs, evaluation_command_receipts,
    evaluation_definition_drafts, evaluation_definition_versions, evaluation_definitions,
    evaluation_outbox_events, evaluation_runs, evaluation_target_snapshots,
};
use crate::retry::{is_serialization_failure_db, is_unique_violation_db};

/// What an evaluation command answers with: the stored rows themselves.
pub type MutationResult = EvaluationMutationResult<
    evaluation_definitions::Model,
    evaluation_definition_versions::Model,
    evaluation_runs::Model,
>;

fn valid_key(value: &str) -> bool {
    let trimmed = value.trim();
    (8..=160).contains(&trimmed.len())
}

fn valid_slug(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && value.len() <= 120
        && chars.all(|value| value.is_ascii_lowercase() || value.is_ascii_digit() || value == '-')
}

/// The draft's validation status for a set of diagnostics.
fn validation_status(diagnostics: &[document::EvaluationDiagnostic]) -> DraftValidationStatus {
    if diagnostics.iter().any(|value| value.severity == "ERROR") {
        DraftValidationStatus::Invalid
    } else {
        DraftValidationStatus::Valid
    }
}

/// A canonical document as the `jsonb` column holds it.
fn document_json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("a canonical evaluation document is always valid JSON")
}

/// `Ok(None)`: no receipt yet, proceed as a new command. `Ok(Some(_))`: either the matched replay
/// result (fingerprint equal) or an idempotency-conflict refusal (fingerprint mismatch).
async fn idempotent_mutation(
    db: &impl ConnectionTrait,
    project: Uuid,
    principal: Uuid,
    action: EvaluationCommandAction,
    key: &str,
    fingerprint: &str,
) -> Result<Option<MutationResult>, DbErr> {
    let Some(row) = evaluation_command_receipts::Entity::find()
        .filter(evaluation_command_receipts::Column::ProjectId.eq(project))
        .filter(evaluation_command_receipts::Column::PrincipalId.eq(principal))
        .filter(evaluation_command_receipts::Column::Action.eq(action))
        .filter(evaluation_command_receipts::Column::IdempotencyKey.eq(key))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    if row.request_fingerprint != fingerprint {
        return Ok(Some(EvaluationMutationResult::refused(
            EvaluationProblem::idempotency(),
        )));
    }
    if let Some(definition_id) = row.definition_id {
        let value = definition(db, principal, definition_id, true)
            .await?
            .ok_or_else(|| {
                DbErr::RecordNotFound(format!("no evaluation definition with id {definition_id}"))
            })?;
        return Ok(Some(EvaluationMutationResult::definition(value)));
    }
    if let Some(version_id) = row.definition_version_id {
        let value = version(db, version_id).await?.ok_or_else(|| {
            DbErr::RecordNotFound(format!(
                "no evaluation definition version with id {version_id}"
            ))
        })?;
        let owner = definition(db, principal, value.definition_id, true)
            .await?
            .ok_or_else(|| {
                DbErr::RecordNotFound(format!(
                    "no evaluation definition with id {}",
                    value.definition_id
                ))
            })?;
        return Ok(Some(EvaluationMutationResult::version(owner, value)));
    }
    let run_id = row.run_id.ok_or_else(|| {
        DbErr::RecordNotFound(
            "evaluation command receipt has no definition/version/run".to_string(),
        )
    })?;
    let value = run(db, principal, run_id, true)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no evaluation run with id {run_id}")))?;
    Ok(Some(EvaluationMutationResult::run(value)))
}

#[allow(clippy::too_many_arguments)]
async fn receipt(
    db: &impl ConnectionTrait,
    project: Uuid,
    principal: Uuid,
    action: EvaluationCommandAction,
    key: &str,
    fingerprint: &str,
    definition_id: Option<Uuid>,
    version_id: Option<Uuid>,
    run_id: Option<Uuid>,
) -> Result<(), DbErr> {
    evaluation_command_receipts::Entity::insert(evaluation_command_receipts::ActiveModel {
        id: Set(Uuid::new_v4()),
        project_id: Set(project),
        principal_id: Set(principal),
        action: Set(action),
        idempotency_key: Set(key.to_string()),
        request_fingerprint: Set(fingerprint.to_string()),
        definition_id: Set(definition_id),
        definition_version_id: Set(version_id),
        run_id: Set(run_id),
        created_at: NotSet,
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

/// One evaluation audit row, carrying the request metadata of the command that wrote it.
async fn audit(
    db: &impl ConnectionTrait,
    definition_id: Option<Uuid>,
    run_id: Option<Uuid>,
    actor: Option<Uuid>,
    action: &str,
    summary: &str,
) -> Result<(), DbErr> {
    let metadata = crate::audit::context::request_metadata();
    evaluation_audit_events::Entity::insert(evaluation_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        run_id: Set(run_id),
        definition_id: Set(definition_id),
        actor_principal_id: Set(actor),
        action: Set(action.to_string()),
        facts: Set(serde_json::json!({ "summary": summary })),
        occurred_at: NotSet,
        request_id: Set(metadata.request_id),
        correlation_id: Set(metadata.correlation_id),
        graphql_operation: Set(metadata.graphql_operation),
        source_ip: Set(metadata.source_ip),
        user_agent: Set(metadata.user_agent),
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

async fn audit_definition(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
    actor: Uuid,
    action: &str,
) -> Result<(), DbErr> {
    let summary = format!(
        "Evaluation definition {}.",
        action.to_lowercase().replace('_', " ")
    );
    audit(db, Some(definition_id), None, Some(actor), action, &summary).await
}

pub(super) async fn audit_run(
    db: &impl ConnectionTrait,
    run_id: Uuid,
    actor: Option<Uuid>,
    action: &str,
    summary: &str,
) -> Result<(), DbErr> {
    audit(db, None, Some(run_id), actor, action, summary).await
}

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
    let updated = evaluation_definition_drafts::Entity::update_many()
        .col_expr(
            Column::CanonicalDocument,
            Expr::value(document_json(document_text_value)),
        )
        .col_expr(Column::Revision, Expr::col(Column::Revision).add(1))
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
        .filter(Column::DefinitionId.eq(definition_id))
        .filter(Column::Revision.eq(expected_revision))
        .exec(db)
        .await?;
    if updated.rows_affected != 1 {
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

pub(super) async fn insert_target(
    db: &impl ConnectionTrait,
    run: Uuid,
    target: &Target,
) -> Result<(), DbErr> {
    evaluation_target_snapshots::Entity::insert(evaluation_target_snapshots::ActiveModel {
        run_id: Set(run),
        agent_version_id: Set(target.agent_version_id),
        deployment_id: Set(target.deployment_id),
        environment_definition_version_id: Set(target.environment_definition_version_id),
        logical_environment_class: Set(target.environment_class),
        agent_content_digest: Set(target.agent_digest.clone()),
        target_digest: Set(target.target_digest.clone()),
        plan_digest: Set(target.plan_digest.clone()),
        package_digest: Set(target.package_digest.clone()),
        binding_digest: Set(target.binding_digest.clone()),
        catalog_release_id: Set(target.catalog_release_id.clone()),
        catalog_release_digest: Set(target.catalog_release_digest.clone()),
        environment_content_digest: Set(target.environment_digest.clone()),
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

pub(super) async fn insert_cases(
    db: &impl ConnectionTrait,
    run: Uuid,
    document_source: &str,
) -> Result<(), DbErr> {
    for case in document::cases(document_source) {
        evaluation_case_runs::Entity::insert(evaluation_case_runs::ActiveModel {
            id: Set(Uuid::new_v4()),
            run_id: Set(run),
            case_key: Set(case.key.clone()),
            ordinal: Set(case.ordinal),
            lifecycle_status: NotSet,
            passed: NotSet,
            failure_code: NotSet,
            completed_at: NotSet,
        })
        .exec_without_returning(db)
        .await?;
    }
    Ok(())
}

/// The `case_slot` of a run-wide event: the outbox's uniqueness key has no nullable column.
pub(super) const NO_CASE_SLOT: Uuid = Uuid::nil();

pub(super) async fn enqueue(
    db: &impl ConnectionTrait,
    run: Uuid,
    event: EvaluationOutboxEventType,
    case_run: Option<Uuid>,
) -> Result<(), DbErr> {
    evaluation_outbox_events::Entity::insert(evaluation_outbox_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        run_id: Set(run),
        event_type: Set(event),
        case_run_id: Set(case_run),
        case_slot: Set(case_run.unwrap_or(NO_CASE_SLOT)),
        status: NotSet,
        available_at: NotSet,
        claimed_at: NotSet,
        claimed_by: NotSet,
        attempt_count: NotSet,
        delivered_at: NotSet,
        last_error: NotSet,
        created_at: NotSet,
    })
    .on_conflict(
        OnConflict::columns([
            evaluation_outbox_events::Column::RunId,
            evaluation_outbox_events::Column::EventType,
            evaluation_outbox_events::Column::CaseSlot,
        ])
        .do_nothing()
        .to_owned(),
    )
    .try_insert()
    .exec_without_returning(db)
    .await?;
    Ok(())
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
        Err(error) if is_unique_violation_db(&error) => {
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
        Some(id),
        None,
        None,
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
        Err(error) if is_serialization_failure_db(&error) => {
            let retry = db.begin().await?;
            let current = draft(&retry, definition_id, false).await?;
            retry.commit().await?;
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
            Some(definition_id),
            None,
            None,
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
        Err(error) if is_serialization_failure_db(&error) => {
            let retry = db.begin().await?;
            let Some(source) = version(&retry, version_id).await? else {
                retry.commit().await?;
                return Ok(EvaluationMutationResult::refused(
                    EvaluationProblem::not_found(),
                ));
            };
            let current = draft(&retry, source.definition_id, false).await?;
            retry.commit().await?;
            Ok(EvaluationMutationResult::refused(
                EvaluationProblem::conflict(
                    source.definition_id,
                    expected_revision,
                    current.revision,
                ),
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
            Some(source.definition_id),
            None,
            None,
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
        Err(error) if is_serialization_failure_db(&error) => {
            let retry = db.begin().await?;
            let current = draft(&retry, definition_id, false).await?;
            retry.commit().await?;
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
        None,
        Some(id),
        None,
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

#[allow(clippy::too_many_arguments)]
pub async fn run_evaluation(
    db: &DatabaseConnection,
    principal: Uuid,
    project: Uuid,
    definition_version_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    environment_id: Uuid,
    key: &str,
) -> Result<MutationResult, DbErr> {
    let Ok(kind) = EvaluationTargetKind::try_from_value(&target_kind.to_string()) else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    };
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!(
        "{definition_version_id}|{target_kind}|{target_id}|{environment_id}"
    ));
    let txn = db.begin().await?;
    let result = run_evaluation_tx(
        &txn,
        principal,
        project,
        definition_version_id,
        kind,
        target_id,
        environment_id,
        key,
        &fingerprint,
    )
    .await;
    match result {
        Ok(value) => {
            txn.commit().await?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_evaluation_tx(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
    definition_version_id: Uuid,
    target_kind: EvaluationTargetKind,
    target_id: Uuid,
    environment_id: Uuid,
    key: &str,
    fingerprint: &str,
) -> Result<MutationResult, DbErr> {
    if !can(txn, principal, tx::EVALUATION_RUN_VIEW, project, true).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    if !can(txn, principal, tx::EVALUATION_RUN_RUN, project, true).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::forbidden(),
        ));
    }
    if !project_active(txn, project).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    let Some(definition_version) = version(txn, definition_version_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if version_project(txn, definition_version_id).await? != Some(project) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    let canonical = document_text(&definition_version.canonical_document);
    if document::validate(&canonical)
        .iter()
        .any(|value| value.severity == "ERROR")
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let kind = target_kind.to_value();
    let Some(target) =
        queries::resolve_target(txn, project, &kind, target_id, environment_id).await?
    else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::target(),
        ));
    };
    let kinds = document::target_kinds(&canonical);
    let environments = document::environment_classes(&canonical);
    if !kinds.contains(&kind) || !environments.contains(&target.environment_class.to_value()) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::target(),
        ));
    }
    if let Some(replay) = idempotent_mutation(
        txn,
        project,
        principal,
        EvaluationCommandAction::Run,
        key,
        fingerprint,
    )
    .await?
    {
        return Ok(replay);
    }
    let run_id = Uuid::new_v4();
    evaluation_runs::Entity::insert(evaluation_runs::ActiveModel {
        id: Set(run_id),
        project_id: Set(project),
        definition_version_id: Set(definition_version_id),
        target_kind: Set(target_kind),
        target_id: Set(target_id),
        environment_definition_version_id: Set(environment_id),
        requester_id: Set(principal),
        source_run_id: Set(None),
        creation_fingerprint: Set(fingerprint.to_string()),
        lifecycle_status: NotSet,
        generation: NotSet,
        outcome_category: NotSet,
        outcome_code: NotSet,
        created_at: NotSet,
        started_at: NotSet,
        completed_at: NotSet,
    })
    .exec_without_returning(txn)
    .await?;
    insert_target(txn, run_id, &target).await?;
    insert_cases(txn, run_id, &canonical).await?;
    receipt(
        txn,
        project,
        principal,
        EvaluationCommandAction::Run,
        key,
        fingerprint,
        None,
        None,
        Some(run_id),
    )
    .await?;
    audit_run(
        txn,
        run_id,
        Some(principal),
        "QUEUED",
        "Evaluation run queued.",
    )
    .await?;
    enqueue(txn, run_id, EvaluationOutboxEventType::Start, None).await?;
    let value = run(txn, principal, run_id, true)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no evaluation run with id {run_id}")))?;
    Ok(EvaluationMutationResult::run(value))
}

pub async fn cancel(
    db: &DatabaseConnection,
    principal: Uuid,
    run_id: Uuid,
    expected_generation: i64,
    key: &str,
) -> Result<MutationResult, DbErr> {
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    // `pg_advisory_xact_lock('m16-evaluation-<run>')` (removed): every call site (here, `commit`,
    // `retry_or_dead_letter`) already takes a real `FOR UPDATE` on this same `evaluation_runs` row
    // moments later, which DSQL's own commit-time OCC already serializes concurrent transactions
    // through — the same reasoning already confirmed for `m14-approval-transition`.
    let fingerprint = sha256(&format!("{run_id}|{expected_generation}"));
    let txn = db.begin().await?;
    let result = cancel_tx(
        &txn,
        principal,
        run_id,
        expected_generation,
        key,
        &fingerprint,
    )
    .await;
    match result {
        Ok(value) => {
            txn.commit().await?;
            Ok(value)
        }
        Err(error) if is_serialization_failure_db(&error) => {
            let retry = db.begin().await?;
            let current = queries::raw_run(&retry, run_id, false).await?;
            retry.commit().await?;
            let generation = current
                .map(|value| value.generation)
                .unwrap_or(expected_generation);
            Ok(EvaluationMutationResult::refused(
                EvaluationProblem::conflict(run_id, expected_generation, generation),
            ))
        }
        Err(error) => Err(error),
    }
}

async fn cancel_tx(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    run_id: Uuid,
    expected_generation: i64,
    key: &str,
    fingerprint: &str,
) -> Result<MutationResult, DbErr> {
    let Some(raw) = queries::raw_run(txn, run_id, true).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if !can(
        txn,
        principal,
        tx::EVALUATION_RUN_VIEW,
        raw.project_id,
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
        tx::EVALUATION_RUN_CANCEL,
        raw.project_id,
        true,
    )
    .await?
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::forbidden(),
        ));
    }
    if let Some(replay) = idempotent_mutation(
        txn,
        raw.project_id,
        principal,
        EvaluationCommandAction::Cancel,
        key,
        fingerprint,
    )
    .await?
    {
        return Ok(replay);
    }
    if raw.generation != expected_generation || !super::rows::run_status(&raw).may_cancel() {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    super::worker::update_run(
        txn,
        &raw,
        EvaluationRunStatus::Canceled,
        raw.generation + 1,
        Some(EvaluationOutcomeCategory::Canceled),
        Some("CANCELED"),
        true,
    )
    .await?;
    cancel_open_cases(txn, run_id).await?;
    cancel_open_events(txn, run_id).await?;
    super::worker::insert_result(
        txn,
        run_id,
        false,
        EvaluationOutcomeCategory::Canceled,
        Some("CANCELED"),
    )
    .await?;
    receipt(
        txn,
        raw.project_id,
        principal,
        EvaluationCommandAction::Cancel,
        key,
        fingerprint,
        None,
        None,
        Some(run_id),
    )
    .await?;
    audit_run(
        txn,
        run_id,
        Some(principal),
        "CANCELED",
        "Evaluation run canceled.",
    )
    .await?;
    let value = run(txn, principal, run_id, true)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no evaluation run with id {run_id}")))?;
    Ok(EvaluationMutationResult::run(value))
}

/// Cancels every case of a run that has not finished yet.
pub(super) async fn cancel_open_cases(
    db: &impl ConnectionTrait,
    run_id: Uuid,
) -> Result<(), DbErr> {
    use evaluation_case_runs::Column;
    evaluation_case_runs::Entity::update_many()
        .col_expr(
            Column::LifecycleStatus,
            Expr::value(EvaluationLifecycleStatus::Canceled.to_value()),
        )
        .col_expr(Column::CompletedAt, Expr::current_timestamp())
        .filter(Column::RunId.eq(run_id))
        .filter(Column::LifecycleStatus.is_in([
            EvaluationLifecycleStatus::Queued,
            EvaluationLifecycleStatus::Running,
        ]))
        .exec(db)
        .await?;
    Ok(())
}

/// Cancels every outbox event of a run that has not been delivered yet.
pub(super) async fn cancel_open_events(
    db: &impl ConnectionTrait,
    run_id: Uuid,
) -> Result<(), DbErr> {
    use evaluation_outbox_events::Column;
    evaluation_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(EvaluationOutboxStatus::Canceled.to_value()),
        )
        .filter(Column::RunId.eq(run_id))
        .filter(Column::Status.is_in([
            EvaluationOutboxStatus::Pending,
            EvaluationOutboxStatus::Processing,
        ]))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn rerun(
    db: &DatabaseConnection,
    principal: Uuid,
    source_run_id: Uuid,
    key: &str,
) -> Result<MutationResult, DbErr> {
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!("{source_run_id}|rerun"));
    let txn = db.begin().await?;
    let result = rerun_tx(&txn, principal, source_run_id, key, &fingerprint).await;
    match result {
        Ok(value) => {
            txn.commit().await?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

async fn rerun_tx(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    source_run_id: Uuid,
    key: &str,
    fingerprint: &str,
) -> Result<MutationResult, DbErr> {
    let Some(source) = queries::raw_run(txn, source_run_id, true).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if !can(
        txn,
        principal,
        tx::EVALUATION_RUN_VIEW,
        source.project_id,
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
        tx::EVALUATION_RUN_RERUN,
        source.project_id,
        true,
    )
    .await?
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::forbidden(),
        ));
    }
    if !project_active(txn, source.project_id).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    if !super::rows::run_status(&source).is_terminal() {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    if let Some(replay) = idempotent_mutation(
        txn,
        source.project_id,
        principal,
        EvaluationCommandAction::Rerun,
        key,
        fingerprint,
    )
    .await?
    {
        return Ok(replay);
    }
    let Some(definition_version) = version(txn, source.definition_version_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::target(),
        ));
    };
    let Some(target) = queries::target_from_snapshot(txn, source_run_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::target(),
        ));
    };
    let canonical = document_text(&definition_version.canonical_document);
    let kinds = document::target_kinds(&canonical);
    let environments = document::environment_classes(&canonical);
    if !kinds.contains(&source.target_kind.to_value())
        || !environments.contains(&target.environment_class.to_value())
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::target(),
        ));
    }
    let run_id = Uuid::new_v4();
    evaluation_runs::Entity::insert(evaluation_runs::ActiveModel {
        id: Set(run_id),
        project_id: Set(source.project_id),
        definition_version_id: Set(source.definition_version_id),
        target_kind: Set(source.target_kind),
        target_id: Set(source.target_id),
        environment_definition_version_id: Set(source.environment_definition_version_id),
        requester_id: Set(principal),
        source_run_id: Set(Some(source_run_id)),
        creation_fingerprint: Set(fingerprint.to_string()),
        lifecycle_status: NotSet,
        generation: NotSet,
        outcome_category: NotSet,
        outcome_code: NotSet,
        created_at: NotSet,
        started_at: NotSet,
        completed_at: NotSet,
    })
    .exec_without_returning(txn)
    .await?;
    insert_target(txn, run_id, &target).await?;
    insert_cases(txn, run_id, &canonical).await?;
    receipt(
        txn,
        source.project_id,
        principal,
        EvaluationCommandAction::Rerun,
        key,
        fingerprint,
        None,
        None,
        Some(run_id),
    )
    .await?;
    audit_run(
        txn,
        run_id,
        Some(principal),
        "RERUN_QUEUED",
        "Evaluation rerun queued.",
    )
    .await?;
    enqueue(txn, run_id, EvaluationOutboxEventType::Start, None).await?;
    let value = run(txn, principal, run_id, true)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no evaluation run with id {run_id}")))?;
    Ok(EvaluationMutationResult::run(value))
}
