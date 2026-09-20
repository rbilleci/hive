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
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

use super::queries::{
    self, can, definition, draft, next_version_number, project_active, run, version,
    version_for_digest,
};
use super::rows::{definition_project, diagnostics_json, version_project, Target};
use crate::capability::tx;
use crate::sql::is_serialization_failure;

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db_error) if db_error.code().as_deref() == Some("23505"))
}

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
    conn: &mut PgConnection,
    project: Uuid,
    principal: Uuid,
    action: &str,
    key: &str,
    fingerprint: &str,
) -> Result<Option<EvaluationMutationResult>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT request_fingerprint, definition_id, definition_version_id, run_id FROM evaluation_command_receipts \
         WHERE project_id = $1 AND principal_id = $2 AND action = $3 AND idempotency_key = $4",
    )
    .bind(project)
    .bind(principal)
    .bind(action)
    .bind(key)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored_fingerprint: String = row.get(0);
    if stored_fingerprint != fingerprint {
        return Ok(Some(EvaluationMutationResult::refused(
            EvaluationProblem::idempotency(),
        )));
    }
    let definition_id: Option<Uuid> = row.get(1);
    let version_id: Option<Uuid> = row.get(2);
    let run_id: Option<Uuid> = row.get(3);
    if let Some(definition_id) = definition_id {
        let value = definition(conn, principal, definition_id, true)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;
        return Ok(Some(EvaluationMutationResult::definition(value)));
    }
    if let Some(version_id) = version_id {
        let value = version(conn, version_id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;
        let owner = definition(conn, principal, value.definition_id, true)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;
        return Ok(Some(EvaluationMutationResult::version(owner, value)));
    }
    let run_id = run_id.ok_or(sqlx::Error::RowNotFound)?;
    let value = run(conn, principal, run_id, true)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(Some(EvaluationMutationResult::run(value)))
}

