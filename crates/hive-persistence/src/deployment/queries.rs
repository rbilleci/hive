//! Ports the read-only `DeploymentRepository` methods that are still repository methods:
//! compile-context resolution (Group B) and the approval inbox/decision/requirement surface
//! (Group D). The list/find/timeline/detail/environments reads (Group C) are deleted: they are
//! generated entity queries now (`docs/idiomatic-seaography-plan.md`, A2), with the nested
//! structures answered by relations and by `super::computed`.

use super::cursors::{self, ApprovalCursor};
use super::rows::{self, uuid_array, ApprovalDecisionRow, RawRequirement};
use crate::capability::{queries as capability_queries, tx};
use hive_application::deployment::{
    ActiveTarget, ApprovalDecision, ApprovalDecisionConnection, ApprovalDecisionMutationResult,
    ApprovalDecisionPlanner, ApprovalDecisionPreview, ApprovalInboxConnection, ApprovalInboxItem,
    ApprovalPrincipal, ApprovalRequirement, ApprovalRule, ApprovalSnapshot, ApprovalTarget,
    Deployment, DeploymentCompilationContext, DeploymentEvidence,
    DeploymentRecoveryCompilationContext, EnvironmentDefinition, PolicySource, VersionSource,
};
use hive_domain::deployment::{
    ApprovalDecisionCommand, ApprovalDecisionFacts,
    ApprovalDecisionProblem as DomainApprovalDecisionProblem,
};
use hive_domain::deployment::{ApprovalRequirementStatus, DeploymentLifecycleStatus};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, TransactionTrait};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

async fn can_view(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    tx::has_deployment_capability(db, principal_id, tx::DEPLOYMENT_VIEW, project_id, lock).await
}

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

async fn version_source(
    db: &impl ConnectionTrait,
    version_id: Uuid,
    lock: bool,
) -> Result<Option<VersionSource>, DbErr> {
    let suffix = if lock {
        " FOR KEY SHARE OF version, agent, project"
    } else {
        ""
    };
    let sql = format!(
        "SELECT version.id AS version_id, project.id AS project_id, agent.id AS agent_id, agent.display_name, version.version_number, version.content_digest, \
             version.catalog_release_id, version.catalog_release_digest, project.organization_id, version.canonical_document::text AS canonical_document \
         FROM agent_versions version JOIN agents agent ON agent.id = version.agent_id JOIN projects project ON project.id = agent.project_id \
         WHERE version.id = $1{suffix}"
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [version_id.into()]);
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(rows::version_source_row(&row)?)),
        None => Ok(None),
    }
}

async fn environment(
    db: &impl ConnectionTrait,
    environment_id: Uuid,
    release_id: &str,
) -> Result<Option<EnvironmentDefinition>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, stable_definition_id, version, display_name, logical_environment_class, catalog_release_id, catalog_release_digest, content_digest \
         FROM environment_definition_versions WHERE id = $1 AND catalog_release_id = $2",
        [environment_id.into(), release_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(rows::compiler_environment_row(&row)?)),
        None => Ok(None),
    }
}

async fn policy(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    lock: bool,
) -> Result<Option<PolicySource>, DbErr> {
    let suffix = if lock {
        " FOR SHARE OF policy, version"
    } else {
        ""
    };
    let sql = format!(
        "SELECT policy.id, version.revision, version.digest, version.matrix::text AS matrix \
         FROM project_approval_policies policy JOIN project_approval_policy_versions version \
           ON version.policy_id = policy.id AND version.revision = policy.current_revision WHERE policy.project_id = $1{suffix}"
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [project_id.into()]);
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(PolicySource {
            id: row.try_get_by("id")?,
            revision: row.try_get_by("revision")?,
            digest: row.try_get_by("digest")?,
            matrix: row.try_get_by("matrix")?,
        })),
        None => Ok(None),
    }
}

async fn active_target(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    agent_id: Uuid,
    environment_id: Uuid,
    lock: bool,
) -> Result<Option<ActiveTarget>, DbErr> {
    let suffix = if lock { " FOR SHARE OF deployment" } else { "" };
    let sql = format!(
        "SELECT plan.target_digest, version.canonical_document::text AS canonical_document, deployment.id, deployment.agent_version_id, version.version_number, deployment.requested_at \
         FROM deployments deployment JOIN deployment_plan_versions plan ON plan.deployment_id = deployment.id AND plan.version_number = 1 \
           JOIN agent_versions version ON version.id = deployment.agent_version_id \
         WHERE deployment.project_id = $1 AND deployment.agent_id = $2 AND deployment.environment_definition_version_id = $3 \
           AND deployment.lifecycle_status = 'ACTIVE' \
         ORDER BY deployment.updated_at DESC, deployment.id DESC LIMIT 1{suffix}"
    );
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        &sql,
        [project_id.into(), agent_id.into(), environment_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(ActiveTarget {
            target_digest: row.try_get_by("target_digest")?,
            canonical_document: row.try_get_by("canonical_document")?,
            deployment_id: row.try_get_by("id")?,
            agent_version_id: row.try_get_by("agent_version_id")?,
            agent_version_number: row.try_get_by("version_number")?,
            requested_at: row.try_get_by("requested_at")?,
        })),
        None => Ok(None),
    }
}

