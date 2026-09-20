//! Ports the worker-delivery methods `PostgresEvaluationWorkStore` delegates
//! to (`claimNext`/`commit`/`idle`/`delivered`/`failed`/`claimFailed`) and
//! their private helpers: `claim`/`prepareWork`/`start`/`executeCase`/
//! `finalizeRun`/`appendEvidence`/`reclaim`/`retryOrDeadLetter`/`heartbeat`.

use hive_application::configuration::canonical::digest as sha256;
use hive_application::evaluation::{
    EvaluationExecutionDecision, EvaluationFinalizationDecision, EvaluationWorkDecision,
    EvaluationWorkItem,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, TransactionTrait};
use uuid::Uuid;

use super::mutations::{audit_run, enqueue};
use super::queries::{raw_run, version};
use super::rows::{self, Event, RawRun};
use hive_application::evaluation::EvaluationRunStatus;

const DELIVERY_LIMIT: i32 = 4;

/// Ports `updateRun`'s optimistic transition. The terminal-state assertion Java takes before the
/// `UPDATE` is not re-derived here (every call site already only reaches this after its own
/// `EvaluationRunStateMachine`- or status-list-driven check decided the transition is legal, the
/// same "share one source of truth, don't duplicate it" reasoning `transition_requirement` in the
/// deployment domain already documents) — the `WHERE id = $.. AND generation = $..` match is the
/// real optimistic guard, same conditional-UPDATE-claim pattern used throughout this rewrite.
pub(super) async fn update_run(
    db: &impl ConnectionTrait,
    current: &RawRun,
    status: EvaluationRunStatus,
    generation: i64,
    outcome: Option<&str>,
    code: Option<&str>,
    terminal: bool,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE evaluation_runs SET lifecycle_status = $1, generation = $2, outcome_category = $3, outcome_code = $4, \
             started_at = CASE WHEN $1 = 'RUNNING' THEN COALESCE(started_at, CURRENT_TIMESTAMP) ELSE started_at END, \
             completed_at = CASE WHEN $5 THEN CURRENT_TIMESTAMP ELSE completed_at END \
         WHERE id = $6 AND generation = $7",
        [
            status.as_str().into(),
            generation.into(),
            outcome.into(),
            code.into(),
            terminal.into(),
            current.id.into(),
            current.generation.into(),
        ],
    );
    let updated = db.execute_raw(statement).await?;
    if updated.rows_affected() != 1 {
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
    outcome: &str,
    code: Option<&str>,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO evaluation_results (id, run_id, passed, outcome_category, summary_digest) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (run_id) DO NOTHING",
        [
            Uuid::new_v4().into(),
            run.into(),
            passed.into(),
            outcome.into(),
            sha256(&format!("{run}|{outcome}|{}", code.unwrap_or(""))).into(),
        ],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

async fn update_case(
    db: &impl ConnectionTrait,
    id: Uuid,
    status: EvaluationRunStatus,
    passed: Option<bool>,
    code: Option<&str>,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE evaluation_case_runs SET lifecycle_status = $1, passed = $2, failure_code = $3, completed_at = CURRENT_TIMESTAMP \
         WHERE id = $4 AND lifecycle_status IN ('QUEUED', 'RUNNING')",
        [status.as_str().into(), passed.into(), code.into(), id.into()],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

async fn start_case(db: &impl ConnectionTrait, id: Uuid) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE evaluation_case_runs SET lifecycle_status = 'RUNNING' WHERE id = $1 AND lifecycle_status = 'QUEUED'",
        [id.into()],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

async fn reclaim(db: &impl ConnectionTrait) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE evaluation_outbox_events SET status = 'PENDING', available_at = CURRENT_TIMESTAMP, claimed_at = NULL, claimed_by = NULL \
         WHERE status = 'PROCESSING' AND claimed_at < CURRENT_TIMESTAMP - INTERVAL '30 seconds'",
        [],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

/// Ports `claim()`: no `FOR UPDATE`/`SKIP LOCKED`, mirroring the deployment outbox worker's own
/// established claim pattern (a conditional `UPDATE ... WHERE status = 'PENDING'` is the real claim;
/// 0 rows affected means a lost race, and the caller's at-least-once loop retries next tick).
async fn claim(db: &impl ConnectionTrait, worker: &str) -> Result<Option<Event>, DbErr> {
    let select_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, run_id, event_type, case_run_id, attempt_count FROM evaluation_outbox_events \
         WHERE status = 'PENDING' AND available_at <= CURRENT_TIMESTAMP ORDER BY available_at, created_at LIMIT 1",
        [],
    );
    let Some(row) = db.query_one_raw(select_statement).await? else {
        return Ok(None);
    };
    let id: Uuid = row.try_get_by("id")?;
    let attempts: i32 = row.try_get_by::<i32, _>("attempt_count")? + 1;
    let update_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE evaluation_outbox_events SET status = 'PROCESSING', claimed_at = CURRENT_TIMESTAMP, claimed_by = $1, attempt_count = $2 \
         WHERE id = $3 AND status = 'PENDING'",
        [worker.into(), attempts.into(), id.into()],
    );
    let updated = db.execute_raw(update_statement).await?;
    if updated.rows_affected() != 1 {
        return Ok(None);
    }
    Ok(Some(Event {
        id,
        run_id: row.try_get_by("run_id")?,
        event_type: row.try_get_by("event_type")?,
        case_run_id: row.try_get_by("case_run_id")?,
        attempts,
    }))
}

