//! `recordApprovalDecision`, the one approval command: the visibility, eligibility and
//! qualifying-approver checks it re-takes under the requirement's own row lock, the replay receipt
//! that makes a retried request idempotent, and the requirement projection every answer carries.
//!
//! A decision that satisfies or rejects the requirement transitions it and moves the deployment
//! with it, through `super::approval`; a refusal that is itself a terminal state reconciles the
//! requirement first, because a generated read cannot write.

use super::queries::active_project_check;
use super::rows::{self, ApprovalDecisionRow, RawRequirement};
use crate::capability::tx;
use crate::entity::enums::{ApprovalDecision as EntityApprovalDecision, LifecycleStatus};
use crate::entity::{
    deployment_approval_decisions, deployment_approval_replay_receipts,
    deployment_approval_requirements, deployment_evidence_invalidations,
    deployment_evidence_snapshots, deployment_policy_snapshots, deployments, principals, projects,
};
use hive_application::deployment::{
    ApprovalDecision, ApprovalDecisionCommand, ApprovalDecisionFacts,
    ApprovalDecisionMutationResult, ApprovalDecisionPlanner, ApprovalDecisionProblem,
    ApprovalEvidenceIssue, ApprovalEvidenceState, ApprovalPrincipal, ApprovalRequirement,
    ApprovalRequirementStatus, ApprovalRule, ApprovalSnapshot, ApprovalTarget, Deployment,
    DeploymentEvidence,
};
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::sea_query::{Expr, ExprTrait, OnConflict};
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    JoinType, NotSet, QueryFilter, QueryOrder, QuerySelect, RelationTrait, Set, TransactionTrait,
    TryInsertResult,
};
use std::collections::HashMap;
use uuid::Uuid;

async fn can_approval_view(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    tx::has_deployment_capability(
        db,
        principal_id,
        tx::DEPLOYMENT_APPROVAL_VIEW,
        project_id,
        lock,
    )
    .await
}