#[allow(clippy::too_many_arguments)]
async fn receipt(
    conn: &mut PgConnection,
    project: Uuid,
    principal: Uuid,
    action: &str,
    key: &str,
    fingerprint: &str,
    definition_id: Option<Uuid>,
    version_id: Option<Uuid>,
    run_id: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO evaluation_command_receipts (id, project_id, principal_id, action, idempotency_key, request_fingerprint, definition_id, definition_version_id, run_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(Uuid::new_v4())
    .bind(project)
    .bind(principal)
    .bind(action)
    .bind(key)
    .bind(fingerprint)
    .bind(definition_id)
    .bind(version_id)
    .bind(run_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn audit_definition(
    conn: &mut PgConnection,
    definition_id: Uuid,
    actor: Uuid,
    action: &str,
) -> Result<(), sqlx::Error> {
    let summary = format!(
        "Evaluation definition {}.",
        action.to_lowercase().replace('_', " ")
    );
    crate::audit::bind_audit_metadata(
        sqlx::query(
            "INSERT INTO evaluation_audit_events (id, definition_id, actor_principal_id, action, facts, request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, $3, $4, jsonb_build_object('summary', $5::text), $6, $7, $8, $9, $10)",
        )
        .bind(Uuid::new_v4())
        .bind(definition_id)
        .bind(actor)
        .bind(action)
        .bind(&summary),
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub(super) async fn audit_run(
    conn: &mut PgConnection,
    run_id: Uuid,
    actor: Option<Uuid>,
    action: &str,
    summary: &str,
) -> Result<(), sqlx::Error> {
    crate::audit::bind_audit_metadata(
        sqlx::query(
            "INSERT INTO evaluation_audit_events (id, run_id, actor_principal_id, action, facts, request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, $3, $4, jsonb_build_object('summary', $5::text), $6, $7, $8, $9, $10)",
        )
        .bind(Uuid::new_v4())
        .bind(run_id)
        .bind(actor)
        .bind(action)
        .bind(summary),
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn insert_draft(
    conn: &mut PgConnection,
    definition_id: Uuid,
    document: &str,
    diagnostics: &[document::EvaluationDiagnostic],
    based_on: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let status = if diagnostics.iter().any(|value| value.severity == "ERROR") {
        "INVALID"
    } else {
        "VALID"
    };
    sqlx::query(
        "INSERT INTO evaluation_definition_drafts (definition_id, canonical_document, validation_status, diagnostics, based_on_version_id) \
         VALUES ($1, $2::jsonb, $3, $4::jsonb, $5)",
    )
    .bind(definition_id)
    .bind(document)
    .bind(status)
    .bind(diagnostics_json(diagnostics))
    .bind(based_on)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn replace_draft(
    conn: &mut PgConnection,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    document_text: &str,
    based_on: Option<Uuid>,
    action: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    let diagnostics = document::validate(document_text);
    let status = if diagnostics.iter().any(|value| value.severity == "ERROR") {
        "INVALID"
    } else {
        "VALID"
    };
    let updated = sqlx::query(
        "UPDATE evaluation_definition_drafts SET canonical_document = $1::jsonb, revision = revision + 1, validation_status = $2, diagnostics = $3::jsonb, \
             based_on_version_id = $4, updated_at = CURRENT_TIMESTAMP WHERE definition_id = $5 AND revision = $6",
    )
    .bind(document_text)
    .bind(status)
    .bind(diagnostics_json(&diagnostics))
    .bind(based_on)
    .bind(definition_id)
    .bind(expected_revision)
    .execute(&mut *conn)
    .await?;
    if updated.rows_affected() != 1 {
        let current = draft(conn, definition_id, true).await?;
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::conflict(definition_id, expected_revision, current.revision),
        ));
    }
    audit_definition(conn, definition_id, principal, action).await?;
    let value = definition(conn, principal, definition_id, true)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(EvaluationMutationResult::definition(value))
}

pub(super) async fn insert_target(
    conn: &mut PgConnection,
    run: Uuid,
    target: &Target,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO evaluation_target_snapshots (run_id,agent_version_id,deployment_id,environment_definition_version_id,logical_environment_class,agent_content_digest,target_digest,plan_digest,package_digest,binding_digest,catalog_release_id,catalog_release_digest,environment_content_digest) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
    )
    .bind(run)
    .bind(target.agent_version_id)
    .bind(target.deployment_id)
    .bind(target.environment_definition_version_id)
    .bind(&target.environment_class)
    .bind(&target.agent_digest)
    .bind(&target.target_digest)
    .bind(&target.plan_digest)
    .bind(&target.package_digest)
    .bind(&target.binding_digest)
    .bind(&target.catalog_release_id)
    .bind(&target.catalog_release_digest)
    .bind(&target.environment_digest)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub(super) async fn insert_cases(
    conn: &mut PgConnection,
    run: Uuid,
    document_text: &str,
) -> Result<(), sqlx::Error> {
    for case in document::cases(document_text) {
        sqlx::query("INSERT INTO evaluation_case_runs (id, run_id, case_key, ordinal) VALUES ($1, $2, $3, $4)")
            .bind(Uuid::new_v4())
            .bind(run)
            .bind(&case.key)
            .bind(case.ordinal)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

pub(super) const NO_CASE_SLOT: Uuid = Uuid::nil();

pub(super) async fn enqueue(
    conn: &mut PgConnection,
    run: Uuid,
    event: &str,
    case_run: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let slot = case_run.unwrap_or(NO_CASE_SLOT);
    sqlx::query(
        "INSERT INTO evaluation_outbox_events (id, run_id, event_type, case_run_id, case_slot) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (run_id, event_type, case_slot) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(run)
    .bind(event)
    .bind(case_run)
    .bind(slot)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn create_definition(
    pool: &PgPool,
    principal: Uuid,
    project: Uuid,
    slug: &str,
    document_text: Option<&str>,
    key: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
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
    let mut tx = pool.begin().await?;
    let result = create_definition_tx(
        &mut tx,
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
            tx.commit().await?;
            Ok(value)
        }
        Err(error) if is_unique_violation(&error) => {
            let _ = tx.rollback().await;
            // A 23505 here is either the receipt's own unique constraint (idempotency replay) or the
            // definitions table's (project_id, slug) constraint (a genuinely new slug collision, not
            // an idempotency replay). Try the receipt replay path first; if it finds nothing, this
            // was the slug collision, which Java reports as VALIDATION too (`insert.executeUpdate()`'s
            // own `23505` catch in `createDefinition`).
            let mut retry = pool.begin().await?;
            match idempotent_mutation(&mut retry, project, principal, "CREATE", key, &fingerprint)
                .await?
            {
                Some(value) => {
                    retry.commit().await?;
                    Ok(value)
                }
                None => {
                    retry.commit().await?;
                    Ok(EvaluationMutationResult::refused(
                        EvaluationProblem::validation(),
                    ))
                }
            }
        }
        Err(error) => Err(error),
    }
}

async fn create_definition_tx(
    tx: &mut PgConnection,
    principal: Uuid,
    project: Uuid,
    slug: &str,
    canonical: &str,
    key: &str,
    fingerprint: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    if !can(tx, principal, tx::EVALUATION_DEFINITION_VIEW, project, true).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    if !can(
        tx,
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
    if !project_active(tx, project).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    if let Some(replay) =
        idempotent_mutation(tx, project, principal, "CREATE", key, fingerprint).await?
    {
        return Ok(replay);
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO evaluation_definitions (id, project_id, slug, created_by) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(project)
        .bind(slug)
        .bind(principal)
        .execute(&mut *tx)
        .await?;
    let diagnostics = document::validate(canonical);
    insert_draft(tx, id, canonical, &diagnostics, None).await?;
    audit_definition(tx, id, principal, "CREATED").await?;
    receipt(
        tx,
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
    let value = definition(tx, principal, id, true)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(EvaluationMutationResult::definition(value))
}

#[allow(clippy::too_many_arguments)]
async fn draft_command(
    pool: &PgPool,
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
) -> Result<EvaluationMutationResult, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let result = draft_command_tx(
        &mut tx,
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
            tx.commit().await?;
            Ok(value)
        }
        Err(error) if is_serialization_failure(&error) => {
            let mut retry = pool.begin().await?;
            let current = draft(&mut retry, definition_id, false).await?;
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
    tx: &mut PgConnection,
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
) -> Result<EvaluationMutationResult, sqlx::Error> {
    let Some(project) = definition_project(tx, definition_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if !can(tx, principal, tx::EVALUATION_DEFINITION_VIEW, project, true).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    if !can(tx, principal, capability, project, true).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::forbidden(),
        ));
    }
    if !project_active(tx, project).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    if let Some(replay) =
        idempotent_mutation(tx, project, principal, receipt_action, key, fingerprint).await?
    {
        return Ok(replay);
    }
    let Some(current) = definition(tx, principal, definition_id, true).await? else {
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
                tx,
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
                tx,
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
            tx,
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
    pool: &PgPool,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    document_text: &str,
    key: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
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
        pool,
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
    pool: &PgPool,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    key: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!("{definition_id}|{expected_revision}"));
    draft_command(
        pool,
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
    pool: &PgPool,
    principal: Uuid,
    version_id: Uuid,
    expected_revision: i64,
    key: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!("{version_id}|{expected_revision}"));
    let mut tx = pool.begin().await?;
    let result = duplicate_version_tx(
        &mut tx,
        principal,
        version_id,
        expected_revision,
        key,
        &fingerprint,
    )
    .await;
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) if is_serialization_failure(&error) => {
            let mut retry = pool.begin().await?;
            let Some(source) = version(&mut retry, version_id).await? else {
                retry.commit().await?;
                return Ok(EvaluationMutationResult::refused(
                    EvaluationProblem::not_found(),
                ));
            };
            let current = draft(&mut retry, source.definition_id, false).await?;
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
    tx: &mut PgConnection,
    principal: Uuid,
    version_id: Uuid,
    expected_revision: i64,
    key: &str,
    fingerprint: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    let Some(source) = version(tx, version_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    let Some(project) = version_project(tx, version_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if !can(tx, principal, tx::EVALUATION_DEFINITION_VIEW, project, true).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    if !can(
        tx,
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
    if !project_active(tx, project).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    if let Some(replay) =
        idempotent_mutation(tx, project, principal, "DUPLICATE", key, fingerprint).await?
    {
        return Ok(replay);
    }
    let result = replace_draft(
        tx,
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
            tx,
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
    pool: &PgPool,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    key: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!("{definition_id}|{expected_revision}"));
    let mut tx = pool.begin().await?;
    let result = publish_draft_tx(
        &mut tx,
        principal,
        definition_id,
        expected_revision,
        key,
        &fingerprint,
    )
    .await;
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) if is_serialization_failure(&error) => {
            let mut retry = pool.begin().await?;
            let current = draft(&mut retry, definition_id, false).await?;
            retry.commit().await?;
            Ok(EvaluationMutationResult::refused(
                EvaluationProblem::conflict(definition_id, expected_revision, current.revision),
            ))
        }
        Err(error) => Err(error),
    }
}

async fn publish_draft_tx(
    tx: &mut PgConnection,
    principal: Uuid,
    definition_id: Uuid,
    expected_revision: i64,
    key: &str,
    fingerprint: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    let Some(project) = definition_project(tx, definition_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if !can(tx, principal, tx::EVALUATION_DEFINITION_VIEW, project, true).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    if !can(
        tx,
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
    if !project_active(tx, project).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    if let Some(replay) =
        idempotent_mutation(tx, project, principal, "PUBLISH", key, fingerprint).await?
    {
        return Ok(replay);
    }
    let Some(current_definition) = definition(tx, principal, definition_id, true).await? else {
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
    if let Some(existing) = version_for_digest(tx, definition_id, &digest).await? {
        return Ok(EvaluationMutationResult::version(
            current_definition,
            existing,
        ));
    }
    let number = next_version_number(tx, definition_id).await?;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO evaluation_definition_versions (id, definition_id, version_number, canonical_document, content_digest, based_on_version_id, published_by) \
         VALUES ($1, $2, $3, $4::jsonb, $5, $6, $7)",
    )
    .bind(id)
    .bind(definition_id)
    .bind(number)
    .bind(&current_definition.draft.canonical_document)
    .bind(&digest)
    .bind(current_definition.draft.based_on_version_id)
    .bind(principal)
    .execute(&mut *tx)
    .await?;
    audit_definition(tx, definition_id, principal, "PUBLISHED").await?;
    receipt(
        tx,
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
    let owner = definition(tx, principal, definition_id, true)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    let value = version(tx, id).await?.ok_or(sqlx::Error::RowNotFound)?;
    Ok(EvaluationMutationResult::version(owner, value))
}

#[allow(clippy::too_many_arguments)]
pub async fn run_evaluation(
    pool: &PgPool,
    principal: Uuid,
    project: Uuid,
    definition_version_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    environment_id: Uuid,
    key: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    if !valid_key(key) || !matches!(target_kind, "AGENT_VERSION" | "DEPLOYMENT") {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!(
        "{definition_version_id}|{target_kind}|{target_id}|{environment_id}"
    ));
    let mut tx = pool.begin().await?;
    let result = run_evaluation_tx(
        &mut tx,
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
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_evaluation_tx(
    tx: &mut PgConnection,
    principal: Uuid,
    project: Uuid,
    definition_version_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    environment_id: Uuid,
    key: &str,
    fingerprint: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    if !can(tx, principal, tx::EVALUATION_RUN_VIEW, project, true).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    if !can(tx, principal, tx::EVALUATION_RUN_RUN, project, true).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::forbidden(),
        ));
    }
    if !project_active(tx, project).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    let Some(definition_version) = version(tx, definition_version_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if version_project(tx, definition_version_id).await? != Some(project) {
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
        queries::resolve_target(tx, project, target_kind, target_id, environment_id).await?
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
        idempotent_mutation(tx, project, principal, "RUN", key, fingerprint).await?
    {
        return Ok(replay);
    }
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO evaluation_runs (id, project_id, definition_version_id, target_kind, target_id, environment_definition_version_id, requester_id, creation_fingerprint) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(run_id)
    .bind(project)
    .bind(definition_version_id)
    .bind(target_kind)
    .bind(target_id)
    .bind(environment_id)
    .bind(principal)
    .bind(fingerprint)
    .execute(&mut *tx)
    .await?;
    insert_target(tx, run_id, &target).await?;
    insert_cases(tx, run_id, &definition_version.canonical_document).await?;
    receipt(
        tx,
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
        tx,
        run_id,
        Some(principal),
        "QUEUED",
        "Evaluation run queued.",
    )
    .await?;
    enqueue(tx, run_id, "START", None).await?;
    let value = run(tx, principal, run_id, true)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(EvaluationMutationResult::run(value))
}

pub async fn cancel(
    pool: &PgPool,
    principal: Uuid,
    run_id: Uuid,
    expected_generation: i64,
    key: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
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
    let mut tx = pool.begin().await?;
    let result = cancel_tx(
        &mut tx,
        principal,
        run_id,
        expected_generation,
        key,
        &fingerprint,
    )
    .await;
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) if is_serialization_failure(&error) => {
            let mut retry = pool.begin().await?;
            let current = queries::raw_run(&mut retry, run_id, false).await?;
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
    tx: &mut PgConnection,
    principal: Uuid,
    run_id: Uuid,
    expected_generation: i64,
    key: &str,
    fingerprint: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    let Some(raw) = queries::raw_run(tx, run_id, true).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if !can(tx, principal, tx::EVALUATION_RUN_VIEW, raw.project_id, true).await? {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    }
    if !can(
        tx,
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
        idempotent_mutation(tx, raw.project_id, principal, "CANCEL", key, fingerprint).await?
    {
        return Ok(replay);
    }
    if raw.generation != expected_generation || !raw.status.may_cancel() {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    super::worker::update_run(
        tx,
        &raw,
        EvaluationRunStatus::Canceled,
        raw.generation + 1,
        Some("CANCELED"),
        Some("CANCELED"),
        true,
    )
    .await?;
    sqlx::query(
        "UPDATE evaluation_case_runs SET lifecycle_status = 'CANCELED', completed_at = CURRENT_TIMESTAMP WHERE run_id = $1 AND lifecycle_status IN ('QUEUED', 'RUNNING')",
    )
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE evaluation_outbox_events SET status = 'CANCELED' WHERE run_id = $1 AND status IN ('PENDING', 'PROCESSING')")
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    super::worker::insert_result(tx, run_id, false, "CANCELED", Some("CANCELED")).await?;
    receipt(
        tx,
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
        tx,
        run_id,
        Some(principal),
        "CANCELED",
        "Evaluation run canceled.",
    )
    .await?;
    let value = run(tx, principal, run_id, true)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(EvaluationMutationResult::run(value))
}

pub async fn rerun(
    pool: &PgPool,
    principal: Uuid,
    source_run_id: Uuid,
    key: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    if !valid_key(key) {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::validation(),
        ));
    }
    let fingerprint = sha256(&format!("{source_run_id}|rerun"));
    let mut tx = pool.begin().await?;
    let result = rerun_tx(&mut tx, principal, source_run_id, key, &fingerprint).await;
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

async fn rerun_tx(
    tx: &mut PgConnection,
    principal: Uuid,
    source_run_id: Uuid,
    key: &str,
    fingerprint: &str,
) -> Result<EvaluationMutationResult, sqlx::Error> {
    let Some(source) = queries::raw_run(tx, source_run_id, true).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::not_found(),
        ));
    };
    if !can(
        tx,
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
        tx,
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
    if !project_active(tx, source.project_id).await? {
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
        idempotent_mutation(tx, source.project_id, principal, "RERUN", key, fingerprint).await?
    {
        return Ok(replay);
    }
    let Some(definition_version) = version(tx, source.definition_version_id).await? else {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::target(),
        ));
    };
    let Some(target) = queries::target_from_snapshot(tx, source_run_id).await? else {
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
    sqlx::query(
        "INSERT INTO evaluation_runs (id, project_id, definition_version_id, target_kind, target_id, environment_definition_version_id, requester_id, source_run_id, creation_fingerprint) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(run_id)
    .bind(source.project_id)
    .bind(source.definition_version_id)
    .bind(&source.target_kind)
    .bind(source.target_id)
    .bind(source.environment_id)
    .bind(principal)
    .bind(source_run_id)
    .bind(fingerprint)
    .execute(&mut *tx)
    .await?;
    insert_target(tx, run_id, &target).await?;
    insert_cases(tx, run_id, &definition_version.canonical_document).await?;
    receipt(
        tx,
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
        tx,
        run_id,
        Some(principal),
        "RERUN_QUEUED",
        "Evaluation rerun queued.",
    )
    .await?;
    enqueue(tx, run_id, "START", None).await?;
    let value = run(tx, principal, run_id, true)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(EvaluationMutationResult::run(value))
}
