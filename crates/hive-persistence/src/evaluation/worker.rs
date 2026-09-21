//! The local outbox worker's durable half on SeaORM entities: `claim_next`, `commit`, `idle`,
//! `delivered`, `failed`, `claim_failed` and their private helpers (`claim`, `prepare_work`,
//! `start`, `execute_case`, `finalize_run`, `append_evidence`, `reclaim`, `retry_or_dead_letter`,
//! `heartbeat`).
//!
//! Every claim and every transition is a conditional `UPDATE` whose `WHERE` clause carries the
//! state it expects — the run's generation, the event's claim identity and attempt, the case's own
//! open statuses — so a lost race writes nothing and the caller's at-least-once loop retries.
//!
//! `append_evidence` is the cross-domain handoff into the deployment and approval domain, and is
//! the only place this module calls into `crate::deployment`.

use hive_application::configuration::canonical::digest as sha256;
use hive_application::evaluation::{
    EvaluationExecutionDecision, EvaluationFinalizationDecision, EvaluationWorkDecision,
    EvaluationWorkItem,
};
use sea_orm::prelude::Decimal;
use sea_orm::sea_query::{Alias, CaseStatement, Expr, ExprTrait, Func, IntoTableRef, OnConflict};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    FromQueryResult, JoinType, NotSet, QueryFilter, QueryOrder, QuerySelect, RelationTrait, Set,
    TransactionTrait,
};
use uuid::Uuid;

use crate::capability::queries::for_update_of;

use super::mutations::{audit_run, cancel_open_cases, cancel_open_events, enqueue};
use super::queries::{raw_run, version};
use super::rows::{self, document_text, Event};
use crate::entity::enums::{
    DeploymentEvidenceKind, EvaluationLifecycleStatus, EvaluationOutboxEventType,
    EvaluationOutboxStatus, EvaluationOutcomeCategory, EvaluationTargetKind, WorkerHeartbeatState,
};
use crate::entity::{
    deployment_approval_requirements, deployment_evidence_snapshots, deployment_policy_snapshots,
    deployments, evaluation_artifact_metadata, evaluation_case_runs, evaluation_metric_results,
    evaluation_outbox_events, evaluation_results, evaluation_runs, evaluation_target_snapshots,
    evaluation_worker_heartbeats,
};
use hive_application::evaluation::EvaluationRunStatus;

const DELIVERY_LIMIT: i32 = 4;

/// How long a claimed event may stay `PROCESSING` before `reclaim` frees it.
const CLAIM_TIMEOUT_SECONDS: i64 = 30;

/// At most this many queued events are counted for a heartbeat; beyond it the count is truncated.
const HEARTBEAT_QUEUE_LIMIT: u64 = 101;

/// The run's status, as the domain models it.
fn status_of(run: &evaluation_runs::Model) -> EvaluationRunStatus {
    rows::run_status(run)
}

/// The run's optimistic transition. It asserts no terminal state of its own: every call site
/// reaches it only after its own state-machine or status-list check decided the transition is
/// legal, and the `WHERE id = .. AND generation = ..` match is the real optimistic guard.
pub(super) async fn update_run(
    db: &impl ConnectionTrait,
    current: &evaluation_runs::Model,
    status: EvaluationRunStatus,
    generation: i64,
    outcome: Option<EvaluationOutcomeCategory>,
    code: Option<&str>,
    terminal: bool,
) -> Result<(), DbErr> {
    use evaluation_runs::Column;
    let updated = evaluation_runs::Entity::update_many()
        .col_expr(Column::LifecycleStatus, Expr::value(status.as_str()))
        .col_expr(Column::Generation, Expr::value(generation))
        .col_expr(
            Column::OutcomeCategory,
            Expr::value(outcome.map(|value| value.to_value())),
        )
        .col_expr(Column::OutcomeCode, Expr::value(code))
        .col_expr(
            Column::StartedAt,
            // `started_at` is stamped once, when the run first reaches RUNNING.
            CaseStatement::new()
                .case(
                    Expr::value(status.as_str()).eq(EvaluationRunStatus::Running.as_str()),
                    Expr::from(Func::coalesce([
                        Expr::col(Column::StartedAt),
                        Expr::current_timestamp(),
                    ])),
                )
                .finally(Expr::col(Column::StartedAt))
                .into(),
        )
        .col_expr(
            Column::CompletedAt,
            CaseStatement::new()
                .case(Expr::value(terminal), Expr::current_timestamp())
                .finally(Expr::col(Column::CompletedAt))
                .into(),
        )
        .filter(Column::Id.eq(current.id))
        .filter(Column::Generation.eq(current.generation))
        .exec(db)
        .await?;
    if updated.rows_affected != 1 {
        return Err(DbErr::RecordNotFound(format!(
            "no evaluation run with id {} at generation {}",
            current.id, current.generation
        )));
    }
    Ok(())
}