pub async fn compilation_context(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    version_id: Uuid,
    environment_id: Uuid,
) -> Result<Option<DeploymentCompilationContext>, DbErr> {
    let Some(version) = version_source(db, version_id, false).await? else {
        return Ok(None);
    };
    if !can_view(db, principal_id, version.project_id, false).await? {
        return Ok(None);
    }
    let environment_value = environment(db, environment_id, &version.catalog_release_id).await?;
    let policy_value = policy(db, version.project_id, false).await?;
    let current_target = active_target(
        db,
        version.project_id,
        version.agent_id,
        environment_id,
        false,
    )
    .await?;
    match (environment_value, policy_value) {
        (Some(environment_value), Some(policy_value)) => Ok(Some(DeploymentCompilationContext {
            version,
            environment: environment_value,
            policy: policy_value,
            current_target,
        })),
        _ => Ok(None),
    }
}

fn canonical_target_version(value: Option<&str>) -> Option<String> {
    let value = value?;
    match Uuid::parse_str(value) {
        Ok(id) => Some(id.to_string()),
        Err(_) => Some(value.trim().to_string()),
    }
}

async fn rollback_target_version(
    db: &impl ConnectionTrait,
    source: &Deployment,
    requested: Option<&str>,
) -> Result<Option<Uuid>, DbErr> {
    let requested_id = match requested {
        Some(value) => match Uuid::parse_str(value) {
            Ok(id) => Some(id),
            Err(_) => return Ok(None),
        },
        None => None,
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT deployment.agent_version_id FROM deployments deployment \
           JOIN deployment_runtime_health health ON health.deployment_id = deployment.id \
         WHERE deployment.project_id = $1 AND deployment.agent_id = $2 AND deployment.environment_definition_version_id = $3 \
           AND deployment.lifecycle_status = 'ACTIVE' AND deployment.id <> $4 \
           AND (deployment.requested_at, deployment.id) < ($5::timestamptz, $6::uuid) \
           AND ($7::uuid IS NULL OR deployment.agent_version_id = $7::uuid) \
         ORDER BY deployment.requested_at DESC, deployment.id DESC LIMIT 1",
        [
            source.project_id.into(),
            source.agent_id.into(),
            source.environment.id.into(),
            source.id.into(),
            source.requested_at.into(),
            source.id.into(),
            requested_id.into(),
        ],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(row.try_get_by("agent_version_id")?)),
        None => Ok(None),
    }
}

async fn recovery_compilation_inputs(
    db: &impl ConnectionTrait,
    source: &Deployment,
    version_id: Option<Uuid>,
    lock: bool,
) -> Result<Option<DeploymentRecoveryCompilationContext>, DbErr> {
    let Some(version_id) = version_id else {
        return Ok(None);
    };
    let Some(version) = version_source(db, version_id, lock).await? else {
        return Ok(None);
    };
    if version.project_id != source.project_id || version.agent_id != source.agent_id {
        return Ok(None);
    }
    let environment_value =
        environment(db, source.environment.id, &version.catalog_release_id).await?;
    let policy_value = policy(db, source.project_id, lock).await?;
    let current_target = active_target(
        db,
        source.project_id,
        source.agent_id,
        source.environment.id,
        lock,
    )
    .await?;
    match (environment_value, policy_value) {
        (Some(environment_value), Some(policy_value)) => {
            Ok(Some(DeploymentRecoveryCompilationContext {
                version,
                environment: environment_value,
                policy: policy_value,
                current_target,
                strategy: source.strategy.clone(),
            }))
        }
        _ => Ok(None),
    }
}

pub async fn recovery_compilation_context(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    deployment_id: Uuid,
    target_agent_version_id: Option<&str>,
    retry: bool,
) -> Result<Option<DeploymentRecoveryCompilationContext>, DbErr> {
    let Some(source) = rows::deployments(db, &[deployment_id], true)
        .await?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    if !can_view(db, principal_id, source.project_id, false).await? {
        return Ok(None);
    }
    let capability = if retry {
        tx::DEPLOYMENT_RETRY
    } else {
        tx::DEPLOYMENT_ROLLBACK
    };
    if !tx::has_deployment_capability(db, principal_id, capability, source.project_id, false)
        .await?
    {
        return Ok(None);
    }
    let version_id = if retry {
        Some(source.agent_version_id)
    } else {
        let canonical = canonical_target_version(target_agent_version_id);
        rollback_target_version(db, &source, canonical.as_deref()).await?
    };
    recovery_compilation_inputs(db, &source, version_id, false).await
}

// --- approval inbox / detail / decisions / recordApprovalDecision ---