async fn delivered_event(db: &impl ConnectionTrait, event: Uuid) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE evaluation_outbox_events SET status = 'DELIVERED', delivered_at = CURRENT_TIMESTAMP, last_error = NULL WHERE id = $1 AND status = 'PROCESSING'",
        [event.into()],
    );
    db.execute_raw(statement).await?;
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
    if !run.status.may_cancel() {
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
    if event.event_type == "START" {
        return Ok(Some(EvaluationWorkItem {
            event_id: event.id,
            run_id: event.run_id,
            case_run_id: None,
            event_type: event.event_type.clone(),
            generation: run.generation,
            attempt: event.attempts,
            canonical_document: definition_version.canonical_document,
            case_ordinal: 0,
            completed_cases: Vec::new(),
            current_lifecycle_status: run.status,
        }));
    }
    if event.event_type == "CASE" {
        let (Some(case_run_id), true) = (
            event.case_run_id,
            run.status == EvaluationRunStatus::Running,
        ) else {
            delivered_event(db, event.id).await?;
            return Ok(None);
        };
        start_case(db, case_run_id).await?;
        let ordinal_statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT ordinal FROM evaluation_case_runs WHERE id = $1",
            [case_run_id.into()],
        );
        let Some(ordinal_row) = db.query_one_raw(ordinal_statement).await? else {
            delivered_event(db, event.id).await?;
            return Ok(None);
        };
        let ordinal: i32 = ordinal_row.try_get_by("ordinal")?;
        return Ok(Some(EvaluationWorkItem {
            event_id: event.id,
            run_id: event.run_id,
            case_run_id: Some(case_run_id),
            event_type: event.event_type.clone(),
            generation: run.generation,
            attempt: event.attempts,
            canonical_document: definition_version.canonical_document,
            case_ordinal: ordinal,
            completed_cases: Vec::new(),
            current_lifecycle_status: run.status,
        }));
    }
    if event.event_type != "FINALIZE" || run.status != EvaluationRunStatus::Running {
        delivered_event(db, event.id).await?;
        return Ok(None);
    }
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT passed FROM evaluation_case_runs WHERE run_id = $1 ORDER BY ordinal",
        [run.id.into()],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut completed_cases = Vec::with_capacity(rows_found.len());
    for row in rows_found {
        completed_cases.push(row.try_get_by::<Option<bool>, _>("passed")?);
    }
    Ok(Some(EvaluationWorkItem {
        event_id: event.id,
        run_id: event.run_id,
        case_run_id: None,
        event_type: event.event_type.clone(),
        generation: run.generation,
        attempt: event.attempts,
        canonical_document: definition_version.canonical_document,
        case_ordinal: 0,
        completed_cases,
        current_lifecycle_status: run.status,
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

async fn claim_owned(
    db: &impl ConnectionTrait,
    worker: &str,
    event: &Event,
) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT EXISTS (SELECT 1 FROM evaluation_outbox_events WHERE id = $1 AND status = 'PROCESSING' AND claimed_by = $2 AND attempt_count = $3) AS owned",
        [event.id.into(), worker.into(), event.attempts.into()],
    );
    db.query_one_raw(statement)
        .await?
        .expect("EXISTS(...) always returns exactly one row")
        .try_get_by("owned")
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
    run: &RawRun,
    next: EvaluationRunStatus,
) -> Result<(), DbErr> {
    if next != EvaluationRunStatus::Running || next == run.status {
        return Ok(());
    }
    update_run(db, run, next, run.generation + 1, None, None, false).await?;
    audit_run(db, run.id, None, "STARTED", "Evaluation run started.").await?;
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id FROM evaluation_case_runs WHERE run_id = $1 ORDER BY ordinal",
        [run.id.into()],
    );
    let rows_found = db.query_all_raw(statement).await?;
    for row in rows_found {
        let case_id: Uuid = row.try_get_by("id")?;
        enqueue(db, run.id, "CASE", Some(case_id)).await?;
    }
    Ok(())
}