pub(super) async fn insert_result(
    db: &impl ConnectionTrait,
    run: Uuid,
    passed: bool,
    outcome: EvaluationOutcomeCategory,
    code: Option<&str>,
) -> Result<(), DbErr> {
    let digest = sha256(&format!(
        "{run}|{}|{}",
        outcome.to_value(),
        code.unwrap_or("")
    ));
    evaluation_results::Entity::insert(evaluation_results::ActiveModel {
        id: Set(Uuid::new_v4()),
        run_id: Set(run),
        passed: Set(passed),
        outcome_category: Set(outcome),
        summary_digest: Set(digest),
        completed_at: NotSet,
    })
    .on_conflict(
        OnConflict::column(evaluation_results::Column::RunId)
            .do_nothing()
            .to_owned(),
    )
    .try_insert()
    .exec_without_returning(db)
    .await?;
    Ok(())
}

async fn update_case(
    db: &impl ConnectionTrait,
    id: Uuid,
    status: EvaluationRunStatus,
    passed: Option<bool>,
    code: Option<&str>,
) -> Result<(), DbErr> {
    use evaluation_case_runs::Column;
    evaluation_case_runs::Entity::update_many()
        .col_expr(Column::LifecycleStatus, Expr::value(status.as_str()))
        .col_expr(Column::Passed, Expr::value(passed))
        .col_expr(Column::FailureCode, Expr::value(code))
        .col_expr(Column::CompletedAt, Expr::current_timestamp())
        .filter(Column::Id.eq(id))
        .filter(Column::LifecycleStatus.is_in([
            EvaluationLifecycleStatus::Queued,
            EvaluationLifecycleStatus::Running,
        ]))
        .exec(db)
        .await?;
    Ok(())
}

async fn start_case(db: &impl ConnectionTrait, id: Uuid) -> Result<(), DbErr> {
    use evaluation_case_runs::Column;
    evaluation_case_runs::Entity::update_many()
        .col_expr(
            Column::LifecycleStatus,
            Expr::value(EvaluationLifecycleStatus::Running.to_value()),
        )
        .filter(Column::Id.eq(id))
        .filter(Column::LifecycleStatus.eq(EvaluationLifecycleStatus::Queued))
        .exec(db)
        .await?;
    Ok(())
}

/// Frees every event a worker claimed and never finished.
async fn reclaim(db: &impl ConnectionTrait) -> Result<(), DbErr> {
    use evaluation_outbox_events::Column;
    evaluation_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(EvaluationOutboxStatus::Pending.to_value()),
        )
        .col_expr(Column::AvailableAt, Expr::current_timestamp())
        .col_expr(
            Column::ClaimedAt,
            Expr::value(None::<chrono::NaiveDateTime>),
        )
        .col_expr(Column::ClaimedBy, Expr::value(None::<String>))
        .filter(Column::Status.eq(EvaluationOutboxStatus::Processing))
        .filter(Expr::col(Column::ClaimedAt).lt(stale_claim_before()))
        .exec(db)
        .await?;
    Ok(())
}

/// `CURRENT_TIMESTAMP - INTERVAL '30 seconds'`: the bound a claim goes stale at. The interval is
/// a bound text value cast to `interval`, because sea-query has no interval `Value`.
fn stale_claim_before() -> Expr {
    Expr::current_timestamp().sub(seconds(CLAIM_TIMEOUT_SECONDS))
}

/// `CAST('<n> seconds' AS interval)`.
fn seconds(count: i64) -> Expr {
    Expr::value(format!("{count} seconds")).cast_as("interval")
}

/// No `FOR UPDATE`/`SKIP LOCKED`, matching the deployment outbox worker's own
/// established claim pattern (a conditional `UPDATE ... WHERE status = 'PENDING'` is the real
/// claim; 0 rows affected means a lost race, and the caller's at-least-once loop retries).
async fn claim(db: &impl ConnectionTrait, worker: &str) -> Result<Option<Event>, DbErr> {
    use evaluation_outbox_events::Column;
    let Some(row) = evaluation_outbox_events::Entity::find()
        .filter(Column::Status.eq(EvaluationOutboxStatus::Pending))
        .filter(Expr::col(Column::AvailableAt).lte(Expr::current_timestamp()))
        .order_by_asc(Column::AvailableAt)
        .order_by_asc(Column::CreatedAt)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let attempts = row.attempt_count + 1;
    let updated = evaluation_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(EvaluationOutboxStatus::Processing.to_value()),
        )
        .col_expr(Column::ClaimedAt, Expr::current_timestamp())
        .col_expr(Column::ClaimedBy, Expr::value(worker))
        .col_expr(Column::AttemptCount, Expr::value(attempts))
        .filter(Column::Id.eq(row.id))
        .filter(Column::Status.eq(EvaluationOutboxStatus::Pending))
        .exec(db)
        .await?;
    if updated.rows_affected != 1 {
        return Ok(None);
    }
    Ok(Some(Event {
        id: row.id,
        run_id: row.run_id,
        event_type: row.event_type.to_value(),
        case_run_id: row.case_run_id,
        attempts,
    }))
}