async fn approval_evidence_for(
    db: &impl ConnectionTrait,
    deployment_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<DeploymentEvidence>>, DbErr> {
    let mut values: HashMap<Uuid, Vec<DeploymentEvidence>> =
        deployment_ids.iter().map(|id| (*id, Vec::new())).collect();
    if deployment_ids.is_empty() {
        return Ok(values);
    }
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT policy.deployment_id, required.evidence_kind, evidence.evidence_digest, evidence.binding_digest, evidence.expires_at, \
           CASE WHEN evidence.id IS NULL THEN 'MISSING' \
                WHEN EXISTS (SELECT 1 FROM deployment_evidence_invalidations invalidation \
                             WHERE invalidation.evidence_snapshot_id = evidence.id AND invalidation.kind = 'FAILED') THEN 'FAILED' \
                WHEN EXISTS (SELECT 1 FROM deployment_evidence_invalidations invalidation \
                             WHERE invalidation.evidence_snapshot_id = evidence.id AND invalidation.kind = 'REVOKED') THEN 'REVOKED' \
                WHEN evidence.expires_at <= clock_timestamp() THEN 'EXPIRED' \
                WHEN evidence.binding_digest = policy.binding_digest AND evidence.agent_version_id = policy.agent_version_id \
                  AND evidence.environment_definition_version_id = policy.environment_definition_version_id \
                  AND evidence.target_digest = policy.target_digest AND evidence.plan_digest = policy.plan_digest \
                  AND evidence.package_digest = policy.package_digest THEN 'VALID' \
                ELSE 'MISMATCH' END AS evidence_state \
         FROM deployment_policy_snapshots policy \
         JOIN deployments deployment ON deployment.id = policy.deployment_id \
         CROSS JOIN LATERAL jsonb_array_elements_text(policy.required_evidence) required(evidence_kind) \
         LEFT JOIN deployment_evidence_snapshots evidence ON evidence.deployment_id = deployment.id \
           AND evidence.evidence_kind = required.evidence_kind \
         WHERE policy.deployment_id = ANY($1) \
         ORDER BY policy.deployment_id, required.evidence_kind",
        [uuid_array(deployment_ids)],
    );
    let rows_found = db.query_all_raw(statement).await?;
    for row in rows_found {
        let deployment_id: Uuid = row.try_get_by("deployment_id")?;
        let evidence = DeploymentEvidence {
            kind: row.try_get_by("evidence_kind")?,
            digest: row.try_get_by("evidence_digest")?,
            binding_digest: row.try_get_by("binding_digest")?,
            expires_at: row.try_get_by("expires_at")?,
            state: row.try_get_by("evidence_state")?,
        };
        values.entry(deployment_id).or_default().push(evidence);
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, subject FROM principals WHERE id = ANY($1)",
        [uuid_array(&ids)],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values = HashMap::new();
    for row in rows_found {
        let id: Uuid = row.try_get_by("id")?;
        values.insert(
            id,
            ApprovalPrincipal {
                id,
                subject: row.try_get_by("subject")?,
            },
        );
    }
    Ok(values)
}

async fn approval_decision_previews(
    db: &impl ConnectionTrait,
    requirement_ids: &[Uuid],
) -> Result<HashMap<Uuid, ApprovalDecisionPreview>, DbErr> {
    if requirement_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut by_requirement: HashMap<Uuid, Vec<ApprovalDecisionRow>> =
        requirement_ids.iter().map(|id| (*id, Vec::new())).collect();
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT decision.id, decision.approval_requirement_id, decision.actor_principal_id, decision.decision, \
           decision.comment, decision.rejection_reason, decision.eligibility_checked_at, decision.decided_at \
         FROM unnest($1::uuid[]) requirement(id) \
         CROSS JOIN LATERAL ( \
           SELECT id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, decided_at \
           FROM deployment_approval_decisions WHERE approval_requirement_id = requirement.id \
           ORDER BY decided_at ASC, id ASC LIMIT 51 \
         ) decision \
         ORDER BY decision.approval_requirement_id ASC, decision.decided_at ASC, decision.id ASC",
        [uuid_array(requirement_ids)],
    );
    let rows_found = db.query_all_raw(statement).await?;
    for row in &rows_found {
        let decision = rows::decision_row(row)?;
        by_requirement
            .entry(decision.requirement_id)
            .or_default()
            .push(decision);
    }
    Ok(by_requirement
        .into_iter()
        .map(|(requirement_id, decisions)| {
            let cursors_list: Vec<String> = decisions
                .iter()
                .map(|decision| {
                    cursors::encode_approval_decision_cursor(
                        requirement_id,
                        decision.decided_at,
                        decision.id,
                    )
                })
                .collect();
            let nodes = decisions
                .into_iter()
                .map(approval_decision_from_row)
                .collect();
            (
                requirement_id,
                ApprovalDecisionPreview {
                    nodes,
                    cursors: cursors_list,
                },
            )
        })
        .collect())
}

async fn approval_decision_requirement_ids(
    db: &impl ConnectionTrait,
    requirement_ids: &[Uuid],
    principal_id: Uuid,
) -> Result<HashSet<Uuid>, DbErr> {
    if requirement_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT DISTINCT approval_requirement_id FROM deployment_approval_decisions WHERE actor_principal_id = $1 AND approval_requirement_id = ANY($2)",
        [principal_id.into(), uuid_array(requirement_ids)],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values = HashSet::new();
    for row in rows_found {
        values.insert(row.try_get_by::<Uuid, _>("approval_requirement_id")?);
    }
    Ok(values)
}