async fn approval_evidence_for(
    db: &impl ConnectionTrait,
    deployment_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<DeploymentEvidence>>, DbErr> {
    let mut values: HashMap<Uuid, Vec<DeploymentEvidence>> =
        deployment_ids.iter().map(|id| (*id, Vec::new())).collect();
    if deployment_ids.is_empty() {
        return Ok(values);
    }
    // Three entity queries read the frozen policies, the evidence snapshots and their
    // invalidations; expanding `policy.required_evidence`, matching each required kind to its
    // snapshot and deciding each state run in Rust. Each query is a read outside any transaction,
    // so it takes no lock and guards no write.
    let policies = deployment_policy_snapshots::Entity::find()
        .join(
            JoinType::InnerJoin,
            deployment_policy_snapshots::Relation::Deployments.def(),
        )
        .filter(deployment_policy_snapshots::Column::DeploymentId.is_in(deployment_ids.to_vec()))
        .all(db)
        .await?;
    let snapshots = deployment_evidence_snapshots::Entity::find()
        .filter(deployment_evidence_snapshots::Column::DeploymentId.is_in(deployment_ids.to_vec()))
        .order_by_asc(deployment_evidence_snapshots::Column::Id)
        .all(db)
        .await?;
    let snapshot_ids: Vec<Uuid> = snapshots.iter().map(|row| row.id).collect();
    let invalidations = if snapshot_ids.is_empty() {
        Vec::new()
    } else {
        deployment_evidence_invalidations::Entity::find()
            .filter(
                deployment_evidence_invalidations::Column::EvidenceSnapshotId.is_in(snapshot_ids),
            )
            .all(db)
            .await?
    };
    let now: DateTimeWithTimeZone = chrono::Utc::now().fixed_offset();
    for policy in &policies {
        // `ORDER BY policy.deployment_id, required.evidence_kind`.
        let mut kinds = rows::string_list(&policy.required_evidence);
        kinds.sort();
        let mut listed = Vec::new();
        for kind in kinds {
            let matching: Vec<&crate::entity::deployment_evidence_snapshots::Model> = snapshots
                .iter()
                .filter(|snapshot| {
                    snapshot.deployment_id == policy.deployment_id
                        && snapshot.evidence_kind.to_value() == kind
                })
                .collect();
            if matching.is_empty() {
                listed.push(DeploymentEvidence {
                    kind,
                    digest: None,
                    binding_digest: None,
                    expires_at: None,
                    state: ApprovalEvidenceState::Missing,
                });
                continue;
            }
            for snapshot in matching {
                listed.push(DeploymentEvidence {
                    kind: kind.clone(),
                    digest: Some(snapshot.evidence_digest.clone()),
                    binding_digest: snapshot.binding_digest.clone(),
                    expires_at: snapshot.expires_at.map(|value| value.to_utc()),
                    state: rows::evidence_state(snapshot, policy, &invalidations, now),
                });
            }
        }
        values.insert(policy.deployment_id, listed);
    }
    Ok(values)
}

async fn approval_principals_for(
    db: &impl ConnectionTrait,
    requirements: &[RawRequirement],
) -> Result<HashMap<Uuid, ApprovalPrincipal>, DbErr> {
    let mut ids: Vec<Uuid> = Vec::new();
    for requirement in requirements {
        if !ids.contains(&requirement.requester_id) {
            ids.push(requirement.requester_id);
        }
        for participant in &requirement.satisfied_participants {
            if !ids.contains(participant) {
                ids.push(*participant);
            }
        }
    }
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut values = HashMap::new();
    for row in principals::Entity::find()
        .filter(principals::Column::Id.is_in(ids))
        .all(db)
        .await?
    {
        values.insert(
            row.id,
            ApprovalPrincipal {
                id: row.id,
                subject: row.subject,
            },
        );
    }
    Ok(values)
}

async fn eligible_approver(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    Ok(tx::has_deployment_capability(
        db,
        principal_id,
        tx::DEPLOYMENT_APPROVAL_DECIDE,
        project_id,
        lock,
    )
    .await?
        && active_project_check(db, project_id, lock).await?)
}

/// Acquires every current and prospective approver authority lock in one stable principal order.
async fn lock_approval_authorities(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
    prospective_actor: Uuid,
    project_id: Uuid,
) -> Result<(), DbErr> {
    let mut actor_ids: std::collections::BTreeSet<Uuid> = approving_actors(db, requirement_id)
        .await?
        .into_iter()
        .collect();
    actor_ids.insert(prospective_actor);
    for actor in actor_ids {
        tx::deployment_approval_capabilities(db, actor, project_id, true).await?;
    }
    Ok(())
}

async fn qualifying_approvers(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
    project_id: Uuid,
    requester_id: Uuid,
    lock: bool,
) -> Result<Vec<Uuid>, DbErr> {
    let mut qualified = Vec::new();
    for actor in approving_actors(db, requirement_id).await? {
        if actor != requester_id && eligible_approver(db, actor, project_id, lock).await? {
            qualified.push(actor);
        }
    }
    Ok(qualified)
}

/// Every principal that recorded an `APPROVE` decision on a requirement, in the stable principal
/// order the authority locks are taken in.
async fn approving_actors(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
) -> Result<Vec<Uuid>, DbErr> {
    deployment_approval_decisions::Entity::find()
        .filter(deployment_approval_decisions::Column::ApprovalRequirementId.eq(requirement_id))
        .filter(deployment_approval_decisions::Column::Decision.eq(EntityApprovalDecision::Approve))
        .order_by_asc(deployment_approval_decisions::Column::ActorPrincipalId)
        .select_only()
        .column(deployment_approval_decisions::Column::ActorPrincipalId)
        .into_tuple::<Uuid>()
        .all(db)
        .await
}

async fn qualifying_approval_count(
    db: &impl ConnectionTrait,
    requirement: &RawRequirement,
) -> Result<i32, DbErr> {
    if requirement.status == ApprovalRequirementStatus::Satisfied {
        return Ok(requirement.satisfied_participants.len() as i32);
    }
    Ok(
        qualifying_approval_counts(db, std::slice::from_ref(requirement))
            .await?
            .get(&requirement.id)
            .copied()
            .unwrap_or(0),
    )
}

async fn qualifying_approval_counts(
    db: &impl ConnectionTrait,
    requirements: &[RawRequirement],
) -> Result<HashMap<Uuid, i32>, DbErr> {
    if requirements.is_empty() {
        return Ok(HashMap::new());
    }
    let mut counts = HashMap::new();
    let mut pending = Vec::new();
    for requirement in requirements {
        if requirement.status == ApprovalRequirementStatus::Satisfied {
            counts.insert(
                requirement.id,
                requirement.satisfied_participants.len() as i32,
            );
        } else {
            pending.push(requirement.id);
            counts.insert(requirement.id, 0);
        }
    }
    if pending.is_empty() {
        return Ok(counts);
    }
    let rows_found = deployment_approval_requirements::Entity::find()
        .join(
            JoinType::InnerJoin,
            deployment_approval_requirements::Relation::Projects
                .def()
                .on_condition(|_left, right| {
                    Condition::all().add(
                        Expr::col((right, projects::Column::LifecycleStatus))
                            .eq(LifecycleStatus::Active.to_value()),
                    )
                }),
        )
        .join(
            JoinType::InnerJoin,
            deployment_approval_requirements::Relation::Deployments.def(),
        )
        .join(
            JoinType::InnerJoin,
            deployment_approval_requirements::Relation::DeploymentApprovalDecisions
                .def()
                .on_condition(|_left, right| {
                    Condition::all()
                        .add(
                            Expr::col((
                                right.clone(),
                                deployment_approval_decisions::Column::Decision,
                            ))
                            .eq(EntityApprovalDecision::Approve.to_value()),
                        )
                        .add(
                            Expr::col((
                                right,
                                deployment_approval_decisions::Column::ActorPrincipalId,
                            ))
                            .ne(Expr::col((
                                deployments::Entity,
                                deployments::Column::RequestedBy,
                            ))),
                        )
                }),
        )
        .filter(deployment_approval_requirements::Column::Id.is_in(pending.clone()))
        .select_only()
        .column(deployment_approval_requirements::Column::Id)
        .column(deployment_approval_requirements::Column::ProjectId)
        .column(deployment_approval_decisions::Column::ActorPrincipalId)
        .distinct()
        .into_tuple::<(Uuid, Uuid, Uuid)>()
        .all(db)
        .await?;
    let mut qualified_cache: HashMap<(Uuid, Uuid), bool> = HashMap::new();
    let mut qualifying_counts: HashMap<Uuid, i32> = HashMap::new();
    for (requirement_id, project_id, actor_id) in rows_found {
        let key = (actor_id, project_id);
        let qualified = match qualified_cache.get(&key) {
            Some(value) => *value,
            None => {
                let value = tx::deployment_approval_capabilities(db, actor_id, project_id, false)
                    .await?
                    .contains(tx::DEPLOYMENT_APPROVAL_DECIDE);
                qualified_cache.insert(key, value);
                value
            }
        };
        if qualified {
            *qualifying_counts.entry(requirement_id).or_insert(0) += 1;
        }
    }
    for id in pending {
        counts.insert(id, *qualifying_counts.get(&id).unwrap_or(&0));
    }
    Ok(counts)
}

async fn has_decision(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
    actor: Uuid,
) -> Result<bool, DbErr> {
    Ok(deployment_approval_decisions::Entity::find()
        .filter(deployment_approval_decisions::Column::ApprovalRequirementId.eq(requirement_id))
        .filter(deployment_approval_decisions::Column::ActorPrincipalId.eq(actor))
        .select_only()
        .column(deployment_approval_decisions::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .is_some())
}

#[allow(clippy::too_many_arguments)]
async fn insert_decision(
    db: &impl ConnectionTrait,
    id: Uuid,
    requirement_id: Uuid,
    actor: Uuid,
    value: &str,
    comment: Option<&str>,
    rejection_reason: Option<&str>,
    request_id: Uuid,
    correlation_id: Uuid,
    expected_revision: i64,
    request_fingerprint: &str,
) -> Result<(), DbErr> {
    let blank = |value: Option<&str>| value.map(str::trim).unwrap_or("").is_empty();
    deployment_approval_decisions::Entity::insert(deployment_approval_decisions::ActiveModel {
        id: Set(id),
        approval_requirement_id: Set(requirement_id),
        actor_principal_id: Set(actor),
        decision: Set(EntityApprovalDecision::try_from_value(&value.to_string())?),
        comment: Set(if value == "REJECT" || blank(comment) {
            None
        } else {
            comment.map(|value| value.trim().to_string())
        }),
        rejection_reason: Set(if value == "REJECT" {
            rejection_reason.map(|value| value.trim().to_string())
        } else {
            None
        }),
        // `eligibility_checked_at` has no column default, so it is bound here; `decided_at`'s own
        // default is the same instant.
        eligibility_checked_at: Set(chrono::Utc::now().fixed_offset()),
        decided_at: NotSet,
        request_key: Set(Some(request_id)),
        correlation_id: Set(Some(correlation_id)),
        request_expected_revision: Set(Some(expected_revision)),
        request_fingerprint: Set(Some(request_fingerprint.to_string())),
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}

/// Hashes the normalized command fields that a retry key owns without exposing review text in storage.
fn decision_request_fingerprint(command: &ApprovalDecisionCommand) -> String {
    let canonical = format!(
        "{}\u{0}{}\u{0}{}\u{0}{}",
        command.value,
        command.expected_revision,
        command.comment.as_deref().unwrap_or("\u{1}"),
        command.rejection_reason.as_deref().unwrap_or("\u{1}")
    );
    hive_application::deployment::compiler::digest(&canonical)
}

fn requirement_state_problem(raw: &RawRequirement) -> ApprovalDecisionProblem {
    if raw.status == ApprovalRequirementStatus::Expired {
        return ApprovalDecisionProblem::of("APPROVAL_REQUIREMENT_EXPIRED");
    }
    if raw.status == ApprovalRequirementStatus::Invalidated {
        if let Some(code) = &raw.invalidation_code {
            if matches!(
                code.as_str(),
                "APPROVAL_EVIDENCE_MISSING"
                    | "APPROVAL_EVIDENCE_EXPIRED"
                    | "APPROVAL_EVIDENCE_MISMATCH"
            ) {
                return ApprovalDecisionProblem::of(code.clone());
            }
        }
    }
    ApprovalDecisionProblem::of("APPROVAL_REQUIREMENT_NOT_PENDING")
}

fn decision_refusal(problem: ApprovalDecisionProblem) -> ApprovalDecisionMutationResult {
    ApprovalDecisionMutationResult::refused(problem)
}

fn approval_decision_from_row(row: ApprovalDecisionRow) -> ApprovalDecision {
    ApprovalDecision {
        id: row.id,
        requirement_id: row.requirement_id,
        actor_principal_id: row.actor_principal_id,
        value: row.value,
        comment: row.comment,
        rejection_reason: row.rejection_reason,
        eligibility_checked_at: row.eligibility_checked_at,
        decided_at: row.decided_at,
    }
}

fn requirement_from_raw(
    raw: &RawRequirement,
    deployment: &Deployment,
    qualifying: i32,
    evidence: Vec<DeploymentEvidence>,
    principals: &HashMap<Uuid, ApprovalPrincipal>,
) -> ApprovalRequirement {
    let status = raw.status;
    let participants = raw.satisfied_participants.clone();
    let qualifying = if status == ApprovalRequirementStatus::Satisfied {
        participants.len() as i32
    } else {
        qualifying
    };
    let target = ApprovalTarget {
        agent_version_id: deployment.plan.agent_version_id,
        agent_version_digest: deployment.plan.agent_content_digest.clone(),
        environment_definition_version_id: deployment.plan.environment_definition_version_id,
        environment_definition_digest: deployment.environment.content_digest.clone(),
        target_digest: deployment.plan.target_digest.clone(),
        deployment_plan_digest: deployment.plan.plan_digest.clone(),
        artifact_digest: deployment.plan.package_digest.clone(),
    };
    let rule = ApprovalRule {
        required_evidence: deployment.policy.required_evidence.clone(),
        required_distinct_approver_count: raw.required_approvers,
    };
    let snapshot = ApprovalSnapshot {
        policy_digest: deployment.policy.policy_digest.clone(),
        policy_revision: deployment.policy.policy_revision,
        environment_class: deployment.policy.logical_environment_class.clone(),
        risk: deployment.policy.risk,
        rule,
        target,
        evidence,
        expires_at: raw.expires_at.unwrap_or(raw.requested_at),
    };
    ApprovalRequirement {
        id: raw.id,
        deployment_id: raw.deployment_id,
        project_id: raw.project_id,
        revision: raw.revision,
        status,
        expires_at: raw.expires_at,
        satisfied_at: raw.satisfied_at,
        rejected_at: raw.rejected_at,
        invalidated_at: raw.invalidated_at,
        requester_id: raw.requester_id,
        requester: principals.get(&raw.requester_id).cloned(),
        required_distinct_approver_count: raw.required_approvers,
        qualifying_approval_count: qualifying,
        satisfied_participants: participants.clone(),
        satisfied_participant_details: participants
            .iter()
            .filter_map(|id| principals.get(id).cloned())
            .collect(),
        approval_snapshot: snapshot,
    }
}

async fn requirement_for(
    db: &impl ConnectionTrait,
    raw: &RawRequirement,
) -> Result<ApprovalRequirement, DbErr> {
    let Some(deployment) = rows::deployments(db, &[raw.deployment_id], true)
        .await?
        .into_iter()
        .next()
    else {
        return Err(DbErr::RecordNotFound(format!(
            "no deployment with id {}",
            raw.deployment_id
        )));
    };
    let qualifying = qualifying_approval_count(db, raw).await?;
    let evidence = approval_evidence_for(db, &[raw.deployment_id])
        .await?
        .remove(&raw.deployment_id)
        .unwrap_or_default();
    let principals = approval_principals_for(db, std::slice::from_ref(raw)).await?;
    Ok(requirement_from_raw(
        raw,
        &deployment,
        qualifying,
        evidence,
        &principals,
    ))
}

async fn reconcile_requirement(
    db: &impl ConnectionTrait,
    raw: RawRequirement,
) -> Result<RawRequirement, DbErr> {
    if raw.status != ApprovalRequirementStatus::Pending {
        return Ok(raw);
    }
    crate::deployment::approval::reconcile_pending(db, raw.deployment_id).await?;
    rows::raw_requirement(db, raw.id, false)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no approval requirement with id {}", raw.id)))
}

async fn decision_by_id(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Option<ApprovalDecision>, DbErr> {
    Ok(deployment_approval_decisions::Entity::find_by_id(id)
        .one(db)
        .await?
        .map(|row| approval_decision_from_row(rows::decision_row(row))))
}

struct ReplayDecision {
    decision: ApprovalDecision,
    request_fingerprint: String,
}

async fn decision_for_request(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
    actor: Uuid,
    request_id: Uuid,
) -> Result<Option<ReplayDecision>, DbErr> {
    Ok(deployment_approval_decisions::Entity::find()
        .filter(deployment_approval_decisions::Column::ApprovalRequirementId.eq(requirement_id))
        .filter(deployment_approval_decisions::Column::ActorPrincipalId.eq(actor))
        .filter(deployment_approval_decisions::Column::RequestKey.eq(request_id))
        .one(db)
        .await?
        .map(|row| {
            let request_fingerprint = row.request_fingerprint.clone().unwrap_or_default();
            ReplayDecision {
                decision: approval_decision_from_row(rows::decision_row(row)),
                request_fingerprint,
            }
        }))
}

pub async fn record_approval_decision(
    db: &DatabaseConnection,
    command: ApprovalDecisionCommand,
    planner: ApprovalDecisionPlanner,
) -> Result<ApprovalDecisionMutationResult, DbErr> {
    let txn = db.begin().await?;
    let result = record_approval_decision_tx(&txn, &command, planner).await;
    match result {
        Ok(value) => {
            txn.commit().await?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

async fn record_approval_decision_tx(
    db: &impl ConnectionTrait,
    command: &ApprovalDecisionCommand,
    planner: ApprovalDecisionPlanner,
) -> Result<ApprovalDecisionMutationResult, DbErr> {
    let Some(initial) = rows::raw_requirement(db, command.requirement_id, false).await? else {
        return Ok(decision_refusal(ApprovalDecisionProblem::unavailable()));
    };
    if rows::deployments(db, &[initial.deployment_id], false)
        .await?
        .is_empty()
    {
        return Ok(decision_refusal(ApprovalDecisionProblem::unavailable()));
    }
    let Some(raw) = rows::raw_requirement(db, command.requirement_id, true).await? else {
        return Ok(decision_refusal(ApprovalDecisionProblem::unavailable()));
    };
    lock_approval_authorities(db, raw.id, command.principal_id, raw.project_id).await?;
    let visible = can_approval_view(db, command.principal_id, initial.project_id, true).await?;
    if !visible {
        let facts = ApprovalDecisionFacts {
            requirement_id: raw.id,
            requester_id: raw.requester_id,
            revision: raw.revision,
            status: raw.status,
            invalidation_code: raw.invalidation_code.clone(),
            required_approvers: raw.required_approvers,
            qualifying_approvers: 0,
            visible: false,
            eligible: false,
            duplicate: false,
            expired: false,
            evidence_issue: None,
            waiting_for_evaluation: false,
        };
        let hidden = planner.plan(command, &facts);
        return Ok(decision_refusal(
            hidden
                .problem
                .unwrap_or_else(ApprovalDecisionProblem::unavailable),
        ));
    }
    if let Some(replay) = decision_for_request(
        db,
        command.requirement_id,
        command.principal_id,
        command.request_id,
    )
    .await?
    {
        if decision_request_fingerprint(command) != replay.request_fingerprint {
            return Ok(decision_refusal(ApprovalDecisionProblem::of(
                "IDEMPOTENCY_CONFLICT",
            )));
        }
        let facts = serde_json::json!({
            "decisionId": replay.decision.id.to_string(),
            "requirementId": command.requirement_id.to_string(),
            "requestId": command.request_id.to_string(),
            "correlationId": command.correlation_id.to_string(),
            "outcome": "IMMUTABLE_DECISION_RETURNED",
        });
        // One durable transport-recovery fact per immutable decision and request; a duplicate
        // retry finds its receipt and leaves the projection unchanged.
        let receipt = deployment_approval_replay_receipts::Entity::insert(
            deployment_approval_replay_receipts::ActiveModel {
                decision_id: Set(replay.decision.id),
                request_id: Set(command.request_id),
                recorded_at: NotSet,
            },
        )
        .on_conflict(
            OnConflict::columns([
                deployment_approval_replay_receipts::Column::DecisionId,
                deployment_approval_replay_receipts::Column::RequestId,
            ])
            .do_nothing()
            .to_owned(),
        )
        .try_insert()
        .exec_without_returning(db)
        .await?;
        // `exec_without_returning` reports the rows the statement affected, which an
        // `ON CONFLICT DO NOTHING` that found the receipt already there leaves at zero.
        if matches!(receipt, TryInsertResult::Inserted(rows) if rows > 0) {
            super::rows::audit(
                db,
                raw.deployment_id,
                Some(command.principal_id),
                "APPROVAL_REPLAYED",
                facts,
            )
            .await?;
        }
        let current_requirement = rows::raw_requirement(db, command.requirement_id, false).await?;
        let current_deployment = rows::deployments(db, &[raw.deployment_id], false)
            .await?
            .into_iter()
            .next();
        return match (current_requirement, current_deployment) {
            (Some(current_requirement), Some(current_deployment)) => {
                let requirement = requirement_for(db, &current_requirement).await?;
                Ok(ApprovalDecisionMutationResult::success(
                    replay.decision,
                    requirement,
                    current_deployment,
                ))
            }
            _ => Ok(decision_refusal(ApprovalDecisionProblem::unavailable())),
        };
    }
    if crate::deployment::approval::deployment_archive_boundary(db, raw.deployment_id).await? {
        return Ok(decision_refusal(ApprovalDecisionProblem::of(
            "PROJECT_ARCHIVED",
        )));
    }
    let eligible = eligible_approver(db, command.principal_id, raw.project_id, true).await?;
    if !eligible {
        return Ok(decision_refusal(ApprovalDecisionProblem::of(
            "APPROVER_INELIGIBLE",
        )));
    }
    let expired = crate::deployment::approval::requirement_expired(db, raw.id).await?;
    let evidence_issue =
        crate::deployment::approval::approval_evidence_issue(db, raw.deployment_id).await?;
    let waiting_for_evaluation = evidence_issue == Some(ApprovalEvidenceIssue::Missing)
        && crate::deployment::approval::waiting_for_evaluation(db, raw.deployment_id).await?;
    let qualifying = qualifying_approvers(db, raw.id, raw.project_id, raw.requester_id, true)
        .await?
        .len() as i32;
    let facts = ApprovalDecisionFacts {
        requirement_id: raw.id,
        requester_id: raw.requester_id,
        revision: raw.revision,
        status: raw.status,
        invalidation_code: raw.invalidation_code.clone(),
        required_approvers: raw.required_approvers,
        qualifying_approvers: qualifying,
        visible,
        eligible,
        duplicate: false,
        expired,
        evidence_issue,
        waiting_for_evaluation,
    };
    let duplicate = has_decision(db, command.requirement_id, command.principal_id).await?;
    let plan = planner.plan(command, &facts);
    if !plan.accepted() {
        // A generated read cannot write, so a still-`PENDING` requirement that has reached a
        // terminal state is reconciled here, inside the transaction that already holds the
        // requirement and deployment locks. Only a refusal that *is* a terminal state reconciles:
        // an ineligible, duplicate or self-approving actor changes nothing about the requirement.
        if expired || evidence_issue.is_some() {
            reconcile_requirement(db, raw).await?;
        }
        return Ok(decision_refusal(
            plan.problem
                .unwrap_or_else(ApprovalDecisionProblem::unavailable),
        ));
    }
    let facts_with_duplicate = ApprovalDecisionFacts { duplicate, ..facts };
    let plan = planner.plan(command, &facts_with_duplicate);
    let raw = reconcile_requirement(db, raw).await?;
    if raw.status != ApprovalRequirementStatus::Pending {
        let reconciled_facts = ApprovalDecisionFacts {
            status: raw.status,
            invalidation_code: raw.invalidation_code.clone(),
            expired: false,
            evidence_issue: None,
            waiting_for_evaluation: false,
            ..facts_with_duplicate
        };
        let reconciled = planner.plan(command, &reconciled_facts);
        return Ok(decision_refusal(
            reconciled
                .problem
                .unwrap_or_else(ApprovalDecisionProblem::unavailable),
        ));
    }
    if !plan.accepted() {
        return Ok(decision_refusal(
            plan.problem
                .unwrap_or_else(ApprovalDecisionProblem::unavailable),
        ));
    }
    // Repeat the expiry check at the write boundary.
    if crate::deployment::approval::requirement_expired(db, raw.id).await? {
        let raw = reconcile_requirement(db, raw).await?;
        return Ok(decision_refusal(requirement_state_problem(&raw)));
    }
    let decision_id = Uuid::new_v4();
    insert_decision(
        db,
        decision_id,
        command.requirement_id,
        command.principal_id,
        &command.value,
        command.comment.as_deref(),
        command.rejection_reason.as_deref(),
        command.request_id,
        command.correlation_id,
        command.expected_revision,
        &decision_request_fingerprint(command),
    )
    .await?;
    super::rows::audit(
        db,
        raw.deployment_id,
        Some(command.principal_id),
        "APPROVAL_RECORDED",
        serde_json::json!({
            "decisionId": decision_id.to_string(), "decision": command.value, "requirementId": command.requirement_id.to_string(),
            "requestId": command.request_id.to_string(), "correlationId": command.correlation_id.to_string(),
            "eligibilityCapability": tx::DEPLOYMENT_APPROVAL_DECIDE, "eligibilityGranted": true,
        }),
    )
    .await?;
    if plan.rejection {
        crate::deployment::approval::transition_requirement(
            db,
            raw.id,
            ApprovalRequirementStatus::Rejected,
            None,
            &[],
        )
        .await?;
        crate::deployment::approval::cancel_rejected_deployment(db, raw.deployment_id).await?;
        super::rows::audit(
            db,
            raw.deployment_id,
            Some(command.principal_id),
            "APPROVAL_REJECTED",
            serde_json::json!({
                "decisionId": decision_id.to_string(), "requirementId": command.requirement_id.to_string(),
                "rejectionReason": command.rejection_reason.as_deref().unwrap_or("").trim(), "correlationId": command.correlation_id.to_string(),
            }),
        )
        .await?;
    } else if plan.satisfies_requirement {
        let participants =
            qualifying_approvers(db, raw.id, raw.project_id, raw.requester_id, true).await?;
        crate::deployment::approval::transition_requirement(
            db,
            raw.id,
            ApprovalRequirementStatus::Satisfied,
            None,
            &participants,
        )
        .await?;
        crate::deployment::approval::approve_deployment_for_execution(db, raw.deployment_id)
            .await?;
        super::rows::audit(
            db,
            raw.deployment_id,
            Some(command.principal_id),
            "APPROVAL_SATISFIED",
            serde_json::json!({
                "requirementId": command.requirement_id.to_string(),
                "participantIds": participants.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "correlationId": command.correlation_id.to_string(),
            }),
        )
        .await?;
    }
    let result = rows::raw_requirement(db, command.requirement_id, false).await?;
    let current = rows::deployments(db, &[raw.deployment_id], false)
        .await?
        .into_iter()
        .next();
    let recorded = decision_by_id(db, decision_id).await?;
    match (result, current, recorded) {
        (Some(result), Some(current), Some(recorded)) => {
            let response_requirement = requirement_for(db, &result).await?;
            Ok(ApprovalDecisionMutationResult::success(
                recorded,
                response_requirement,
                current,
            ))
        }
        _ => Ok(decision_refusal(ApprovalDecisionProblem::unavailable())),
    }
}

#[cfg(test)]
mod decision_rule_tests {
    use super::{decision_request_fingerprint, requirement_state_problem};
    use crate::deployment::rows::RawRequirement;
    use hive_application::deployment::{ApprovalDecisionCommand, ApprovalRequirementStatus};
    use uuid::{uuid, Uuid};

    const REQUIREMENT: Uuid = uuid!("11111111-1111-1111-1111-111111111111");

    fn command() -> ApprovalDecisionCommand {
        ApprovalDecisionCommand {
            principal_id: Uuid::nil(),
            requirement_id: REQUIREMENT,
            expected_revision: 3,
            value: "APPROVE".to_string(),
            comment: Some("REVIEWED_CHANGE_SCOPE".to_string()),
            rejection_reason: None,
            request_id: Uuid::nil(),
            correlation_id: Uuid::nil(),
        }
    }

    fn requirement(
        status: ApprovalRequirementStatus,
        invalidation_code: Option<&str>,
    ) -> RawRequirement {
        RawRequirement {
            id: REQUIREMENT,
            deployment_id: Uuid::nil(),
            project_id: Uuid::nil(),
            requester_id: Uuid::nil(),
            requested_at: chrono::Utc::now(),
            revision: 3,
            status,
            expires_at: None,
            satisfied_at: None,
            rejected_at: None,
            invalidated_at: None,
            invalidation_code: invalidation_code.map(str::to_string),
            satisfied_participants: Vec::new(),
            required_approvers: 2,
        }
    }

    /// The fingerprint is what decides whether a retried decision is the same command. Each of the
    /// four fields it covers must change it, and it must not be reversible to the review text.
    #[test]
    fn every_field_the_retry_key_owns_changes_the_fingerprint() {
        let base = decision_request_fingerprint(&command());
        assert_eq!(base, decision_request_fingerprint(&command()));
        assert!(!base.contains("REVIEWED_CHANGE_SCOPE"));

        let mut other_value = command();
        other_value.value = "REJECT".to_string();
        let mut other_revision = command();
        other_revision.expected_revision = 4;
        let mut other_comment = command();
        other_comment.comment = Some("AUTHORIZATION_GRANTED".to_string());
        let mut other_reason = command();
        other_reason.rejection_reason = Some("CHANGE_SCOPE_NOT_APPROVED".to_string());
        for changed in [other_value, other_revision, other_comment, other_reason] {
            assert_ne!(base, decision_request_fingerprint(&changed));
        }
    }

    /// An absent optional is its own value, distinct from the empty string, so clearing a comment
    /// is not the same command as never having written one.
    #[test]
    fn an_absent_comment_is_not_the_same_command_as_an_empty_one() {
        let mut absent = command();
        absent.comment = None;
        let mut empty = command();
        empty.comment = Some(String::new());
        assert_ne!(
            decision_request_fingerprint(&absent),
            decision_request_fingerprint(&empty)
        );
    }

    /// The two fields are separated, so moving text from the comment to the rejection reason is a
    /// different command rather than the same concatenation.
    #[test]
    fn the_comment_and_the_rejection_reason_do_not_run_together() {
        let mut commented = command();
        commented.comment = Some("AB".to_string());
        commented.rejection_reason = None;
        let mut reasoned = command();
        reasoned.comment = Some("A".to_string());
        reasoned.rejection_reason = Some("B".to_string());
        assert_ne!(
            decision_request_fingerprint(&commented),
            decision_request_fingerprint(&reasoned)
        );
    }

    #[test]
    fn an_expired_requirement_reports_its_expiry() {
        assert_eq!(
            requirement_state_problem(&requirement(ApprovalRequirementStatus::Expired, None)).code,
            "APPROVAL_REQUIREMENT_EXPIRED"
        );
    }

    /// An invalidated requirement reports the evidence code that invalidated it, so the caller
    /// learns which evidence to re-establish.
    #[test]
    fn an_invalidated_requirement_reports_its_evidence_code() {
        for code in [
            "APPROVAL_EVIDENCE_MISSING",
            "APPROVAL_EVIDENCE_EXPIRED",
            "APPROVAL_EVIDENCE_MISMATCH",
        ] {
            assert_eq!(
                requirement_state_problem(&requirement(
                    ApprovalRequirementStatus::Invalidated,
                    Some(code)
                ))
                .code,
                code
            );
        }
    }

    /// A code outside the three is not passed through: an unrecognized reason must not reach the
    /// client as a refusal code it cannot interpret.
    #[test]
    fn an_unrecognized_invalidation_code_falls_back_to_not_pending() {
        for code in [None, Some("SOMETHING_ELSE")] {
            assert_eq!(
                requirement_state_problem(&requirement(
                    ApprovalRequirementStatus::Invalidated,
                    code
                ))
                .code,
                "APPROVAL_REQUIREMENT_NOT_PENDING"
            );
        }
    }

    /// An evidence code on a requirement that was not invalidated is not its refusal.
    #[test]
    fn a_satisfied_or_rejected_requirement_is_only_not_pending() {
        for status in [
            ApprovalRequirementStatus::Satisfied,
            ApprovalRequirementStatus::Rejected,
        ] {
            assert_eq!(
                requirement_state_problem(&requirement(status, Some("APPROVAL_EVIDENCE_MISSING")))
                    .code,
                "APPROVAL_REQUIREMENT_NOT_PENDING"
            );
        }
    }
}