async fn delivered_event(db: &impl ConnectionTrait, event: Uuid) -> Result<(), DbErr> {
    use evaluation_outbox_events::Column;
    evaluation_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(EvaluationOutboxStatus::Delivered.to_value()),
        )
        .col_expr(Column::DeliveredAt, Expr::current_timestamp())
        .col_expr(Column::LastError, Expr::value(None::<String>))
        .filter(Column::Id.eq(event))
        .filter(Column::Status.eq(EvaluationOutboxStatus::Processing))
        .exec(db)
        .await?;
    Ok(())
}

async fn prepare_work(
    db: &impl ConnectionTrait,
    event: &Event,
) -> Result<Option<EvaluationWorkItem>, DbErr> {
    let Some(run) = raw_run(db, event.run_id, false).await? else {
        delivered_event(db, event.id).await?;
        return Ok(None);
    };
    let run_status = status_of(&run);
    if !run_status.may_cancel() {
        delivered_event(db, event.id).await?;
        return Ok(None);
    }
    let definition_version = version(db, run.definition_version_id)
        .await?
        .ok_or_else(|| {
            DbErr::RecordNotFound(format!(
                "no evaluation definition version with id {}",
                run.definition_version_id
            ))
        })?;
    let canonical_document = document_text(&definition_version.canonical_document);
    if event.event_type == EvaluationOutboxEventType::Start.to_value() {
        return Ok(Some(EvaluationWorkItem {
            event_id: event.id,
            run_id: event.run_id,
            case_run_id: None,
            event_type: event.event_type.clone(),
            generation: run.generation,
            attempt: event.attempts,
            canonical_document,
            case_ordinal: 0,
            completed_cases: Vec::new(),
            current_lifecycle_status: run_status,
        }));
    }
    if event.event_type == EvaluationOutboxEventType::Case.to_value() {
        let (Some(case_run_id), true) = (
            event.case_run_id,
            run_status == EvaluationRunStatus::Running,
        ) else {
            delivered_event(db, event.id).await?;
            return Ok(None);
        };
        start_case(db, case_run_id).await?;
        let Some(case) = evaluation_case_runs::Entity::find_by_id(case_run_id)
            .one(db)
            .await?
        else {
            delivered_event(db, event.id).await?;
            return Ok(None);
        };
        return Ok(Some(EvaluationWorkItem {
            event_id: event.id,
            run_id: event.run_id,
            case_run_id: Some(case_run_id),
            event_type: event.event_type.clone(),
            generation: run.generation,
            attempt: event.attempts,
            canonical_document,
            case_ordinal: case.ordinal,
            completed_cases: Vec::new(),
            current_lifecycle_status: run_status,
        }));
    }
    if event.event_type != EvaluationOutboxEventType::Finalize.to_value()
        || run_status != EvaluationRunStatus::Running
    {
        delivered_event(db, event.id).await?;
        return Ok(None);
    }
    let completed_cases = evaluation_case_runs::Entity::find()
        .filter(evaluation_case_runs::Column::RunId.eq(run.id))
        .order_by_asc(evaluation_case_runs::Column::Ordinal)
        .select_only()
        .column(evaluation_case_runs::Column::Passed)
        .into_tuple::<Option<bool>>()
        .all(db)
        .await?;
    Ok(Some(EvaluationWorkItem {
        event_id: event.id,
        run_id: event.run_id,
        case_run_id: None,
        event_type: event.event_type.clone(),
        generation: run.generation,
        attempt: event.attempts,
        canonical_document,
        case_ordinal: 0,
        completed_cases,
        current_lifecycle_status: run_status,
    }))
}

pub async fn claim_next(
    db: &DatabaseConnection,
    worker_id: &str,
) -> Result<Option<EvaluationWorkItem>, DbErr> {
    let txn = db.begin().await?;
    reclaim(&txn).await?;
    let event = claim(&txn, worker_id).await?;
    let work = match &event {
        Some(event) => prepare_work(&txn, event).await?,
        None => None,
    };
    txn.commit().await?;
    Ok(work)
}