async fn execute_case(
    db: &impl ConnectionTrait,
    event: &Event,
    run: &RawRun,
    decision: EvaluationExecutionDecision,
) -> Result<(), DbErr> {
    let (Some(case_run_id), true) = (
        event.case_run_id,
        run.status == EvaluationRunStatus::Running,
    ) else {
        return Ok(());
    };
    if decision.terminal_run {
        update_case(
            db,
            case_run_id,
            decision.lifecycle_status,
            decision.passed,
            decision.outcome_code.as_deref(),
        )
        .await?;
        terminal_failure(db, run, &decision).await?;
        return Ok(());
    }
    update_case(
        db,
        case_run_id,
        decision.lifecycle_status,
        decision.passed,
        decision.outcome_code.as_deref(),
    )
    .await?;
    let remaining_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT EXISTS (SELECT 1 FROM evaluation_case_runs WHERE run_id = $1 AND lifecycle_status IN ('QUEUED', 'RUNNING')) AS remaining",
        [run.id.into()],
    );
    let remaining: bool = db
        .query_one_raw(remaining_statement)
        .await?
        .expect("EXISTS(...) always returns exactly one row")
        .try_get_by("remaining")?;
    if !remaining {
        enqueue(db, run.id, "FINALIZE", None).await?;
    }
    Ok(())
}

async fn finalize_run(
    db: &impl ConnectionTrait,
    run: &RawRun,
    decision: EvaluationFinalizationDecision,
) -> Result<(), DbErr> {
    if run.status != EvaluationRunStatus::Running {
        return Ok(());
    }
    for metric in &decision.metrics {
        let statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO evaluation_metric_results (id, run_id, metric_code, value, threshold, passed) VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (run_id, metric_code) DO NOTHING",
            [
                Uuid::new_v4().into(),
                run.id.into(),
                metric.code.clone().into(),
                metric.value.into(),
                metric.threshold.into(),
                metric.passed.into(),
            ],
        );
        db.execute_raw(statement).await?;
    }
    let artifact_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO evaluation_artifact_metadata (id, run_id, artifact_kind, content_digest, media_type, byte_length) VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT DO NOTHING",
        [
            Uuid::new_v4().into(),
            run.id.into(),
            "LOCAL_SUMMARY".into(),
            sha256(&format!("{}|summary", run.id)).into(),
            "application/json".into(),
            0i64.into(),
        ],
    );
    db.execute_raw(artifact_statement).await?;
    insert_result(
        db,
        run.id,
        decision.passed,
        &decision.outcome_category,
        Some(decision.summary_digest_material.as_str()),
    )
    .await?;
    update_run(
        db,
        run,
        decision.lifecycle_status,
        run.generation + 1,
        Some(&decision.outcome_category),
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

async fn terminal_failure(
    db: &impl ConnectionTrait,
    run: &RawRun,
    decision: &EvaluationExecutionDecision,
) -> Result<(), DbErr> {
    insert_result(
        db,
        run.id,
        false,
        decision.outcome_category.as_deref().unwrap_or(""),
        decision.outcome_code.as_deref(),
    )
    .await?;
    update_run(
        db,
        run,
        decision.lifecycle_status,
        run.generation + 1,
        decision.outcome_category.as_deref(),
        decision.outcome_code.as_deref(),
        true,
    )
    .await?;
    let case_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE evaluation_case_runs SET lifecycle_status = 'CANCELED', completed_at = CURRENT_TIMESTAMP WHERE run_id = $1 AND lifecycle_status IN ('QUEUED', 'RUNNING')",
        [run.id.into()],
    );
    db.execute_raw(case_statement).await?;
    let outbox_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE evaluation_outbox_events SET status = 'CANCELED' WHERE run_id = $1 AND status IN ('PENDING', 'PROCESSING')",
        [run.id.into()],
    );
    db.execute_raw(outbox_statement).await?;
    audit_run(db, run.id, None, "FAILED", "Evaluation run failed.").await?;
    Ok(())
}

