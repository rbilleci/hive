//! The shared timeline/audit/outbox/insert helpers every mutation and the worker call: `audit`,
//! `stage`, `enqueue`, `touch_projection`, the timeline-sequence allocators, and the
//! new-deployment-cycle insert helpers (`insert_deployment`/`insert_plan`/.../
//! `insert_approval_requirement`).
//!
//! Every write here is a SeaORM `ActiveModel` insert, an `update_many` with the guard in its
//! `WHERE` clause, or an `on_conflict` upsert. The two statements that used to read a row inside
//! their own `INSERT ... SELECT` (the policy snapshot and the approval requirement) read it first
//! and insert by key instead: both run inside the command's transaction, after that transaction
//! created the deployment row itself, so no other writer can see or change it in between.

use crate::audit::context::request_metadata;
use crate::entity::enums::{
    DeploymentAttemptStatus, DeploymentAuditAction, DeploymentEvidenceKind,
    DeploymentLifecycleStatus, DeploymentOutboxEventType, DeploymentOutboxStatus, DeploymentRisk,
    DeploymentRuntimeHealthStatus, DeploymentStage, DeploymentStageStatus, DeploymentStrategy,
    EvaluationTargetKind, LogicalEnvironmentClass,
};
use crate::entity::{
    deployment_approval_requirements, deployment_attempts, deployment_audit_events,
    deployment_evidence_snapshots, deployment_outbox_events, deployment_plan_review_facts,
    deployment_plan_versions, deployment_policy_snapshots, deployment_runtime_health,
    deployment_stage_events, deployment_timeline_counters, deployments,
    environment_definition_versions, evaluation_target_projections,
};
use hive_application::deployment::CompiledRequest;
use sea_orm::sea_query::{Expr, ExprTrait, Func, OnConflict, Query};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, NotSet, QueryFilter, QueryOrder,
    QuerySelect, QueryTrait, Set,
};
use serde_json::Value;
use uuid::Uuid;

/// Makes every returned detail/timeline append visible to stale-response protection.
pub async fn touch_projection(db: &impl ConnectionTrait, deployment_id: Uuid) -> Result<(), DbErr> {
    deployments::Entity::update_many()
        .col_expr(
            deployments::Column::ProjectionRevision,
            Expr::col(deployments::Column::ProjectionRevision).add(1),
        )
        .filter(deployments::Column::Id.eq(deployment_id))
        .exec(db)
        .await?;
    Ok(())
}