/// Whether this worker still holds the claim it is about to commit under.
async fn claim_owned(
    db: &impl ConnectionTrait,
    worker: &str,
    event: &Event,
) -> Result<bool, DbErr> {
    use evaluation_outbox_events::Column;
    Ok(evaluation_outbox_events::Entity::find_by_id(event.id)
        .filter(Column::Status.eq(EvaluationOutboxStatus::Processing))
        .filter(Column::ClaimedBy.eq(worker))
        .filter(Column::AttemptCount.eq(event.attempts))
        .one(db)
        .await?
        .is_some())
}

fn event_from_work(work: &EvaluationWorkItem) -> Event {
    Event {
        id: work.event_id,
        run_id: work.run_id,
        event_type: work.event_type.clone(),
        case_run_id: work.case_run_id,
        attempts: work.attempt,
    }
}

pub async fn commit(
    db: &DatabaseConnection,
    worker_id: &str,
    work: &EvaluationWorkItem,
    decision: EvaluationWorkDecision,
) -> Result<(), DbErr> {
    let txn = db.begin().await?;
    let event = event_from_work(work);
    if !claim_owned(&txn, worker_id, &event).await? {
        txn.commit().await?;
        return Ok(());
    }
    let Some(run) = raw_run(&txn, work.run_id, true).await? else {
        delivered_event(&txn, event.id).await?;
        txn.commit().await?;
        return Ok(());
    };
    if run.generation != work.generation {
        delivered_event(&txn, event.id).await?;
        txn.commit().await?;
        return Ok(());
    }
    match decision {
        EvaluationWorkDecision::Start { lifecycle_status } => {
            start(&txn, &run, lifecycle_status).await?
        }
        EvaluationWorkDecision::Case(value) => execute_case(&txn, &event, &run, value).await?,
        EvaluationWorkDecision::Finalize(value) => finalize_run(&txn, &run, value).await?,
    }
    delivered_event(&txn, event.id).await?;
    txn.commit().await?;
    Ok(())
}

async fn start(
    db: &impl ConnectionTrait,
    run: &evaluation_runs::Model,
    next: EvaluationRunStatus,
) -> Result<(), DbErr> {
    if next != EvaluationRunStatus::Running || next == status_of(run) {
        return Ok(());
    }
    update_run(db, run, next, run.generation + 1, None, None, false).await?;
    audit_run(db, run.id, None, "STARTED", "Evaluation run started.").await?;
    let cases = evaluation_case_runs::Entity::find()
        .filter(evaluation_case_runs::Column::RunId.eq(run.id))
        .order_by_asc(evaluation_case_runs::Column::Ordinal)
        .select_only()
        .column(evaluation_case_runs::Column::Id)
        .into_tuple::<Uuid>()
        .all(db)
        .await?;
    for case_id in cases {
        enqueue(db, run.id, EvaluationOutboxEventType::Case, Some(case_id)).await?;
    }
    Ok(())
}

async fn execute_case(
    db: &impl ConnectionTrait,
    event: &Event,
    run: &evaluation_runs::Model,
    decision: EvaluationExecutionDecision,
) -> Result<(), DbErr> {
    let (Some(case_run_id), true) = (
        event.case_run_id,
        status_of(run) == EvaluationRunStatus::Running,
    ) else {
        return Ok(());
    };
    update_case(
        db,
        case_run_id,
        decision.lifecycle_status,
        decision.passed,
        decision.outcome_code.as_deref(),
    )
    .await?;
    if decision.terminal_run {
        terminal_failure(db, run, &decision).await?;
        return Ok(());
    }
    let remaining = evaluation_case_runs::Entity::find()
        .filter(evaluation_case_runs::Column::RunId.eq(run.id))
        .filter(evaluation_case_runs::Column::LifecycleStatus.is_in([
            EvaluationLifecycleStatus::Queued,
            EvaluationLifecycleStatus::Running,
        ]))
        .one(db)
        .await?
        .is_some();
    if !remaining {
        enqueue(db, run.id, EvaluationOutboxEventType::Finalize, None).await?;
    }
    Ok(())
}

/// The finite decimal a metric's `numeric(12, 8)` column holds.
fn metric_value(value: f64) -> Decimal {
    Decimal::from_f64_retain(value)
        .unwrap_or_default()
        .round_dp(8)
}

