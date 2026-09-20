//! Ports `PostgresEvaluationRepository`'s 8 mutations and their shared
//! `command`/`transaction`/`idempotentMutation`/`receipt` machinery. Java's
//! `ReplayException`/`IdempotencyException` become plain `Result` branches
//! here: a unique-violation on the receipt insert is caught by the caller
//! (matching `command()`'s `ReplayException` catch); a fingerprint mismatch
//! on an existing receipt is returned directly as a refusal (matching
//! `transaction()`'s `IdempotencyException` catch) instead of thrown.

use hive_application::configuration::canonical::digest as sha256;
use hive_application::evaluation::document;
use hive_application::evaluation::{
    EvaluationMutationResult, EvaluationProblem, EvaluationRunStatus,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, TransactionTrait};
use uuid::Uuid;

use super::queries::{
    self, can, definition, draft, next_version_number, project_active, run, version,
    version_for_digest,
};
use super::rows::{definition_project, diagnostics_json, version_project, Target};
use crate::capability::tx;
use crate::sql::{is_serialization_failure_db, is_unique_violation_db};

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

/// `Ok(None)`: no receipt yet, proceed as a new command. `Ok(Some(_))`: either the matched replay
/// result (fingerprint equal) or an idempotency-conflict refusal (fingerprint mismatch) — Java
/// throws `IdempotencyException` for the mismatch case instead of returning it, but the caller-side
/// effect (roll back, refuse) is identical either way.
async fn idempotent_mutation(
    db: &impl ConnectionTrait,
    project: Uuid,
    principal: Uuid,
    action: &str,
    key: &str,
    fingerprint: &str,
) -> Result<Option<EvaluationMutationResult>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT request_fingerprint, definition_id, definition_version_id, run_id FROM evaluation_command_receipts \
         WHERE project_id = $1 AND principal_id = $2 AND action = $3 AND idempotency_key = $4",
        [project.into(), principal.into(), action.into(), key.into()],
    );
    let Some(row) = db.query_one_raw(statement).await? else {
        return Ok(None);
    };
    let stored_fingerprint: String = row.try_get_by("request_fingerprint")?;
    if stored_fingerprint != fingerprint {
        return Ok(Some(EvaluationMutationResult::refused(
            EvaluationProblem::idempotency(),
        )));
    }
    let definition_id: Option<Uuid> = row.try_get_by("definition_id")?;
    let version_id: Option<Uuid> = row.try_get_by("definition_version_id")?;
    let run_id: Option<Uuid> = row.try_get_by("run_id")?;
    if let Some(definition_id) = definition_id {
        let value = definition(db, principal, definition_id, true)
            .await?
            .ok_or_else(|| {
                DbErr::RecordNotFound(format!("no evaluation definition with id {definition_id}"))
            })?;
        return Ok(Some(EvaluationMutationResult::definition(value)));
    }
    if let Some(version_id) = version_id {
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
    let run_id = run_id.ok_or_else(|| {
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
    action: &str,
    key: &str,
    fingerprint: &str,
    definition_id: Option<Uuid>,
    version_id: Option<Uuid>,
    run_id: Option<Uuid>,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO evaluation_command_receipts (id, project_id, principal_id, action, idempotency_key, request_fingerprint, definition_id, definition_version_id, run_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        [
            Uuid::new_v4().into(),
            project.into(),
            principal.into(),
            action.into(),
            key.into(),
            fingerprint.into(),
            definition_id.into(),
            version_id.into(),
            run_id.into(),
        ],
    );
    db.execute_raw(statement).await?;
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
    let mut values: Vec<sea_orm::Value> = vec![
        Uuid::new_v4().into(),
        definition_id.into(),
        actor.into(),
        action.into(),
        summary.into(),
    ];
    values.extend(crate::audit::context::audit_metadata_values());
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO evaluation_audit_events (id, definition_id, actor_principal_id, action, facts, request_id, correlation_id, graphql_operation, source_ip, user_agent) \
         VALUES ($1, $2, $3, $4, jsonb_build_object('summary', $5::text), $6, $7, $8, $9, $10)",
        values,
    );
    db.execute_raw(statement).await?;
    Ok(())
}

pub(super) async fn audit_run(
    db: &impl ConnectionTrait,
    run_id: Uuid,
    actor: Option<Uuid>,
    action: &str,
    summary: &str,
) -> Result<(), DbErr> {
    let mut values: Vec<sea_orm::Value> = vec![
        Uuid::new_v4().into(),
        run_id.into(),
        actor.into(),
        action.into(),
        summary.into(),
    ];
    values.extend(crate::audit::context::audit_metadata_values());
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO evaluation_audit_events (id, run_id, actor_principal_id, action, facts, request_id, correlation_id, graphql_operation, source_ip, user_agent) \
         VALUES ($1, $2, $3, $4, jsonb_build_object('summary', $5::text), $6, $7, $8, $9, $10)",
        values,
    );
    db.execute_raw(statement).await?;
    Ok(())
}