/// Ports `appendEvidence`: the cross-domain handoff into the already-ported deployment/approval
/// domain on a PASSED terminal run whose target was a `DEPLOYMENT`. Returns the disposition code
/// only for parity with Java's return value; no caller here consumes it (matching `finalizeRun`,
/// which calls this only for its side effect).
async fn append_evidence(db: &impl ConnectionTrait, run: Uuid) -> Result<String, DbErr> {
    let snapshot_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT deployment_id FROM evaluation_target_snapshots WHERE run_id = $1",
        [run.into()],
    );
    let deployment_id: Option<Uuid> = db
        .query_one_raw(snapshot_statement)
        .await?
        .map(|row| row.try_get_by("deployment_id"))
        .transpose()?
        .flatten();
    let Some(_deployment_id) = deployment_id else {
        return Ok("NOT_ELIGIBLE".to_string());
    };
    let candidate_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT run.lifecycle_status, run.outcome_category, run.target_kind, run.target_id, \
             snapshot.deployment_id, snapshot.agent_version_id, snapshot.environment_definition_version_id, \
             snapshot.target_digest, snapshot.plan_digest, snapshot.package_digest, snapshot.binding_digest, \
             deployment.agent_version_id AS deployment_agent_version_id, deployment.environment_definition_version_id AS deployment_environment_definition_version_id, \
             policy.agent_version_id AS policy_agent_version_id, policy.environment_definition_version_id AS policy_environment_definition_version_id, \
             policy.target_digest AS policy_target_digest, policy.plan_digest AS policy_plan_digest, policy.package_digest AS policy_package_digest, policy.binding_digest AS policy_binding_digest, \
             policy.evaluation_requirement_expires_at \
         FROM evaluation_runs run \
           JOIN evaluation_results result ON result.run_id = run.id AND result.passed = TRUE AND result.outcome_category = 'PASSED' \
           JOIN evaluation_target_snapshots snapshot ON snapshot.run_id = run.id \
           JOIN deployments deployment ON deployment.id = snapshot.deployment_id \
           JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id \
         WHERE run.id = $1 \
         FOR UPDATE OF deployment",
        [run.into()],
    );
    let Some(candidate) = db.query_one_raw(candidate_statement).await? else {
        return Ok("NOT_ELIGIBLE".to_string());
    };
    let lifecycle_status = rows::run_status(candidate.try_get_by("lifecycle_status")?);
    let outcome_category: Option<String> = candidate.try_get_by("outcome_category")?;
    let target_kind: String = candidate.try_get_by("target_kind")?;
    let target_id: Uuid = candidate.try_get_by("target_id")?;
    let deployment_id: Uuid = candidate.try_get_by("deployment_id")?;
    let agent_version_id: Uuid = candidate.try_get_by("agent_version_id")?;
    let environment_definition_version_id: Uuid =
        candidate.try_get_by("environment_definition_version_id")?;
    let target_digest: Option<String> = candidate.try_get_by("target_digest")?;
    let plan_digest: Option<String> = candidate.try_get_by("plan_digest")?;
    let package_digest: Option<String> = candidate.try_get_by("package_digest")?;
    let binding_digest: Option<String> = candidate.try_get_by("binding_digest")?;
    let deployment_agent_version_id: Uuid = candidate.try_get_by("deployment_agent_version_id")?;
    let deployment_environment_definition_version_id: Uuid =
        candidate.try_get_by("deployment_environment_definition_version_id")?;
    let policy_agent_version_id: Uuid = candidate.try_get_by("policy_agent_version_id")?;
    let policy_environment_definition_version_id: Uuid =
        candidate.try_get_by("policy_environment_definition_version_id")?;
    let policy_target_digest: Option<String> = candidate.try_get_by("policy_target_digest")?;
    let policy_plan_digest: Option<String> = candidate.try_get_by("policy_plan_digest")?;
    let policy_package_digest: Option<String> = candidate.try_get_by("policy_package_digest")?;
    let policy_binding_digest: Option<String> = candidate.try_get_by("policy_binding_digest")?;
    let evaluation_requirement_expires_at: chrono::DateTime<chrono::Utc> =
        candidate.try_get_by("evaluation_requirement_expires_at")?;

    if lifecycle_status != EvaluationRunStatus::Completed
        || outcome_category.as_deref() != Some("PASSED")
        || target_kind != "DEPLOYMENT"
        || target_id != deployment_id
    {
        return Ok("NOT_ELIGIBLE".to_string());
    }
    if agent_version_id != deployment_agent_version_id
        || environment_definition_version_id != deployment_environment_definition_version_id
        || agent_version_id != policy_agent_version_id
        || environment_definition_version_id != policy_environment_definition_version_id
        || target_digest != policy_target_digest
        || plan_digest != policy_plan_digest
        || package_digest != policy_package_digest
        || binding_digest != policy_binding_digest
    {
        return Ok("BINDING_MISMATCH".to_string());
    }
    let pending_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT requirement.id FROM deployment_approval_requirements requirement \
         WHERE requirement.deployment_id = $1 AND requirement.status = 'PENDING' FOR UPDATE",
        [deployment_id.into()],
    );
    let pending_requirement = db.query_one_raw(pending_statement).await?.is_some();
    let waiting = if pending_requirement {
        crate::deployment::waiting_for_evaluation(db, deployment_id).await?
    } else {
        false
    };
    if !pending_requirement || !waiting {
        return Ok("NOT_PENDING".to_string());
    }
    let already_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT EXISTS (SELECT 1 FROM deployment_evidence_snapshots WHERE deployment_id = $1 AND evidence_kind = 'EVALUATION_PASSED') AS already",
        [deployment_id.into()],
    );
    let already: bool = db
        .query_one_raw(already_statement)
        .await?
        .expect("EXISTS(...) always returns exactly one row")
        .try_get_by("already")?;
    if already {
        return Ok("ALREADY_APPENDED".to_string());
    }
    let insert_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_evidence_snapshots (id, deployment_id, evidence_kind, evidence_digest, expires_at, \
             agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest, \
             binding_digest, source_evaluation_run_id) \
         VALUES ($1, $2, 'EVALUATION_PASSED', $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        [
            Uuid::new_v4().into(),
            deployment_id.into(),
            sha256(&format!(
                "{}|EVALUATION_PASSED",
                binding_digest.as_deref().unwrap_or("")
            ))
            .into(),
            evaluation_requirement_expires_at.into(),
            agent_version_id.into(),
            environment_definition_version_id.into(),
            target_digest.into(),
            plan_digest.into(),
            package_digest.into(),
            binding_digest.into(),
            run.into(),
        ],
    );
    db.execute_raw(insert_statement).await?;
    crate::deployment::touch_projection(db, deployment_id).await?;
    crate::deployment::automatic_approval_handoff(db, deployment_id).await?;
    Ok("APPENDED".to_string())
}