async fn finalize_run(
    db: &impl ConnectionTrait,
    run: &evaluation_runs::Model,
    decision: EvaluationFinalizationDecision,
) -> Result<(), DbErr> {
    if status_of(run) != EvaluationRunStatus::Running {
        return Ok(());
    }
    for metric in &decision.metrics {
        evaluation_metric_results::Entity::insert(evaluation_metric_results::ActiveModel {
            id: Set(Uuid::new_v4()),
            run_id: Set(run.id),
            metric_code: Set(metric.code.clone()),
            value: Set(metric_value(metric.value)),
            threshold: Set(metric_value(metric.threshold)),
            passed: Set(metric.passed),
        })
        .on_conflict(
            OnConflict::columns([
                evaluation_metric_results::Column::RunId,
                evaluation_metric_results::Column::MetricCode,
            ])
            .do_nothing()
            .to_owned(),
        )
        .try_insert()
        .exec_without_returning(db)
        .await?;
    }
    evaluation_artifact_metadata::Entity::insert(evaluation_artifact_metadata::ActiveModel {
        id: Set(Uuid::new_v4()),
        run_id: Set(run.id),
        artifact_kind: Set("LOCAL_SUMMARY".to_string()),
        content_digest: Set(sha256(&format!("{}|summary", run.id))),
        media_type: Set("application/json".to_string()),
        byte_length: Set(0),
    })
    .on_conflict(
        OnConflict::columns([
            evaluation_artifact_metadata::Column::RunId,
            evaluation_artifact_metadata::Column::ArtifactKind,
            evaluation_artifact_metadata::Column::ContentDigest,
        ])
        .do_nothing()
        .to_owned(),
    )
    .try_insert()
    .exec_without_returning(db)
    .await?;
    let outcome = outcome_category(&decision.outcome_category);
    insert_result(
        db,
        run.id,
        decision.passed,
        outcome,
        Some(decision.summary_digest_material.as_str()),
    )
    .await?;
    update_run(
        db,
        run,
        decision.lifecycle_status,
        run.generation + 1,
        Some(outcome),
        None,
        true,
    )
    .await?;
    if decision.passed {
        append_evidence(db, run.id).await?;
    }
    audit_run(db, run.id, None, "COMPLETED", "Evaluation run completed.").await?;
    Ok(())
}

/// The stored outcome category. The column's `CHECK` admits only these values, and every caller
/// supplies one the domain named, so an unrecognized value is schema drift.
fn outcome_category(value: &str) -> EvaluationOutcomeCategory {
    EvaluationOutcomeCategory::try_from_value(&value.to_string())
        .unwrap_or_else(|error| panic!("{error}"))
}

async fn terminal_failure(
    db: &impl ConnectionTrait,
    run: &evaluation_runs::Model,
    decision: &EvaluationExecutionDecision,
) -> Result<(), DbErr> {
    let outcome = decision
        .outcome_category
        .as_deref()
        .map(outcome_category)
        .unwrap_or(EvaluationOutcomeCategory::RunnerFailed);
    insert_result(db, run.id, false, outcome, decision.outcome_code.as_deref()).await?;
    update_run(
        db,
        run,
        decision.lifecycle_status,
        run.generation + 1,
        decision.outcome_category.as_deref().map(outcome_category),
        decision.outcome_code.as_deref(),
        true,
    )
    .await?;
    cancel_open_cases(db, run.id).await?;
    cancel_open_events(db, run.id).await?;
    audit_run(db, run.id, None, "FAILED", "Evaluation run failed.").await?;
    Ok(())
}

/// The facts `append_evidence` compares before it writes a deployment's evaluation evidence.
#[derive(FromQueryResult)]
struct EvidenceCandidate {
    lifecycle_status: EvaluationLifecycleStatus,
    outcome_category: Option<EvaluationOutcomeCategory>,
    target_kind: EvaluationTargetKind,
    target_id: Uuid,
    deployment_id: Uuid,
    agent_version_id: Uuid,
    environment_definition_version_id: Uuid,
    target_digest: Option<String>,
    plan_digest: Option<String>,
    package_digest: Option<String>,
    binding_digest: Option<String>,
    deployment_agent_version_id: Uuid,
    deployment_environment_definition_version_id: Option<Uuid>,
    policy_agent_version_id: Option<Uuid>,
    policy_environment_definition_version_id: Option<Uuid>,
    policy_target_digest: Option<String>,
    policy_plan_digest: Option<String>,
    policy_package_digest: Option<String>,
    policy_binding_digest: Option<String>,
    evaluation_requirement_expires_at: Option<chrono::DateTime<chrono::FixedOffset>>,
}