async fn active_project_check(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let sql = format!(
        "SELECT 1 FROM projects WHERE id = $1 AND lifecycle_status = 'ACTIVE'{}",
        if lock { " FOR SHARE" } else { "" }
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [project_id.into()]);
    Ok(db.query_one_raw(statement).await?.is_some())
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

async fn eligible_approver_cached(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    cache: &mut HashMap<(Uuid, Uuid), bool>,
) -> Result<bool, DbErr> {
    let key = (principal_id, project_id);
    if let Some(value) = cache.get(&key) {
        return Ok(*value);
    }
    let value = eligible_approver(db, principal_id, project_id, false).await?;
    cache.insert(key, value);
    Ok(value)
}

/// Acquires every current and prospective approver authority lock in one stable principal order.
async fn lock_approval_authorities(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
    prospective_actor: Uuid,
    project_id: Uuid,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT actor_principal_id FROM deployment_approval_decisions WHERE approval_requirement_id = $1 AND decision = 'APPROVE' ORDER BY actor_principal_id",
        [requirement_id.into()],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut actor_ids: std::collections::BTreeSet<Uuid> = std::collections::BTreeSet::new();
    for row in rows_found {
        actor_ids.insert(row.try_get_by("actor_principal_id")?);
    }
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT actor_principal_id FROM deployment_approval_decisions WHERE approval_requirement_id = $1 AND decision = 'APPROVE' ORDER BY actor_principal_id",
        [requirement_id.into()],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut qualified = Vec::new();
    for row in rows_found {
        let actor: Uuid = row.try_get_by("actor_principal_id")?;
        if actor != requester_id && eligible_approver(db, actor, project_id, lock).await? {
            qualified.push(actor);
        }
    }
    Ok(qualified)
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT DISTINCT requirement.id, requirement.project_id, decision.actor_principal_id \
         FROM deployment_approval_requirements requirement \
         JOIN projects project ON project.id = requirement.project_id AND project.lifecycle_status = 'ACTIVE' \
         JOIN deployments deployment ON deployment.id = requirement.deployment_id \
         JOIN deployment_approval_decisions decision ON decision.approval_requirement_id = requirement.id \
           AND decision.decision = 'APPROVE' AND decision.actor_principal_id <> deployment.requested_by \
         WHERE requirement.id = ANY($1)",
        [uuid_array(&pending)],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut qualified_cache: HashMap<(Uuid, Uuid), bool> = HashMap::new();
    let mut qualifying_counts: HashMap<Uuid, i32> = HashMap::new();
    for row in rows_found {
        let requirement_id: Uuid = row.try_get_by("id")?;
        let project_id: Uuid = row.try_get_by("project_id")?;
        let actor_id: Uuid = row.try_get_by("actor_principal_id")?;
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM deployment_approval_decisions WHERE approval_requirement_id = $1 AND actor_principal_id = $2",
        [requirement_id.into(), actor.into()],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO deployment_approval_decisions (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, request_key, correlation_id, eligibility_checked_at, request_expected_revision, request_fingerprint) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, CURRENT_TIMESTAMP, $9, $10)",
        [
            id.into(),
            requirement_id.into(),
            actor.into(),
            value.into(),
            (if value == "REJECT" || blank(comment) {
                None
            } else {
                comment.map(str::trim)
            })
            .into(),
            (if value == "REJECT" {
                rejection_reason.map(str::trim)
            } else {
                None
            })
            .into(),
            request_id.into(),
            correlation_id.into(),
            expected_revision.into(),
            request_fingerprint.into(),
        ],
    );
    db.execute_raw(statement).await?;
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

fn requirement_state_problem(raw: &RawRequirement) -> DomainApprovalDecisionProblem {
    if raw.status == ApprovalRequirementStatus::Expired {
        return DomainApprovalDecisionProblem::of("APPROVAL_REQUIREMENT_EXPIRED");
    }
    if raw.status == ApprovalRequirementStatus::Invalidated {
        if let Some(code) = &raw.invalidation_code {
            if matches!(
                code.as_str(),
                "APPROVAL_EVIDENCE_MISSING"
                    | "APPROVAL_EVIDENCE_EXPIRED"
                    | "APPROVAL_EVIDENCE_MISMATCH"
            ) {
                return DomainApprovalDecisionProblem::of(code.clone());
            }
        }
    }
    DomainApprovalDecisionProblem::of("APPROVAL_REQUIREMENT_NOT_PENDING")
}

fn decision_refusal(problem: DomainApprovalDecisionProblem) -> ApprovalDecisionMutationResult {
    ApprovalDecisionMutationResult::refused(problem.into())
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
    decision_preview: Option<ApprovalDecisionPreview>,
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
        risk: deployment.policy.risk.clone(),
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
        decision_preview,
    }
}

#[allow(clippy::too_many_arguments)]
async fn approval_item(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    raw: &RawRequirement,
    deployment: &Deployment,
    qualifying: i32,
    eligibility: &mut HashMap<(Uuid, Uuid), bool>,
    evidence: Vec<DeploymentEvidence>,
    principals: &HashMap<Uuid, ApprovalPrincipal>,
    decision_preview: Option<ApprovalDecisionPreview>,
    prior_decision: Option<bool>,
) -> Result<ApprovalInboxItem, DbErr> {
    let requirement = requirement_from_raw(
        raw,
        deployment,
        qualifying,
        evidence,
        principals,
        decision_preview,
    );
    let eligible = eligible_approver_cached(db, principal_id, raw.project_id, eligibility).await?;
    let prior_decision = match prior_decision {
        Some(value) => value,
        None => has_decision(db, raw.id, principal_id).await?,
    };
    let available = eligible
        && requirement.required_distinct_approver_count > 0
        && requirement.status == ApprovalRequirementStatus::Pending
        && deployment.lifecycle_status == DeploymentLifecycleStatus::AwaitingApproval
        && principal_id != requirement.requester_id
        && !prior_decision;
    Ok(ApprovalInboxItem {
        requirement,
        deployment: deployment.clone(),
        eligible,
        decision_available: available,
    })
}