pub async fn idle(db: &DatabaseConnection, worker_id: &str) -> Result<(), DbErr> {
    heartbeat(db, worker_id, "READY", None, 0, true).await
}

pub async fn delivered(db: &DatabaseConnection, worker_id: &str) -> Result<(), DbErr> {
    heartbeat(db, worker_id, "READY", None, 1, true).await
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
        "DEGRADED",
        decision.outcome_code.as_deref(),
        0,
        false,
    )
    .await?;
    retry_result
}

pub async fn claim_failed(db: &DatabaseConnection, worker_id: &str) -> Result<(), DbErr> {
    heartbeat(db, worker_id, "DEGRADED", Some("RUNNER_FAILED"), 0, false).await
}

async fn retry_or_dead_letter(
    db: &DatabaseConnection,
    worker: &str,
    event: &Event,
    decision: &EvaluationExecutionDecision,
) -> Result<(), DbErr> {
    let txn = db.begin().await?;
    if event.attempts < DELIVERY_LIMIT {
        let statement = Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE evaluation_outbox_events SET status = 'PENDING', available_at = CURRENT_TIMESTAMP + ($1 * INTERVAL '1 second'), \
                 claimed_at = NULL, claimed_by = NULL, last_error = $2 WHERE id = $3 AND status = 'PROCESSING' AND claimed_by = $4 AND attempt_count = $5",
            [
                event.attempts.into(),
                decision.outcome_code.clone().into(),
                event.id.into(),
                worker.into(),
                event.attempts.into(),
            ],
        );
        txn.execute_raw(statement).await?;
        txn.commit().await?;
        return Ok(());
    }
    let run = raw_run(&txn, event.run_id, true).await?;
    let claim_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE evaluation_outbox_events SET status = 'DEAD_LETTER', last_error = $1 WHERE id = $2 AND status = 'PROCESSING' AND claimed_by = $3 AND attempt_count = $4",
        [
            decision.outcome_code.clone().into(),
            event.id.into(),
            worker.into(),
            event.attempts.into(),
        ],
    );
    let claimed = txn.execute_raw(claim_statement).await?;
    if claimed.rows_affected() == 1 {
        if let Some(run) = &run {
            if run.status.may_cancel() {
                terminal_failure(&txn, run, decision).await?;
            }
        }
    }
    txn.commit().await?;
    Ok(())
}