/// The cross-domain handoff into the deployment/approval domain on a PASSED terminal run whose
/// target was a `DEPLOYMENT`. Returns the disposition code only for parity with the original's
/// return value; `finalize_run` calls this for its side effect.
async fn append_evidence(db: &impl ConnectionTrait, run: Uuid) -> Result<String, DbErr> {
    let deployment_id = evaluation_target_snapshots::Entity::find_by_id(run)
        .one(db)
        .await?
        .and_then(|snapshot| snapshot.deployment_id);
    if deployment_id.is_none() {
        return Ok("NOT_ELIGIBLE".to_string());
    }
    // One statement, locking the deployment row only (`FOR UPDATE OF deployment`), exactly as the
    // deleted join did.
    let candidate_select = evaluation_runs::Entity::find_by_id(run)
        .join(
            JoinType::InnerJoin,
            evaluation_runs::Relation::EvaluationResults.def(),
        )
        .join(
            JoinType::InnerJoin,
            evaluation_runs::Relation::EvaluationTargetSnapshots.def(),
        )
        .join(
            JoinType::InnerJoin,
            evaluation_target_snapshots::Relation::Deployments.def(),
        )
        .join(
            JoinType::InnerJoin,
            deployments::Relation::DeploymentPolicySnapshots.def(),
        )
        .filter(evaluation_results::Column::Passed.eq(true))
        .filter(evaluation_results::Column::OutcomeCategory.eq(EvaluationOutcomeCategory::Passed))
        .select_only()
        .column(evaluation_runs::Column::LifecycleStatus)
        .column(evaluation_runs::Column::OutcomeCategory)
        .column(evaluation_runs::Column::TargetKind)
        .column(evaluation_runs::Column::TargetId)
        .column_as(
            evaluation_target_snapshots::Column::DeploymentId,
            "deployment_id",
        )
        .column(evaluation_target_snapshots::Column::AgentVersionId)
        .column(evaluation_target_snapshots::Column::EnvironmentDefinitionVersionId)
        .column(evaluation_target_snapshots::Column::TargetDigest)
        .column(evaluation_target_snapshots::Column::PlanDigest)
        .column(evaluation_target_snapshots::Column::PackageDigest)
        .column(evaluation_target_snapshots::Column::BindingDigest)
        .column_as(
            deployments::Column::AgentVersionId,
            "deployment_agent_version_id",
        )
        .column_as(
            deployments::Column::EnvironmentDefinitionVersionId,
            "deployment_environment_definition_version_id",
        )
        .column_as(
            deployment_policy_snapshots::Column::AgentVersionId,
            "policy_agent_version_id",
        )
        .column_as(
            deployment_policy_snapshots::Column::EnvironmentDefinitionVersionId,
            "policy_environment_definition_version_id",
        )
        .column_as(
            deployment_policy_snapshots::Column::TargetDigest,
            "policy_target_digest",
        )
        .column_as(
            deployment_policy_snapshots::Column::PlanDigest,
            "policy_plan_digest",
        )
        .column_as(
            deployment_policy_snapshots::Column::PackageDigest,
            "policy_package_digest",
        )
        .column_as(
            deployment_policy_snapshots::Column::BindingDigest,
            "policy_binding_digest",
        )
        .column(deployment_policy_snapshots::Column::EvaluationRequirementExpiresAt);
    let Some(candidate) = for_update_of(candidate_select, [deployments::Entity.into_table_ref()])
        .into_model::<EvidenceCandidate>()
        .one(db)
        .await?
    else {
        return Ok("NOT_ELIGIBLE".to_string());
    };

    if candidate.lifecycle_status != EvaluationLifecycleStatus::Completed
        || candidate.outcome_category != Some(EvaluationOutcomeCategory::Passed)
        || candidate.target_kind != EvaluationTargetKind::Deployment
        || candidate.target_id != candidate.deployment_id
    {
        return Ok("NOT_ELIGIBLE".to_string());
    }
    if candidate.agent_version_id != candidate.deployment_agent_version_id
        || Some(candidate.environment_definition_version_id)
            != candidate.deployment_environment_definition_version_id
        || Some(candidate.agent_version_id) != candidate.policy_agent_version_id
        || Some(candidate.environment_definition_version_id)
            != candidate.policy_environment_definition_version_id
        || candidate.target_digest != candidate.policy_target_digest
        || candidate.plan_digest != candidate.policy_plan_digest
        || candidate.package_digest != candidate.policy_package_digest
        || candidate.binding_digest != candidate.policy_binding_digest
    {
        return Ok("BINDING_MISMATCH".to_string());
    }
    let deployment_id = candidate.deployment_id;
    let pending_requirement = deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .filter(
            deployment_approval_requirements::Column::Status
                .eq(crate::entity::enums::ApprovalRequirementStatus::Pending),
        )
        .lock_exclusive()
        .one(db)
        .await?
        .is_some();
    let waiting = if pending_requirement {
        crate::deployment::waiting_for_evaluation(db, deployment_id).await?
    } else {
        false
    };
    if !pending_requirement || !waiting {
        return Ok("NOT_PENDING".to_string());
    }
    let already = deployment_evidence_snapshots::Entity::find()
        .filter(deployment_evidence_snapshots::Column::DeploymentId.eq(deployment_id))
        .filter(
            deployment_evidence_snapshots::Column::EvidenceKind
                .eq(DeploymentEvidenceKind::EvaluationPassed),
        )
        .one(db)
        .await?
        .is_some();
    if already {
        return Ok("ALREADY_APPENDED".to_string());
    }
    deployment_evidence_snapshots::Entity::insert(deployment_evidence_snapshots::ActiveModel {
        id: Set(Uuid::new_v4()),
        deployment_id: Set(deployment_id),
        evidence_kind: Set(DeploymentEvidenceKind::EvaluationPassed),
        evidence_digest: Set(sha256(&format!(
            "{}|EVALUATION_PASSED",
            candidate.binding_digest.as_deref().unwrap_or("")
        ))),
        expires_at: Set(candidate.evaluation_requirement_expires_at),
        created_at: NotSet,
        agent_version_id: Set(Some(candidate.agent_version_id)),
        environment_definition_version_id: Set(Some(candidate.environment_definition_version_id)),
        target_digest: Set(candidate.target_digest),
        plan_digest: Set(candidate.plan_digest),
        package_digest: Set(candidate.package_digest),
        binding_digest: Set(candidate.binding_digest),
        source_evaluation_run_id: Set(Some(run)),
    })
    .exec_without_returning(db)
    .await?;
    crate::deployment::touch_projection(db, deployment_id).await?;
    crate::deployment::automatic_approval_handoff(db, deployment_id).await?;
    Ok("APPENDED".to_string())
}