async fn approval_item_for_requirement(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    raw: &RawRequirement,
) -> Result<ApprovalInboxItem, DbErr> {
    let Some(deployment) = rows::deployments(db, &[raw.deployment_id], false)
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
    let mut eligibility = HashMap::new();
    approval_item(
        db,
        principal_id,
        raw,
        &deployment,
        qualifying,
        &mut eligibility,
        evidence,
        &principals,
        None,
        None,
    )
    .await
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
        None,
    ))
}

async fn has_approval_inbox_scope(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Option<Uuid>,
) -> Result<bool, DbErr> {
    if let Some(organization_id) = organization_id {
        let statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT 1 FROM organizations WHERE id = $1",
            [organization_id.into()],
        );
        if db.query_one_raw(statement).await?.is_none() {
            return Ok(false);
        }
    }
    let administrator = capability_queries::has_platform_admin(db, principal_id, false).await?;
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "WITH params AS (SELECT $1::uuid AS authority_principal, $2::uuid AS scoped_organization, $3 AS administrator), \
           candidate_projects AS ( \
             (SELECT scope.project_id \
             FROM deployment_approval_principal_project_scopes scope, params \
             WHERE scope.principal_id = params.authority_principal AND scope.valid_after <= CURRENT_TIMESTAMP \
             ORDER BY scope.project_id ASC LIMIT 1) \
             UNION ALL \
             (SELECT membership.project_id \
             FROM project_memberships membership \
             JOIN project_membership_roles role ON role.membership_id = membership.id \
             JOIN organization_memberships organization_membership \
               ON organization_membership.principal_id = membership.principal_id \
             JOIN projects project \
               ON project.id = membership.project_id AND project.organization_id = organization_membership.organization_id \
             , params \
             WHERE membership.principal_id = params.authority_principal \
               AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
               AND organization_membership.started_at <= CURRENT_TIMESTAMP AND organization_membership.ended_at IS NULL \
               AND role.role_code IN ('PROJECT_ADMIN', 'DEPLOYMENT_APPROVER', 'AUDITOR') \
               AND (params.scoped_organization IS NULL OR project.organization_id = params.scoped_organization) \
             ORDER BY membership.project_id ASC LIMIT 1) \
             UNION ALL \
             (SELECT project.id \
             FROM deployment_approval_principal_organization_scopes scope, params \
             CROSS JOIN LATERAL ( \
               SELECT candidate.id FROM projects candidate \
               WHERE candidate.organization_id = scope.organization_id ORDER BY candidate.id ASC LIMIT 1 \
             ) project \
             WHERE scope.principal_id = params.authority_principal AND scope.valid_after <= CURRENT_TIMESTAMP \
               AND (params.scoped_organization IS NULL OR scope.organization_id = params.scoped_organization) \
             ORDER BY project.id ASC LIMIT 1) \
             UNION ALL \
             (SELECT project.id \
             FROM organization_memberships membership \
             JOIN organization_membership_roles role ON role.membership_id = membership.id \
             , params \
             CROSS JOIN LATERAL ( \
               SELECT candidate.id FROM projects candidate \
               WHERE candidate.organization_id = membership.organization_id ORDER BY candidate.id ASC LIMIT 1 \
             ) project \
             WHERE membership.principal_id = params.authority_principal \
               AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
               AND role.role_code IN ('ORGANIZATION_ADMIN', 'AUDITOR') \
               AND (params.scoped_organization IS NULL OR membership.organization_id = params.scoped_organization) \
             ORDER BY project.id ASC LIMIT 1) \
             UNION ALL \
             (SELECT project.id \
             FROM projects project, params \
             WHERE params.administrator \
               AND (params.scoped_organization IS NULL OR project.organization_id = params.scoped_organization) \
             ORDER BY project.id ASC LIMIT 1) \
           ) \
         SELECT DISTINCT candidate.project_id \
         FROM candidate_projects candidate JOIN projects project ON project.id = candidate.project_id, params \
         WHERE params.scoped_organization IS NULL OR project.organization_id = params.scoped_organization",
        [principal_id.into(), organization_id.into(), administrator.into()],
    );
    let rows_found = db.query_all_raw(statement).await?;
    for row in rows_found {
        let project_id: Uuid = row.try_get_by("project_id")?;
        if tx::deployment_approval_capabilities(db, principal_id, project_id, false)
            .await?
            .contains(tx::DEPLOYMENT_APPROVAL_VIEW)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Java port of `deployment_approval_visible_requirements()`'s final redefinition (V034). See the
/// scoping report: the capability recheck applies once over the fetched union rather than per branch
/// — safe for the platform-admin and direct-membership branches (their own `WHERE` already re-derives
/// what the recheck checks) but the three scope-cache-backed branches fetch unbounded (limited only
/// by the outer application-level limit) so a stale-cache row can never crowd a viewable requirement
/// out of a per-branch window before the recheck runs.
#[allow(clippy::too_many_arguments)]
async fn approval_inbox_requirement_ids(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Option<Uuid>,
    project_id: Option<Uuid>,
    cursor: &Option<ApprovalCursor>,
    first: i32,
) -> Result<Vec<Uuid>, DbErr> {
    let administrator = capability_queries::has_platform_admin(db, principal_id, false).await?;
    let cursor_requested_at = cursor.as_ref().map(|value| value.requested_at);
    let cursor_id = cursor.as_ref().map(|value| value.id);
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "WITH limits AS ( \
           SELECT LEAST(GREATEST($1, 1), 51) AS rows \
         ), candidates AS ( \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM deployment_approval_requirements requirement \
            WHERE $2 \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6)) \
            ORDER BY requirement.requested_at DESC, requirement.id DESC LIMIT (SELECT rows FROM limits)) \
           UNION ALL \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM deployment_approval_principal_organization_scopes scope \
            JOIN deployment_approval_requirements requirement ON requirement.organization_id = scope.organization_id \
            WHERE scope.principal_id = $7 AND scope.valid_after <= CURRENT_TIMESTAMP \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6))) \
           UNION ALL \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM organization_memberships membership \
            JOIN organization_membership_roles role ON role.membership_id = membership.id \
            JOIN projects project ON project.organization_id = membership.organization_id \
            JOIN deployment_approval_requirements requirement ON requirement.project_id = project.id \
            WHERE membership.principal_id = $7 \
              AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
              AND role.role_code IN ('ORGANIZATION_ADMIN', 'AUDITOR') \
              AND NOT EXISTS (SELECT 1 FROM deployment_approval_principal_organization_scopes scope \
                              WHERE scope.principal_id = membership.principal_id AND scope.organization_id = membership.organization_id) \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6)) \
            ORDER BY requirement.requested_at DESC, requirement.id DESC LIMIT (SELECT rows FROM limits)) \
           UNION ALL \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM deployment_approval_principal_organization_membership_scopes membership_scope \
            JOIN deployment_approval_principal_project_scopes scope ON scope.principal_id = membership_scope.principal_id \
            JOIN deployment_approval_requirements requirement ON requirement.project_id = scope.project_id \
              AND requirement.organization_id = membership_scope.organization_id \
            WHERE membership_scope.principal_id = $7 AND scope.valid_after <= CURRENT_TIMESTAMP \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6))) \
           UNION ALL \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM project_memberships membership \
            JOIN project_membership_roles role ON role.membership_id = membership.id \
            JOIN projects project ON project.id = membership.project_id \
            JOIN organization_memberships organization_membership ON organization_membership.organization_id = project.organization_id \
              AND organization_membership.principal_id = membership.principal_id \
            JOIN deployment_approval_requirements requirement ON requirement.project_id = membership.project_id \
            WHERE membership.principal_id = $7 \
              AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
              AND organization_membership.started_at <= CURRENT_TIMESTAMP AND organization_membership.ended_at IS NULL \
              AND role.role_code IN ('PROJECT_ADMIN', 'DEPLOYMENT_APPROVER', 'AUDITOR') \
              AND NOT EXISTS (SELECT 1 FROM deployment_approval_principal_project_scopes scope \
                              WHERE scope.principal_id = membership.principal_id AND scope.project_id = membership.project_id) \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6)) \
            ORDER BY requirement.requested_at DESC, requirement.id DESC LIMIT (SELECT rows FROM limits)) \
           UNION ALL \
           (SELECT requirement.id, requirement.requested_at, requirement.project_id \
            FROM deployment_approval_principal_project_scopes scope \
            JOIN deployment_approval_requirements requirement ON requirement.project_id = scope.project_id \
            WHERE scope.principal_id = $7 AND scope.valid_after <= CURRENT_TIMESTAMP \
              AND ($3::uuid IS NULL OR requirement.organization_id = $3) \
              AND ($4::uuid IS NULL OR requirement.project_id = $4) \
              AND ($5::timestamptz IS NULL OR (requirement.requested_at, requirement.id) < ($5, $6))) \
         ) \
         SELECT DISTINCT id, requested_at, project_id FROM candidates",
        [
            (first + 1).into(),
            administrator.into(),
            organization_id.into(),
            project_id.into(),
            cursor_requested_at.into(),
            cursor_id.into(),
            principal_id.into(),
        ],
    );
    let rows_found = db.query_all_raw(statement).await?;

    let mut viewable_projects: HashMap<Uuid, bool> = HashMap::new();
    let mut viewable: Vec<(Uuid, chrono::DateTime<chrono::Utc>)> = Vec::new();
    for row in rows_found {
        let id: Uuid = row.try_get_by("id")?;
        let requested_at: chrono::DateTime<chrono::Utc> = row.try_get_by("requested_at")?;
        let project_id: Uuid = row.try_get_by("project_id")?;
        let viewable_project = match viewable_projects.get(&project_id) {
            Some(value) => *value,
            None => {
                let value =
                    tx::deployment_approval_capabilities(db, principal_id, project_id, false)
                        .await?
                        .contains(tx::DEPLOYMENT_APPROVAL_VIEW);
                viewable_projects.insert(project_id, value);
                value
            }
        };
        if viewable_project {
            viewable.push((id, requested_at));
        }
    }
    let mut deduped: HashMap<Uuid, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let mut order: Vec<Uuid> = Vec::new();
    for (id, requested_at) in viewable {
        if let std::collections::hash_map::Entry::Vacant(entry) = deduped.entry(id) {
            entry.insert(requested_at);
            order.push(id);
        }
    }
    order.sort_by(|a, b| deduped[b].cmp(&deduped[a]).then_with(|| b.cmp(a)));
    order.truncate((first + 1) as usize);
    Ok(order)
}