/// Ports `heartbeat()`'s anti-flap upsert: a `READY` heartbeat cannot silently clear an existing
/// `DEGRADED` row unless `recovery` is true (only `idle()`/`delivered()` pass `recovery = true`).
async fn heartbeat(
    db: &DatabaseConnection,
    worker: &str,
    status: &str,
    code: Option<&str>,
    deliveries: i32,
    recovery: bool,
) -> Result<(), DbErr> {
    let queue_statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT count(*) AS count, min(available_at) AS oldest FROM (SELECT available_at FROM evaluation_outbox_events \
             WHERE status IN ('PENDING', 'PROCESSING') ORDER BY available_at LIMIT 101) bounded",
        [],
    );
    let queue_row = db
        .query_one_raw(queue_statement)
        .await?
        .expect("COUNT(*) always returns exactly one row");
    let pending_raw: i64 = queue_row.try_get_by("count")?;
    let oldest: Option<chrono::DateTime<chrono::Utc>> = queue_row.try_get_by("oldest")?;
    let truncated = pending_raw > 100;
    let pending = pending_raw.min(100) as i32;
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO evaluation_worker_heartbeats (worker_id, status, last_failure_code, last_delivery_count, pending_events, oldest_pending_at, pending_truncated) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (worker_id) DO UPDATE SET observed_at = CURRENT_TIMESTAMP, \
           status = CASE WHEN EXCLUDED.status = 'READY' AND evaluation_worker_heartbeats.status = 'DEGRADED' AND NOT $8 THEN 'DEGRADED' ELSE EXCLUDED.status END, \
           last_failure_code = CASE WHEN EXCLUDED.status = 'READY' AND evaluation_worker_heartbeats.status = 'DEGRADED' AND NOT $9 THEN evaluation_worker_heartbeats.last_failure_code ELSE EXCLUDED.last_failure_code END, \
           last_delivery_count = EXCLUDED.last_delivery_count, pending_events = EXCLUDED.pending_events, oldest_pending_at = EXCLUDED.oldest_pending_at, pending_truncated = EXCLUDED.pending_truncated",
        [
            worker.into(),
            status.into(),
            code.into(),
            deliveries.into(),
            pending.into(),
            oldest.into(),
            truncated.into(),
            recovery.into(),
            recovery.into(),
        ],
    );
    db.execute_raw(statement).await?;
    Ok(())
}
