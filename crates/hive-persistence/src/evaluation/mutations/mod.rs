//! The eight evaluation commands: the five that author a definition and its versions
//! (`createEvaluationDefinition`, `updateEvaluationDefinitionDraft`,
//! `validateEvaluationDefinitionDraft`, `duplicateEvaluationDefinitionVersionToDraft`,
//! `publishEvaluationDefinitionDraft`) in `definitions`, and the three that drive a run
//! (`runEvaluation`, `cancelEvaluation`, `rerunEvaluation`) in `runs`.
//!
//! Held here is the receipt skeleton both families share. Each command runs in one transaction:
//! it re-checks the principal's authority with the evaluator's locking checks, takes its row
//! locks, compares the expected revision or generation, writes with that revision or generation in
//! the `WHERE` clause, records its audit row, and stores a command receipt before it commits. A
//! command answers with the stored `evaluation_definitions`, `evaluation_definition_versions` or
//! `evaluation_runs` row itself; the GraphQL payload exposes it as the same generated type the
//! reads use.
//!
//! A unique violation on the receipt insert is the idempotency replay (the caller retries the
//! lookup); a fingerprint mismatch on an existing receipt is returned as a refusal.

mod definitions;
mod runs;

pub use definitions::{
    create_definition, duplicate_version, publish_draft, update_draft, validate_draft,
};
pub use runs::{cancel, rerun, run_evaluation};
pub(super) use runs::{cancel_open_cases, cancel_open_events, enqueue};

use super::queries::{definition, run, version};
use crate::entity::enums::{DraftValidationStatus, EvaluationCommandAction};
use crate::entity::{
    evaluation_audit_events, evaluation_command_receipts, evaluation_definition_versions,
    evaluation_definitions, evaluation_runs,
};
use hive_application::evaluation::document;
use hive_application::evaluation::{EvaluationMutationResult, EvaluationProblem};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, NotSet, QueryFilter, Set};
use uuid::Uuid;

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

/// Which row a command receipt points at: a definition, one of its versions, or a run. Exactly
/// one is set. Named rather than positional because all three are `Option<Uuid>` and sit next to
/// each other, so a transposed pair would compile and file the receipt against the wrong row.
#[derive(Default)]
struct ReceiptSubject {
    definition_id: Option<Uuid>,
    version_id: Option<Uuid>,
    run_id: Option<Uuid>,
}

async fn receipt(
    db: &impl ConnectionTrait,
    project: Uuid,
    principal: Uuid,
    action: EvaluationCommandAction,
    key: &str,
    fingerprint: &str,
    subject: ReceiptSubject,
) -> Result<(), DbErr> {
    evaluation_command_receipts::Entity::insert(evaluation_command_receipts::ActiveModel {
        id: Set(Uuid::new_v4()),
        project_id: Set(project),
        principal_id: Set(principal),
        action: Set(action),
        idempotency_key: Set(key.to_string()),
        request_fingerprint: Set(fingerprint.to_string()),
        definition_id: Set(subject.definition_id),
        definition_version_id: Set(subject.version_id),
        run_id: Set(subject.run_id),
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

#[cfg(test)]
mod command_rule_tests {
    use super::{valid_key, valid_slug, validation_status};
    use crate::entity::enums::DraftValidationStatus;
    use hive_application::evaluation::document::EvaluationDiagnostic;

    fn diagnostic(severity: &str) -> EvaluationDiagnostic {
        EvaluationDiagnostic {
            code: "CODE".to_string(),
            severity: severity.to_string(),
            message: "message".to_string(),
            path: Vec::new(),
        }
    }

    /// The bounds the column's own `CHECK` enforces, measured on the trimmed value, so a key that
    /// is only long enough with its padding is refused here rather than by the database.
    #[test]
    fn an_idempotency_key_is_measured_trimmed_and_bounded_at_both_ends() {
        assert!(!valid_key(""));
        assert!(!valid_key("1234567"));
        assert!(valid_key("12345678"));
        assert!(valid_key(&"k".repeat(160)));
        assert!(!valid_key(&"k".repeat(161)));
        assert!(!valid_key("  123456  "));
        assert!(valid_key("  12345678  "));
        assert!(!valid_key(&" ".repeat(200)));
    }

    #[test]
    fn a_slug_starts_lowercase_and_carries_only_lowercase_digits_and_hyphens() {
        assert!(valid_slug("a"));
        assert!(valid_slug("nightly-regression-2"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("2-leading-digit"));
        assert!(!valid_slug("-leading-hyphen"));
        assert!(!valid_slug("Upper"));
        assert!(!valid_slug("has upper Case"));
        assert!(!valid_slug("under_score"));
        assert!(!valid_slug("dot.separated"));
    }

    /// The length bound is on the whole slug, and it is counted in bytes, so a multi-byte
    /// character costs what the column charges for it.
    #[test]
    fn a_slug_is_bounded_at_a_hundred_and_twenty_bytes() {
        assert!(valid_slug(&format!("a{}", "b".repeat(119))));
        assert!(!valid_slug(&format!("a{}", "b".repeat(120))));
        assert!(!valid_slug(&format!("a{}", "é".repeat(60))));
    }

    /// One `ERROR` invalidates the draft whatever else is present; a draft with only warnings is
    /// valid, and so is one with no diagnostics at all.
    #[test]
    fn any_error_severity_diagnostic_makes_a_draft_invalid() {
        assert_eq!(validation_status(&[]), DraftValidationStatus::Valid);
        assert_eq!(
            validation_status(&[diagnostic("WARNING")]),
            DraftValidationStatus::Valid
        );
        assert_eq!(
            validation_status(&[diagnostic("WARNING"), diagnostic("ERROR")]),
            DraftValidationStatus::Invalid
        );
        // The comparison is exact: a lower-case severity is not an error.
        assert_eq!(
            validation_status(&[diagnostic("error")]),
            DraftValidationStatus::Valid
        );
    }
}