pub async fn approval_inbox(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Option<Uuid>,
    project_id: Option<Uuid>,
    after: Option<&str>,
    first: i32,
    include_decision_preview: bool,
) -> Result<Option<ApprovalInboxConnection>, DbErr> {
    if organization_id.is_some() && project_id.is_some() {
        return Ok(None);
    }
    if let Some(project_id) = project_id {
        if !can_approval_view(db, principal_id, project_id, false).await? {
            return Ok(None);
        }
    } else if !has_approval_inbox_scope(db, principal_id, organization_id).await? {
        return Ok(None);
    }
    let cursor = match cursors::decode_approval_cursor(after, organization_id, project_id) {
        Ok(cursor) => cursor,
        Err(()) => return Ok(None),
    };
    let mut ids = approval_inbox_requirement_ids(
        db,
        principal_id,
        organization_id,
        project_id,
        &cursor,
        first,
    )
    .await?;
    // Re-run the P-10 keyset query after the ordered authority locks. This establishes the read at
    // the same authority boundary as the loaded facts without post-filtering a page.
    let mut requirements = rows::raw_requirements(db, &ids, true).await?;
    let distinct_projects: Vec<Uuid> = requirements
        .values()
        .map(|value| value.project_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    for project in distinct_projects {
        tx::deployment_approval_capabilities(db, principal_id, project, true).await?;
    }
    ids = approval_inbox_requirement_ids(
        db,
        principal_id,
        organization_id,
        project_id,
        &cursor,
        first,
    )
    .await?;
    requirements = rows::raw_requirements(db, &ids, true).await?;

    let deployment_ids: Vec<Uuid> = requirements
        .values()
        .map(|value| value.deployment_id)
        .collect();
    let deployments_list = rows::deployments(db, &deployment_ids, false).await?;
    let deployments: HashMap<Uuid, Deployment> = deployments_list
        .into_iter()
        .map(|value| (value.id, value))
        .collect();
    let mut evidence = approval_evidence_for(db, &deployment_ids).await?;
    let requirement_values: Vec<RawRequirement> =
        requirements.values().map(clone_raw_requirement).collect();
    let qualifying = qualifying_approval_counts(db, &requirement_values).await?;
    let principals = approval_principals_for(db, &requirement_values).await?;
    let decision_previews = if include_decision_preview {
        approval_decision_previews(db, &ids).await?
    } else {
        HashMap::new()
    };
    let prior_decisions = approval_decision_requirement_ids(db, &ids, principal_id).await?;
    let mut eligibility = HashMap::new();

    let mut rows_out = Vec::new();
    let mut cursors_out = Vec::new();
    for id in &ids {
        let Some(raw) = requirements.remove(id) else {
            continue;
        };
        let Some(deployment) = deployments.get(&raw.deployment_id) else {
            continue;
        };
        let deployment_evidence = evidence.remove(&raw.deployment_id).unwrap_or_default();
        let item = approval_item(
            db,
            principal_id,
            &raw,
            deployment,
            *qualifying.get(&raw.id).unwrap_or(&0),
            &mut eligibility,
            deployment_evidence,
            &principals,
            decision_previews.get(&raw.id).cloned(),
            Some(prior_decisions.contains(&raw.id)),
        )
        .await?;
        cursors_out.push(cursors::encode_approval_cursor(
            organization_id,
            project_id,
            raw.requested_at,
            raw.id,
        ));
        rows_out.push(item);
    }
    let has_next = rows_out.len() > first as usize;
    if has_next {
        rows_out.pop();
        cursors_out.pop();
    }
    let end_cursor = cursors_out.last().cloned();
    Ok(Some(ApprovalInboxConnection {
        nodes: rows_out,
        cursors: cursors_out,
        end_cursor,
        has_next_page: has_next,
    }))
}

fn clone_raw_requirement(value: &RawRequirement) -> RawRequirement {
    RawRequirement {
        id: value.id,
        deployment_id: value.deployment_id,
        project_id: value.project_id,
        requester_id: value.requester_id,
        requested_at: value.requested_at,
        revision: value.revision,
        status: value.status,
        expires_at: value.expires_at,
        satisfied_at: value.satisfied_at,
        rejected_at: value.rejected_at,
        invalidated_at: value.invalidated_at,
        invalidation_code: value.invalidation_code.clone(),
        satisfied_participants: value.satisfied_participants.clone(),
        required_approvers: value.required_approvers,
    }
}

pub async fn approval_detail(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    approval_requirement_id: Uuid,
) -> Result<Option<ApprovalInboxItem>, DbErr> {
    let Some(initial) = rows::raw_requirement(db, approval_requirement_id, false).await? else {
        return Ok(None);
    };
    if rows::deployments(db, &[initial.deployment_id], false)
        .await?
        .is_empty()
    {
        return Ok(None);
    }
    if !can_approval_view(db, principal_id, initial.project_id, true).await? {
        return Ok(None);
    }
    let Some(raw) = rows::raw_requirement(db, approval_requirement_id, true).await? else {
        return Ok(None);
    };
    let raw = reconcile_requirement(db, raw).await?;
    Ok(Some(
        approval_item_for_requirement(db, principal_id, &raw).await?,
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

pub async fn approval_decisions(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    approval_requirement_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<ApprovalDecisionConnection>, DbErr> {
    if !(1..=50).contains(&first) {
        return Ok(None);
    }
    let Some(initial) = rows::raw_requirement(db, approval_requirement_id, false).await? else {
        return Ok(None);
    };
    if rows::deployments(db, &[initial.deployment_id], false)
        .await?
        .is_empty()
        || !can_approval_view(db, principal_id, initial.project_id, true).await?
    {
        return Ok(None);
    }
    let cursor = match cursors::decode_approval_decision_cursor(after) {
        Ok(cursor) => cursor,
        Err(()) => return Ok(None),
    };
    if let Some(cursor) = &cursor {
        if cursor.requirement_id != approval_requirement_id {
            return Ok(None);
        }
    }
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, decided_at \
         FROM deployment_approval_decisions \
         WHERE approval_requirement_id = $1 \
           AND ($2::timestamptz IS NULL OR (decided_at, id) > ($2, $3)) \
         ORDER BY decided_at ASC, id ASC LIMIT $4",
        [
            approval_requirement_id.into(),
            cursor.as_ref().map(|value| value.decided_at).into(),
            cursor.as_ref().map(|value| value.id).into(),
            (first + 1).into(),
        ],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values: Vec<ApprovalDecision> = rows_found
        .iter()
        .map(|row| rows::decision_row(row).map(approval_decision_from_row))
        .collect::<Result<Vec<_>, _>>()?;
    let has_next = values.len() > first as usize;
    if has_next {
        values.pop();
    }
    let cursors_list: Vec<String> = values
        .iter()
        .map(|value| {
            cursors::encode_approval_decision_cursor(
                approval_requirement_id,
                value.decided_at,
                value.id,
            )
        })
        .collect();
    let end_cursor = cursors_list.last().cloned();
    Ok(Some(ApprovalDecisionConnection {
        nodes: values,
        cursors: cursors_list,
        end_cursor,
        has_next_page: has_next,
    }))
}

async fn decision_by_id(
    db: &impl ConnectionTrait,
    id: Uuid,
) -> Result<Option<ApprovalDecision>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, decided_at FROM deployment_approval_decisions WHERE id = $1",
        [id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(approval_decision_from_row(rows::decision_row(&row)?))),
        None => Ok(None),
    }
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, decided_at, request_fingerprint \
         FROM deployment_approval_decisions WHERE approval_requirement_id = $1 AND actor_principal_id = $2 AND request_key = $3",
        [requirement_id.into(), actor.into(), request_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(ReplayDecision {
            decision: approval_decision_from_row(rows::decision_row(&row)?),
            request_fingerprint: row.try_get_by("request_fingerprint")?,
        })),
        None => Ok(None),
    }
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
        return Ok(decision_refusal(
            DomainApprovalDecisionProblem::unavailable(),
        ));
    };
    if rows::deployments(db, &[initial.deployment_id], false)
        .await?
        .is_empty()
    {
        return Ok(decision_refusal(
            DomainApprovalDecisionProblem::unavailable(),
        ));
    }
    let Some(raw) = rows::raw_requirement(db, command.requirement_id, true).await? else {
        return Ok(decision_refusal(
            DomainApprovalDecisionProblem::unavailable(),
        ));
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
                .unwrap_or_else(DomainApprovalDecisionProblem::unavailable),
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
            return Ok(decision_refusal(DomainApprovalDecisionProblem::of(
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
        // Ports `auditApprovalReplay`: one durable transport-recovery fact per immutable decision
        // and request; a duplicate retry finds its receipt and leaves the projection unchanged.
        let receipt_statement = Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO deployment_approval_replay_receipts (decision_id, request_id) \
             VALUES ($1, $2) ON CONFLICT DO NOTHING",
            [replay.decision.id.into(), command.request_id.into()],
        );
        let receipt = db.execute_raw(receipt_statement).await?;
        if receipt.rows_affected() > 0 {
            super::writes::audit(
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
            _ => Ok(decision_refusal(
                DomainApprovalDecisionProblem::unavailable(),
            )),
        };
    }
    if crate::deployment::approval::deployment_archive_boundary(db, raw.deployment_id).await? {
        return Ok(decision_refusal(DomainApprovalDecisionProblem::of(
            "PROJECT_ARCHIVED",
        )));
    }
    let eligible = eligible_approver(db, command.principal_id, raw.project_id, true).await?;
    if !eligible {
        return Ok(decision_refusal(DomainApprovalDecisionProblem::of(
            "APPROVER_INELIGIBLE",
        )));
    }
    let expired = crate::deployment::approval::requirement_expired(db, raw.id).await?;
    let evidence_issue =
        crate::deployment::approval::approval_evidence_issue(db, raw.deployment_id).await?;
    let waiting_for_evaluation = evidence_issue.as_deref() == Some("APPROVAL_EVIDENCE_MISSING")
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
        evidence_issue: evidence_issue.clone(),
        waiting_for_evaluation,
    };
    let duplicate = has_decision(db, command.requirement_id, command.principal_id).await?;
    let plan = planner.plan(command, &facts);
    if !plan.accepted() {
        return Ok(decision_refusal(
            plan.problem
                .unwrap_or_else(DomainApprovalDecisionProblem::unavailable),
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
                .unwrap_or_else(DomainApprovalDecisionProblem::unavailable),
        ));
    }
    if !plan.accepted() {
        return Ok(decision_refusal(
            plan.problem
                .unwrap_or_else(DomainApprovalDecisionProblem::unavailable),
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
    super::writes::audit(
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
        super::writes::audit(
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
        super::writes::audit(
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
        _ => Ok(decision_refusal(
            DomainApprovalDecisionProblem::unavailable(),
        )),
    }
}