pub async fn idle(db: &DatabaseConnection, worker_id: &str) -> Result<(), DbErr> {
    heartbeat(db, worker_id, WorkerHeartbeatState::Ready, None, 0, true).await
}

pub async fn delivered(db: &DatabaseConnection, worker_id: &str) -> Result<(), DbErr> {
    heartbeat(db, worker_id, WorkerHeartbeatState::Ready, None, 1, true).await
}

pub async fn failed(
    db: &DatabaseConnection,
    worker_id: &str,
    work: &EvaluationWorkItem,
    decision: &EvaluationExecutionDecision,
) -> Result<(), DbErr> {
    let event = event_from_work(work);
    let retry_result = retry_or_dead_letter(db, worker_id, &event, decision).await;
    heartbeat(
        db,
        worker_id,
        WorkerHeartbeatState::Degraded,
        decision.outcome_code.as_deref(),
        0,
        false,
    )
    .await?;
    retry_result
}

pub async fn claim_failed(db: &DatabaseConnection, worker_id: &str) -> Result<(), DbErr> {
    heartbeat(
        db,
        worker_id,
        WorkerHeartbeatState::Degraded,
        Some("RUNNER_FAILED"),
        0,
        false,
    )
    .await
}

async fn retry_or_dead_letter(
    db: &DatabaseConnection,
    worker: &str,
    event: &Event,
    decision: &EvaluationExecutionDecision,
) -> Result<(), DbErr> {
    use evaluation_outbox_events::Column;
    let txn = db.begin().await?;
    /// The claim this worker holds, which every retry write guards on.
    fn owned(event: &Event, worker: &str) -> sea_orm::Condition {
        sea_orm::Condition::all()
            .add(Column::Id.eq(event.id))
            .add(Column::Status.eq(EvaluationOutboxStatus::Processing))
            .add(Column::ClaimedBy.eq(worker))
            .add(Column::AttemptCount.eq(event.attempts))
    }
    if event.attempts < DELIVERY_LIMIT {
        evaluation_outbox_events::Entity::update_many()
            .col_expr(
                Column::Status,
                Expr::value(EvaluationOutboxStatus::Pending.to_value()),
            )
            .col_expr(
                Column::AvailableAt,
                Expr::current_timestamp().add(seconds(i64::from(event.attempts))),
            )
            .col_expr(
                Column::ClaimedAt,
                Expr::value(None::<chrono::NaiveDateTime>),
            )
            .col_expr(Column::ClaimedBy, Expr::value(None::<String>))
            .col_expr(
                Column::LastError,
                Expr::value(decision.outcome_code.clone()),
            )
            .filter(owned(event, worker))
            .exec(&txn)
            .await?;
        txn.commit().await?;
        return Ok(());
    }
    let run = raw_run(&txn, event.run_id, true).await?;
    let claimed = evaluation_outbox_events::Entity::update_many()
        .col_expr(
            Column::Status,
            Expr::value(EvaluationOutboxStatus::DeadLetter.to_value()),
        )
        .col_expr(
            Column::LastError,
            Expr::value(decision.outcome_code.clone()),
        )
        .filter(owned(event, worker))
        .exec(&txn)
        .await?;
    if claimed.rows_affected == 1 {
        if let Some(run) = &run {
            if status_of(run).may_cancel() {
                terminal_failure(&txn, run, decision).await?;
            }
        }
    }
    txn.commit().await?;
    Ok(())
}

