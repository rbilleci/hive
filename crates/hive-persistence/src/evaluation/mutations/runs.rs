//! The three run commands — starting a run, canceling one, and rerunning a finished one — with
//! the row writes a run is made of: its target snapshot, its case rows, and the outbox events the
//! worker claims.
//!
//! A cancel is a generation-guarded write: the run's generation is in the `WHERE` clause, and the
//! open case rows and undelivered outbox events are closed with it.

use super::{audit_run, idempotent_mutation, receipt, valid_key, MutationResult, ReceiptSubject};
use crate::capability::tx;
use crate::entity::enums::{
    EvaluationCommandAction, EvaluationLifecycleStatus, EvaluationOutboxEventType,
    EvaluationOutboxStatus, EvaluationOutcomeCategory, EvaluationTargetKind,
};
use crate::entity::{
    evaluation_case_runs, evaluation_outbox_events, evaluation_runs, evaluation_target_snapshots,
};
use crate::evaluation::queries::{self, can, project_active, run, version};
use crate::evaluation::rows::{self, document_text, version_project, Target};
use crate::retry;
use hive_application::configuration::canonical::digest as sha256;
use hive_application::evaluation::document;
use hive_application::evaluation::{
    EvaluationMutationResult, EvaluationProblem, EvaluationRunStatus,
};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, NotSet,
    QueryFilter, Set, TransactionTrait,
};
use uuid::Uuid;

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

pub(in crate::evaluation) async fn enqueue(
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
        ReceiptSubject {
            run_id: Some(run_id),
            ..Default::default()
        },
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
        Err(error) if retry::is_serialization_failure_db(&error) => {
            let current = retry::reread(db, async |retry| {
                queries::raw_run(retry, run_id, false).await
            })
            .await?;
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
    if raw.generation != expected_generation || !rows::run_status(&raw).may_cancel() {
        return Ok(EvaluationMutationResult::refused(
            EvaluationProblem::lifecycle(),
        ));
    }
    crate::evaluation::worker::update_run(
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
    crate::evaluation::worker::insert_result(
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
        ReceiptSubject {
            run_id: Some(run_id),
            ..Default::default()
        },
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
pub(in crate::evaluation) async fn cancel_open_cases(
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
pub(in crate::evaluation) async fn cancel_open_events(
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
    if !rows::run_status(&source).is_terminal() {
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
        ReceiptSubject {
            run_id: Some(run_id),
            ..Default::default()
        },
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