async fn lock_timeline_deployment(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    let found = deployments::Entity::find_by_id(deployment_id)
        .lock_exclusive()
        .select_only()
        .column(deployments::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?;
    if found.is_none() {
        return Err(DbErr::RecordNotFound(format!(
            "no deployment with id {deployment_id}"
        )));
    }
    Ok(())
}

/// Allocates a cross-table sequence while the deployment row serializes writers for this target.
pub async fn next_timeline_sequence(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    attempt_number: i64,
) -> Result<i64, DbErr> {
    lock_timeline_deployment(db, deployment_id).await?;
    let counter =
        deployment_timeline_counters::Entity::insert(deployment_timeline_counters::ActiveModel {
            deployment_id: Set(deployment_id),
            attempt_number: Set(attempt_number),
            next_sequence: Set(2),
        })
        .on_conflict(
            OnConflict::columns([
                deployment_timeline_counters::Column::DeploymentId,
                deployment_timeline_counters::Column::AttemptNumber,
            ])
            .value(
                deployment_timeline_counters::Column::NextSequence,
                Expr::col((
                    deployment_timeline_counters::Entity,
                    deployment_timeline_counters::Column::NextSequence,
                ))
                .add(1),
            )
            .to_owned(),
        )
        .exec_with_returning(db)
        .await?;
    Ok(counter.next_sequence - 1)
}

pub struct TimelineAnchor {
    pub attempt_id: Option<Uuid>,
    pub attempt_number: i64,
    pub sequence: i64,
}

async fn timeline_anchor(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    supplied_attempt: Option<Uuid>,
) -> Result<TimelineAnchor, DbErr> {
    lock_timeline_deployment(db, deployment_id).await?;
    let mut attempt_id = supplied_attempt;
    let mut attempt_number = 0i64;
    if let Some(id) = attempt_id {
        match deployment_attempts::Entity::find_by_id(id)
            .filter(deployment_attempts::Column::DeploymentId.eq(deployment_id))
            .one(db)
            .await?
        {
            Some(attempt) => attempt_number = attempt.attempt_number,
            None => attempt_id = None,
        }
    }
    if attempt_id.is_none() {
        if let Some(attempt) = deployment_attempts::Entity::find()
            .filter(deployment_attempts::Column::DeploymentId.eq(deployment_id))
            .order_by_desc(deployment_attempts::Column::AttemptNumber)
            .one(db)
            .await?
        {
            attempt_id = Some(attempt.id);
            attempt_number = attempt.attempt_number;
        }
    }
    let sequence = next_timeline_sequence(db, deployment_id, attempt_number).await?;
    Ok(TimelineAnchor {
        attempt_id,
        attempt_number,
        sequence,
    })
}

pub struct AttemptTimelineAnchor {
    pub deployment_id: Uuid,
    pub attempt_number: i64,
}

async fn attempt_timeline_anchor(
    db: &impl ConnectionTrait,
    attempt_id: Uuid,
) -> Result<AttemptTimelineAnchor, DbErr> {
    let attempt = deployment_attempts::Entity::find_by_id(attempt_id)
        .one(db)
        .await?
        .ok_or_else(|| {
            DbErr::RecordNotFound(format!("no deployment_attempts row with id {attempt_id}"))
        })?;
    Ok(AttemptTimelineAnchor {
        deployment_id: attempt.deployment_id,
        attempt_number: attempt.attempt_number,
    })
}

pub async fn stage(
    db: &impl ConnectionTrait,
    attempt_id: Uuid,
    stage: &str,
    status: &str,
    message: &str,
) -> Result<(), DbErr> {
    let anchor = attempt_timeline_anchor(db, attempt_id).await?;
    let timeline_sequence =
        next_timeline_sequence(db, anchor.deployment_id, anchor.attempt_number).await?;
    let stage_value = DeploymentStage::try_from_value(&stage.to_string())?;
    let status_value = DeploymentStageStatus::try_from_value(&status.to_string())?;
    // The attempt's next sequence number is `COALESCE(MAX(sequence_number), 0) + 1` over the rows
    // this attempt already has, taken by the insert itself rather than by a separate read.
    let mut insert = Query::insert();
    insert
        .into_table(deployment_stage_events::Entity)
        .columns([
            deployment_stage_events::Column::Id,
            deployment_stage_events::Column::DeploymentAttemptId,
            deployment_stage_events::Column::SequenceNumber,
            deployment_stage_events::Column::TimelineSequence,
            deployment_stage_events::Column::Stage,
            deployment_stage_events::Column::Status,
            deployment_stage_events::Column::Message,
        ])
        .select_from(
            deployment_stage_events::Entity::find()
                .select_only()
                .expr(Expr::val(Uuid::new_v4()))
                .expr(Expr::val(attempt_id))
                .expr(
                    Expr::from(Func::coalesce([
                        Expr::from(Func::max(Expr::col(
                            deployment_stage_events::Column::SequenceNumber,
                        ))),
                        Expr::val(0_i64),
                    ]))
                    .add(1),
                )
                .expr(Expr::val(timeline_sequence))
                .expr(Expr::val(stage_value.to_value()))
                .expr(Expr::val(status_value.to_value()))
                .expr(Expr::val(message))
                .filter(deployment_stage_events::Column::DeploymentAttemptId.eq(attempt_id))
                .into_query(),
        )
        .map_err(|error| DbErr::Custom(error.to_string()))?;
    db.execute(&insert).await?;
    touch_projection(db, anchor.deployment_id).await
}

pub async fn enqueue(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    event_type: &str,
    mode: &str,
) -> Result<(), DbErr> {
    deployment_outbox_events::Entity::insert(deployment_outbox_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        deployment_id: Set(deployment_id),
        event_type: Set(DeploymentOutboxEventType::try_from_value(
            &event_type.to_string(),
        )?),
        payload: Set(serde_json::json!({ "mode": mode })),
        status: Set(DeploymentOutboxStatus::Pending),
        available_at: NotSet,
        claimed_at: NotSet,
        claimed_by: NotSet,
        attempt_count: NotSet,
        delivered_at: NotSet,
        last_error: NotSet,
        created_at: NotSet,
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

fn attempt_id_from_facts(facts: &Value) -> Option<Uuid> {
    facts
        .get("attemptId")?
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
}

/// `actor` is `None` for a system-attributed event (Java passes `null`), matching every worker-side
/// audit call. Request metadata (`request_id`/`correlation_id`/`graphql_operation`/`source_ip`/
/// `user_agent`) is bound from `crate::audit::context`'s task-local (`None` outside any
/// `/graphql` request, e.g. a worker call), matching Java's `PostgresAuditRequestContext`.
pub async fn audit(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    actor: Option<Uuid>,
    action: &str,
    facts: Value,
) -> Result<(), DbErr> {
    let attempt_id = attempt_id_from_facts(&facts);
    let anchor = timeline_anchor(db, deployment_id, attempt_id).await?;
    let metadata = request_metadata();
    deployment_audit_events::Entity::insert(deployment_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        deployment_id: Set(deployment_id),
        actor_principal_id: Set(actor),
        action: Set(DeploymentAuditAction::try_from_value(&action.to_string())?),
        facts: Set(facts),
        occurred_at: NotSet,
        deployment_attempt_id: Set(anchor.attempt_id),
        attempt_number: Set(Some(anchor.attempt_number)),
        timeline_sequence: Set(Some(anchor.sequence)),
        request_id: Set(metadata.request_id),
        correlation_id: Set(metadata.correlation_id),
        graphql_operation: Set(metadata.graphql_operation),
        source_ip: Set(metadata.source_ip),
        user_agent: Set(metadata.user_agent),
    })
    .exec_without_returning(db)
    .await?;
    touch_projection(db, deployment_id).await
}

/// The audit row every approval-lifecycle statement wrote by hand: no attempt of its own
/// (`deployment_attempt_id` NULL, `attempt_number` 0) and the sequence the zero-attempt counter
/// allocates, where [`audit`] anchors on the deployment's latest attempt instead. Request metadata
/// is bound the same way.
pub async fn system_audit(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    actor: Option<Uuid>,
    action: &str,
    facts: Value,
) -> Result<(), DbErr> {
    let sequence = next_timeline_sequence(db, deployment_id, 0).await?;
    let metadata = request_metadata();
    deployment_audit_events::Entity::insert(deployment_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        deployment_id: Set(deployment_id),
        actor_principal_id: Set(actor),
        action: Set(DeploymentAuditAction::try_from_value(&action.to_string())?),
        facts: Set(facts),
        occurred_at: NotSet,
        deployment_attempt_id: Set(None),
        attempt_number: Set(Some(0)),
        timeline_sequence: Set(Some(sequence)),
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

pub async fn insert_runtime_health(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    deployment_runtime_health::Entity::insert(deployment_runtime_health::ActiveModel {
        deployment_id: Set(deployment_id),
        status: Set(DeploymentRuntimeHealthStatus::NotObserved),
        summary: Set("No local runtime observation is available yet.".to_string()),
        observed_at: NotSet,
        generation: Set(1),
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

/// Ports `PostgresEvaluationTargetProjection.projectDeploymentTarget`. The deployment and its
/// environment definition version are read first and the projection row is written by key: the
/// deployment row is this transaction's own insert, so nothing else can change it in between.
pub async fn project_deployment_target(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<(), DbErr> {
    let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    else {
        return Ok(());
    };
    let Some(environment_id) = deployment.environment_definition_version_id else {
        return Ok(());
    };
    let Some(environment) = environment_definition_versions::Entity::find_by_id(environment_id)
        .one(db)
        .await?
    else {
        return Ok(());
    };
    evaluation_target_projections::Entity::insert(evaluation_target_projections::ActiveModel {
        project_id: Set(deployment.project_id),
        target_kind: Set(EvaluationTargetKind::Deployment),
        target_id: Set(deployment.id),
        agent_version_id: Set(deployment.agent_version_id),
        environment_definition_version_id: Set(environment.id),
        logical_environment_class: Set(environment.logical_environment_class),
        display_name: Set(format!("Deployment {}", deployment.id)),
    })
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

/// The canonical text of a JSON document the compiler produced, as the `jsonb` column holds it.
fn json_document(text: &str) -> Value {
    serde_json::from_str(text).expect("a compiled deployment document is always valid JSON")
}

pub async fn insert_deployment(
    db: &impl ConnectionTrait,
    id: Uuid,
    actor: Uuid,
    request: &CompiledRequest,
    key: &str,
    fingerprint: &str,
) -> Result<(), DbErr> {
    let lifecycle = if request.rule.approvers > 0 {
        DeploymentLifecycleStatus::AwaitingApproval
    } else {
        DeploymentLifecycleStatus::Requested
    };
    deployments::Entity::insert(deployments::ActiveModel {
        id: Set(id),
        organization_id: Set(request.version.organization_id),
        project_id: Set(request.version.project_id),
        agent_id: Set(request.version.agent_id),
        agent_version_id: Set(request.version.id),
        catalog_release_id: Set(request.version.catalog_release_id.clone()),
        catalog_release_digest: Set(request.version.catalog_release_digest.clone()),
        environment: Set(LogicalEnvironmentClass::try_from_value(
            &request.environment.logical_environment_class,
        )?),
        environment_definition_version_id: Set(Some(request.environment.id)),
        target_digest: Set(request.target_digest.clone()),
        strategy: Set(DeploymentStrategy::try_from_value(&request.strategy)?),
        lifecycle_status: Set(lifecycle),
        revision: Set(1),
        idempotency_key: Set(key.trim().to_string()),
        request_fingerprint: Set(Some(fingerprint.to_string())),
        requested_by: Set(actor),
        requested_at: NotSet,
        updated_at: NotSet,
        projection_revision: NotSet,
        project_lifecycle_revision: NotSet,
    })
    .exec_without_returning(db)
    .await?;
    project_deployment_target(db, id).await
}

pub async fn insert_plan(
    db: &impl ConnectionTrait,
    plan_id: Uuid,
    deployment_id: Uuid,
    actor: Uuid,
    request: &CompiledRequest,
) -> Result<(), DbErr> {
    deployment_plan_versions::Entity::insert(deployment_plan_versions::ActiveModel {
        id: Set(plan_id),
        deployment_id: Set(deployment_id),
        version_number: Set(1),
        agent_version_id: Set(request.version.id),
        catalog_release_id: Set(request.version.catalog_release_id.clone()),
        environment: Set(LogicalEnvironmentClass::try_from_value(
            &request.environment.logical_environment_class,
        )?),
        environment_definition_version_id: Set(Some(request.environment.id)),
        agent_content_digest: Set(Some(request.version.content_digest.clone())),
        catalog_release_digest: Set(Some(request.version.catalog_release_digest.clone())),
        target_digest: Set(Some(request.target_digest.clone())),
        compiler_version: Set(hive_application::deployment::compiler::COMPILER_VERSION.to_string()),
        canonical_plan: Set(json_document(&request.canonical_plan)),
        plan_digest: Set(request.plan_digest.clone()),
        package_digest: Set(request.package_digest.clone()),
        package_reference: Set(format!("local://packages/{}", request.package_digest)),
        created_by: Set(actor),
        created_at: NotSet,
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

pub async fn insert_plan_review(
    db: &impl ConnectionTrait,
    plan_id: Uuid,
    review: &hive_application::deployment::CompilerReview,
) -> Result<(), DbErr> {
    deployment_plan_review_facts::Entity::insert(deployment_plan_review_facts::ActiveModel {
        plan_id: Set(plan_id),
        active_agent_version_number: Set(review.active_agent_version_number),
        change_summary: Set(review.change_summary.clone()),
        requested_dependency_versions: Set(serde_json::json!(review.requested_dependency_versions)),
        added_dependency_versions: Set(serde_json::json!(review.added_dependency_versions)),
        removed_dependency_versions: Set(serde_json::json!(review.removed_dependency_versions)),
    })
    .on_conflict(
        OnConflict::column(deployment_plan_review_facts::Column::PlanId)
            .do_nothing()
            .to_owned(),
    )
    .try_insert()
    .exec_without_returning(db)
    .await?;
    Ok(())
}

pub async fn insert_policy_snapshot(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<(), DbErr> {
    // The deleted statement's `SELECT ... FROM deployments WHERE id = $1` inserted nothing when the
    // deployment did not exist; the existence test is the same read, taken in this transaction,
    // which created that row itself.
    if deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
        .is_none()
    {
        return Ok(());
    }
    deployment_policy_snapshots::Entity::insert(deployment_policy_snapshots::ActiveModel {
        deployment_id: Set(deployment_id),
        policy_id: Set(request.policy.id),
        policy_revision: Set(request.policy.revision),
        policy_digest: Set(request.policy.digest.clone()),
        policy_matrix: Set(json_document(&request.policy.matrix)),
        logical_environment_class: Set(LogicalEnvironmentClass::try_from_value(
            &request.environment.logical_environment_class,
        )?),
        risk: Set(DeploymentRisk::try_from_value(&request.risk)?),
        required_evidence: Set(serde_json::json!(request.rule.evidence)),
        required_approvers: Set(request.rule.approvers),
        created_at: NotSet,
        agent_version_id: Set(Some(request.version.id)),
        environment_definition_version_id: Set(Some(request.environment.id)),
        target_digest: Set(Some(request.target_digest.clone())),
        plan_digest: Set(Some(request.plan_digest.clone())),
        package_digest: Set(Some(request.package_digest.clone())),
        binding_digest: Set(Some(request.binding_digest.clone())),
        risk_verification_digest: Set(Some(request.risk_verification_digest.clone())),
        evaluation_requirement_expires_at: Set(request
            .evaluation_requirement_expires_at
            .map(|value| value.fixed_offset())),
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

pub async fn insert_evidence(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<(), DbErr> {
    for kind in &request.rule.evidence {
        // M16 owns evaluation authoring. M14 freezes and validates only an evaluation fact that
        // that owner has already appended for this exact deployment target.
        if kind == "EVALUATION_PASSED" {
            continue;
        }
        deployment_evidence_snapshots::Entity::insert(deployment_evidence_snapshots::ActiveModel {
            id: Set(Uuid::new_v4()),
            deployment_id: Set(deployment_id),
            evidence_kind: Set(DeploymentEvidenceKind::try_from_value(kind)?),
            evidence_digest: Set(hive_application::deployment::compiler::digest(&format!(
                "{}|{}",
                request.binding_digest, kind
            ))),
            expires_at: Set(None),
            created_at: NotSet,
            agent_version_id: Set(Some(request.version.id)),
            environment_definition_version_id: Set(Some(request.environment.id)),
            target_digest: Set(Some(request.target_digest.clone())),
            plan_digest: Set(Some(request.plan_digest.clone())),
            package_digest: Set(Some(request.package_digest.clone())),
            binding_digest: Set(Some(request.binding_digest.clone())),
            source_evaluation_run_id: Set(None),
        })
        .exec_without_returning(db)
        .await?;
    }
    Ok(())
}

/// Inserts the one frozen approval requirement for this idempotent deployment cycle. The
/// deployment row supplies the organization, the project and the requested-at instant the deleted
/// `INSERT ... SELECT` read; the expiry is that instant plus 24 hours.
pub async fn insert_approval_requirement(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    request: &CompiledRequest,
) -> Result<Uuid, DbErr> {
    if let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    {
        deployment_approval_requirements::Entity::insert(
            deployment_approval_requirements::ActiveModel {
                id: Set(Uuid::new_v4()),
                deployment_id: Set(deployment.id),
                revision: Set(1),
                organization_id: Set(deployment.organization_id),
                project_id: Set(deployment.project_id),
                requested_at: Set(deployment.requested_at),
                required_approvers: Set(request.rule.approvers),
                status: Set(crate::entity::enums::ApprovalRequirementStatus::Pending),
                expires_at: Set(deployment.requested_at + chrono::Duration::hours(24)),
                satisfied_at: Set(None),
                rejected_at: NotSet,
                invalidated_at: NotSet,
                invalidation_code: NotSet,
                satisfied_participants: NotSet,
                created_at: NotSet,
            },
        )
        .on_conflict(
            OnConflict::column(deployment_approval_requirements::Column::DeploymentId)
                .do_nothing()
                .to_owned(),
        )
        .try_insert()
        .exec_without_returning(db)
        .await?;
    }
    deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .select_only()
        .column(deployment_approval_requirements::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .ok_or_else(|| {
            DbErr::RecordNotFound(format!(
                "no deployment_approval_requirements row for deployment {deployment_id}"
            ))
        })
}

/// Terminalizes every still-`QUEUED`/`RUNNING` attempt for a deployment being force-terminated
/// (cancel, or the worker's dead-letter path), staging a matching timeline event for each.
pub async fn terminalize_running_attempts(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    status: &str,
    code: &str,
    summary: &str,
    stage_name: &str,
    stage_status: &str,
) -> Result<Vec<Uuid>, DbErr> {
    let terminal = DeploymentAttemptStatus::try_from_value(&status.to_string())?;
    let updated = deployment_attempts::Entity::update_many()
        .col_expr(
            deployment_attempts::Column::Status,
            Expr::val(terminal.to_value()),
        )
        .col_expr(
            deployment_attempts::Column::CompletedAt,
            Expr::current_timestamp(),
        )
        .col_expr(
            deployment_attempts::Column::Generation,
            Expr::col(deployment_attempts::Column::Generation).add(1),
        )
        .col_expr(deployment_attempts::Column::FailureCode, Expr::val(code))
        .col_expr(
            deployment_attempts::Column::FailureSummary,
            Expr::val(summary),
        )
        .filter(deployment_attempts::Column::DeploymentId.eq(deployment_id))
        .filter(deployment_attempts::Column::Status.is_in([
            DeploymentAttemptStatus::Queued,
            DeploymentAttemptStatus::Running,
        ]))
        .exec_with_returning(db)
        .await?;
    let ids: Vec<Uuid> = updated.into_iter().map(|attempt| attempt.id).collect();
    for attempt_id in &ids {
        stage(db, *attempt_id, stage_name, stage_status, summary).await?;
    }
    Ok(ids)
}