/// An anti-flap upsert: a `READY` heartbeat cannot silently clear an existing `DEGRADED` row
/// unless `recovery` is true, which only `idle` and `delivered` pass.
async fn heartbeat(
    db: &DatabaseConnection,
    worker: &str,
    status: WorkerHeartbeatState,
    code: Option<&str>,
    deliveries: i32,
    recovery: bool,
) -> Result<(), DbErr> {
    use evaluation_worker_heartbeats::Column;
    // The queued events, bounded: at most 101 rows are read, and the count and the oldest are
    // taken from them. This is a heartbeat metric read outside any transaction, exactly as the
    // deleted aggregate over the same bounded subquery was.
    let queued = evaluation_outbox_events::Entity::find()
        .filter(evaluation_outbox_events::Column::Status.is_in([
            EvaluationOutboxStatus::Pending,
            EvaluationOutboxStatus::Processing,
        ]))
        .order_by_asc(evaluation_outbox_events::Column::AvailableAt)
        .limit(HEARTBEAT_QUEUE_LIMIT)
        .select_only()
        .column(evaluation_outbox_events::Column::AvailableAt)
        .into_tuple::<chrono::DateTime<chrono::FixedOffset>>()
        .all(db)
        .await?;
    let truncated = queued.len() as u64 > HEARTBEAT_QUEUE_LIMIT - 1;
    let pending = queued.len().min(100) as i32;
    let oldest = queued.iter().min().copied();

    let excluded = Alias::new("excluded");
    // `EXCLUDED.status = 'READY' AND evaluation_worker_heartbeats.status = 'DEGRADED' AND NOT
    // <recovery>`: the anti-flap condition both CASE arms share.
    let flapping = || {
        sea_orm::Condition::all()
            .add(
                Expr::col((excluded.clone(), Column::Status))
                    .eq(WorkerHeartbeatState::Ready.to_value()),
            )
            .add(
                Expr::col((evaluation_worker_heartbeats::Entity, Column::Status))
                    .eq(WorkerHeartbeatState::Degraded.to_value()),
            )
            .add(Expr::value(recovery).not())
    };
    evaluation_worker_heartbeats::Entity::insert(evaluation_worker_heartbeats::ActiveModel {
        worker_id: Set(worker.to_string()),
        observed_at: NotSet,
        status: Set(status),
        last_failure_code: Set(code.map(str::to_string)),
        last_delivery_count: Set(deliveries),
        pending_events: Set(pending),
        oldest_pending_at: Set(oldest),
        pending_truncated: Set(truncated),
    })
    .on_conflict(
        OnConflict::column(Column::WorkerId)
            .values([
                (Column::ObservedAt, Expr::current_timestamp()),
                (
                    Column::Status,
                    CaseStatement::new()
                        .case(
                            flapping(),
                            Expr::value(WorkerHeartbeatState::Degraded.to_value()),
                        )
                        .finally(Expr::col((excluded.clone(), Column::Status)))
                        .into(),
                ),
                (
                    Column::LastFailureCode,
                    CaseStatement::new()
                        .case(
                            flapping(),
                            Expr::col((
                                evaluation_worker_heartbeats::Entity,
                                Column::LastFailureCode,
                            )),
                        )
                        .finally(Expr::col((excluded.clone(), Column::LastFailureCode)))
                        .into(),
                ),
                (
                    Column::LastDeliveryCount,
                    Expr::col((excluded.clone(), Column::LastDeliveryCount)),
                ),
                (
                    Column::PendingEvents,
                    Expr::col((excluded.clone(), Column::PendingEvents)),
                ),
                (
                    Column::OldestPendingAt,
                    Expr::col((excluded.clone(), Column::OldestPendingAt)),
                ),
                (
                    Column::PendingTruncated,
                    Expr::col((excluded.clone(), Column::PendingTruncated)),
                ),
            ])
            .to_owned(),
    )
    .exec_without_returning(db)
    .await?;
    Ok(())
}