async fn insert_draft(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
    document: &str,
    diagnostics: &[document::EvaluationDiagnostic],
    based_on: Option<Uuid>,
) -> Result<(), DbErr> {
    let status = if diagnostics.iter().any(|value| value.severity == "ERROR") {
        "INVALID"
    } else {
        "VALID"
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO evaluation_definition_drafts (definition_id, canonical_document, validation_status, diagnostics, based_on_version_id) \
         VALUES ($1, $2::jsonb, $3, $4::jsonb, $5)",
        [
            definition_id.into(),
            document.into(),
            status.into(),
            diagnostics_json(diagnostics).into(),
            based_on.into(),
        ],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

async fn replace_draft(
    db: &impl ConnectionTrait,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    document_text: &str,
    based_on: Option<Uuid>,
    action: &str,
) -> Result<EvaluationMutationResult, DbErr> {
    let diagnostics = document::validate(document_text);
    let status = if diagnostics.iter().any(|value| value.severity == "ERROR") {
        "INVALID"
    } else {
        "VALID"
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE evaluation_definition_drafts SET canonical_document = $1::jsonb, revision = revision + 1, validation_status = $2, diagnostics = $3::jsonb, \
             based_on_version_id = $4, updated_at = CURRENT_TIMESTAMP WHERE definition_id = $5 AND revision = $6",
        [
            document_text.into(),
            status.into(),
            diagnostics_json(&diagnostics).into(),
            based_on.into(),
            definition_id.into(),
            expected_revision.into(),
        ],
    );
    let updated = db.execute_raw(statement).await?;
    if updated.rows_affected() != 1 {
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO evaluation_target_snapshots (run_id,agent_version_id,deployment_id,environment_definition_version_id,logical_environment_class,agent_content_digest,target_digest,plan_digest,package_digest,binding_digest,catalog_release_id,catalog_release_digest,environment_content_digest) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        [
            run.into(),
            target.agent_version_id.into(),
            target.deployment_id.into(),
            target.environment_definition_version_id.into(),
            target.environment_class.clone().into(),
            target.agent_digest.clone().into(),
            target.target_digest.clone().into(),
            target.plan_digest.clone().into(),
            target.package_digest.clone().into(),
            target.binding_digest.clone().into(),
            target.catalog_release_id.clone().into(),
            target.catalog_release_digest.clone().into(),
            target.environment_digest.clone().into(),
        ],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

pub(super) async fn insert_cases(
    db: &impl ConnectionTrait,
    run: Uuid,
    document_text: &str,
) -> Result<(), DbErr> {
    for case in document::cases(document_text) {
        let statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO evaluation_case_runs (id, run_id, case_key, ordinal) VALUES ($1, $2, $3, $4)",
            [
                Uuid::new_v4().into(),
                run.into(),
                case.key.clone().into(),
                case.ordinal.into(),
            ],
        );
        db.execute_raw(statement).await?;
    }
    Ok(())
}

pub(super) const NO_CASE_SLOT: Uuid = Uuid::nil();

pub(super) async fn enqueue(
    db: &impl ConnectionTrait,
    run: Uuid,
    event: &str,
    case_run: Option<Uuid>,
) -> Result<(), DbErr> {
    let slot = case_run.unwrap_or(NO_CASE_SLOT);
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO evaluation_outbox_events (id, run_id, event_type, case_run_id, case_slot) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (run_id, event_type, case_slot) DO NOTHING",
        [
            Uuid::new_v4().into(),
            run.into(),
            event.into(),
            case_run.into(),
            slot.into(),
        ],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

pub async fn create_definition(
    db: &DatabaseConnection,
    principal: Uuid,
    project: Uuid,
    slug: &str,
    document_text: Option<&str>,
    key: &str,
) -> Result<EvaluationMutationResult, DbErr> {
    let source = document_text
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
            // A 23505 here is either the receipt's own unique constraint (idempotency replay) or the
            // definitions table's (project_id, slug) constraint (a genuinely new slug collision, not
            // an idempotency replay). Try the receipt replay path first; if it finds nothing, this
            // was the slug collision, which Java reports as VALIDATION too (`insert.executeUpdate()`'s
            // own `23505` catch in `createDefinition`).
            let retry = db.begin().await?;
            let replay =
                idempotent_mutation(&retry, project, principal, "CREATE", key, &fingerprint)
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
) -> Result<EvaluationMutationResult, DbErr> {
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
    if let Some(replay) =
        idempotent_mutation(txn, project, principal, "CREATE", key, fingerprint).await?
    {
        return Ok(replay);
    }
    let id = Uuid::new_v4();
    let insert_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO evaluation_definitions (id, project_id, slug, created_by) VALUES ($1, $2, $3, $4)",
        [id.into(), project.into(), slug.into(), principal.into()],
    );
    txn.execute_raw(insert_statement).await?;
    let diagnostics = document::validate(canonical);
    insert_draft(txn, id, canonical, &diagnostics, None).await?;
    audit_definition(txn, id, principal, "CREATED").await?;
    receipt(
        txn,
        project,
        principal,
        "CREATE",
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
    based_on: Option<Uuid>,
    action: &str,
    receipt_action: &str,
    key: &str,
    fingerprint: &str,
) -> Result<EvaluationMutationResult, DbErr> {
    let txn = db.begin().await?;
    let result = draft_command_tx(
        &txn,
        principal,
        definition_id,
        expected_revision,
        capability,
        replacement,
        based_on,
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
    based_on: Option<Uuid>,
    action: &str,
    receipt_action: &str,
    key: &str,
    fingerprint: &str,
) -> Result<EvaluationMutationResult, DbErr> {
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
    let Some(current) = definition(txn, principal, definition_id, true).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if current.draft.revision != expected_revision {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::conflict(definition_id, expected_revision, current.draft.revision),
        ));
    }
    let result = match replacement {
        Some(document_text) => {
            replace_draft(
                txn,
                principal,
                definition_id,
                expected_revision,
                document_text,
                based_on,
                action,
            )
            .await?
        }
        None => {
            replace_draft(
                txn,
                principal,
                definition_id,
                expected_revision,
                &current.draft.canonical_document,
                current.draft.based_on_version_id,
                action,
            )
            .await?
        }
    };
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
    document_text: &str,
    key: &str,
) -> Result<EvaluationMutationResult, DbErr> {
    let Some(canonical) = document::canonicalize(document_text) else {
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
        None,
        "SAVED",
        "UPDATE",
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
) -> Result<EvaluationMutationResult, DbErr> {
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
        None,
        "VALIDATED",
        "VALIDATE",
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
) -> Result<EvaluationMutationResult, DbErr> {
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
) -> Result<EvaluationMutationResult, DbErr> {
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
    if let Some(replay) =
        idempotent_mutation(txn, project, principal, "DUPLICATE", key, fingerprint).await?
    {
        return Ok(replay);
    }
    let result = replace_draft(
        txn,
        principal,
        source.definition_id,
        expected_revision,
        &source.canonical_document,
        Some(source.id),
        "DUPLICATED",
    )
    .await?;
    if result.problem.is_none() {
        receipt(
            txn,
            project,
            principal,
            "DUPLICATE",
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
) -> Result<EvaluationMutationResult, DbErr> {
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
) -> Result<EvaluationMutationResult, DbErr> {
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
    if let Some(replay) =
        idempotent_mutation(txn, project, principal, "PUBLISH", key, fingerprint).await?
    {
        return Ok(replay);
    }
    let Some(current_definition) = definition(txn, principal, definition_id, true).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if current_definition.draft.revision != expected_revision {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::conflict(
                definition_id,
                expected_revision,
                current_definition.draft.revision,
            ),
        ));
    }
    let diagnostics = document::validate(&current_definition.draft.canonical_document);
    if diagnostics.iter().any(|value| value.severity == "ERROR") {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let digest = document::digest(&current_definition.draft.canonical_document);
    if let Some(existing) = version_for_digest(txn, definition_id, &digest).await? {
        return Ok(EvaluationMutationResult::version(
            current_definition,
            existing,
        ));
    }
    let number = next_version_number(txn, definition_id).await?;
    let id = Uuid::new_v4();
    let insert_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO evaluation_definition_versions (id, definition_id, version_number, canonical_document, content_digest, based_on_version_id, published_by) \
         VALUES ($1, $2, $3, $4::jsonb, $5, $6, $7)",
        [
            id.into(),
            definition_id.into(),
            number.into(),
            current_definition.draft.canonical_document.clone().into(),
            digest.into(),
            current_definition.draft.based_on_version_id.into(),
            principal.into(),
        ],
    );
    txn.execute_raw(insert_statement).await?;
    audit_definition(txn, definition_id, principal, "PUBLISHED").await?;
    receipt(
        txn,
        project,
        principal,
        "PUBLISH",
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
) -> Result<EvaluationMutationResult, DbErr> {
    if !valid_key(key) || !matches!(target_kind, "AGENT_VERSION" | "DEPLOYMENT") {
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
        target_kind,
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
    target_kind: &str,
    target_id: Uuid,
    environment_id: Uuid,
    key: &str,
    fingerprint: &str,
) -> Result<EvaluationMutationResult, DbErr> {
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
    if document::validate(&definition_version.canonical_document)
        .iter()
        .any(|value| value.severity == "ERROR")
    {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let Some(target) =
        queries::resolve_target(txn, project, target_kind, target_id, environment_id).await?
    else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::target(),
        ));
    };
    let kinds = document::target_kinds(&definition_version.canonical_document);
    let environments = document::environment_classes(&definition_version.canonical_document);
    if !kinds.contains(target_kind) || !environments.contains(&target.environment_class) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::target(),
        ));
    }
    if let Some(replay) =
        idempotent_mutation(txn, project, principal, "RUN", key, fingerprint).await?
    {
        return Ok(replay);
    }
    let run_id = Uuid::new_v4();
    let insert_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO evaluation_runs (id, project_id, definition_version_id, target_kind, target_id, environment_definition_version_id, requester_id, creation_fingerprint) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        [
            run_id.into(),
            project.into(),
            definition_version_id.into(),
            target_kind.into(),
            target_id.into(),
            environment_id.into(),
            principal.into(),
            fingerprint.into(),
        ],
    );
    txn.execute_raw(insert_statement).await?;
    insert_target(txn, run_id, &target).await?;
    insert_cases(txn, run_id, &definition_version.canonical_document).await?;
    receipt(
        txn,
        project,
        principal,
        "RUN",
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
    enqueue(txn, run_id, "START", None).await?;
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
) -> Result<EvaluationMutationResult, DbErr> {
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    // `pg_advisory_xact_lock('m16-evaluation-<run>')` (removed): every call site (here, `commit()`,
    // `retryOrDeadLetter()`) already takes a real `raw_run(..., true)` FOR UPDATE on this same
    // `evaluation_runs` row moments later, which DSQL's own commit-time OCC already serializes
    // concurrent transactions through — the same reasoning already confirmed for
    // `m14-approval-transition`.
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
) -> Result<EvaluationMutationResult, DbErr> {
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
    if let Some(replay) =
        idempotent_mutation(txn, raw.project_id, principal, "CANCEL", key, fingerprint).await?
    {
        return Ok(replay);
    }
    if raw.generation != expected_generation || !raw.status.may_cancel() {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    super::worker::update_run(
        txn,
        &raw,
        EvaluationRunStatus::Canceled,
        raw.generation + 1,
        Some("CANCELED"),
        Some("CANCELED"),
        true,
    )
    .await?;
    let case_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE evaluation_case_runs SET lifecycle_status = 'CANCELED', completed_at = CURRENT_TIMESTAMP WHERE run_id = $1 AND lifecycle_status IN ('QUEUED', 'RUNNING')",
        [run_id.into()],
    );
    txn.execute_raw(case_statement).await?;
    let outbox_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE evaluation_outbox_events SET status = 'CANCELED' WHERE run_id = $1 AND status IN ('PENDING', 'PROCESSING')",
        [run_id.into()],
    );
    txn.execute_raw(outbox_statement).await?;
    super::worker::insert_result(txn, run_id, false, "CANCELED", Some("CANCELED")).await?;
    receipt(
        txn,
        raw.project_id,
        principal,
        "CANCEL",
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

pub async fn rerun(
    db: &DatabaseConnection,
    principal: Uuid,
    source_run_id: Uuid,
    key: &str,
) -> Result<EvaluationMutationResult, DbErr> {
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
) -> Result<EvaluationMutationResult, DbErr> {
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
    if !source.status.is_terminal() {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    if let Some(replay) =
        idempotent_mutation(txn, source.project_id, principal, "RERUN", key, fingerprint).await?
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
    let kinds = document::target_kinds(&definition_version.canonical_document);
    let environments = document::environment_classes(&definition_version.canonical_document);
    if !kinds.contains(&source.target_kind) || !environments.contains(&target.environment_class) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::target(),
        ));
    }
    let run_id = Uuid::new_v4();
    let insert_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO evaluation_runs (id, project_id, definition_version_id, target_kind, target_id, environment_definition_version_id, requester_id, source_run_id, creation_fingerprint) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        [
            run_id.into(),
            source.project_id.into(),
            source.definition_version_id.into(),
            source.target_kind.clone().into(),
            source.target_id.into(),
            source.environment_id.into(),
            principal.into(),
            source_run_id.into(),
            fingerprint.into(),
        ],
    );
    txn.execute_raw(insert_statement).await?;
    insert_target(txn, run_id, &target).await?;
    insert_cases(txn, run_id, &definition_version.canonical_document).await?;
    receipt(
        txn,
        source.project_id,
        principal,
        "RERUN",
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
    enqueue(txn, run_id, "START", None).await?;
    let value = run(txn, principal, run_id, true)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no evaluation run with id {run_id}")))?;
    Ok(EvaluationMutationResult::run(value))
}
