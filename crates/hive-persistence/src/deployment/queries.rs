//! Ports the read-only `DeploymentRepository` methods that are still repository methods:
//! compile-context resolution (Group B) and the approval inbox/decision/requirement surface
//! (Group D). The list/find/timeline/detail/environments reads (Group C) are deleted: they are
//! generated entity queries now (`docs/idiomatic-seaography-plan.md`, A2), with the nested
//! structures answered by relations and by `super::computed`.

use super::cursors::{self, ApprovalCursor};
use super::rows::{self, ApprovalDecisionRow, RawRequirement};
use crate::capability::{queries as capability_queries, tx};
use crate::entity::enums::{
    ApprovalDecision as EntityApprovalDecision, DeploymentLifecycleStatus as EntityLifecycleStatus,
    LifecycleStatus, OrganizationRoleCode, ProjectRoleCode,
};
use crate::entity::{
    agent_versions, agents, deployment_approval_decisions,
    deployment_approval_principal_organization_membership_scopes,
    deployment_approval_principal_organization_scopes,
    deployment_approval_principal_project_scopes, deployment_approval_replay_receipts,
    deployment_approval_requirements, deployment_evidence_invalidations,
    deployment_evidence_snapshots, deployment_plan_versions, deployment_policy_snapshots,
    deployments, environment_definition_versions, organization_membership_roles,
    organization_memberships, organizations, principals, project_approval_policies,
    project_approval_policy_versions, project_membership_roles, project_memberships, projects,
};
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
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::sea_query::{
    Expr, ExprTrait, IntoTableRef, LockType, OnConflict, Query, SelectStatement, TableRef,
};
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    FromQueryResult, JoinType, NotSet, QueryFilter, QueryOrder, QuerySelect, RelationTrait, Set,
    TransactionTrait, TryInsertResult,
};
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

/// `select`, locking the rows it reads from `tables` in `mode` when `lock` is set.
fn locking_tables<E: EntityTrait>(
    mut select: sea_orm::Select<E>,
    lock: bool,
    mode: LockType,
    tables: impl IntoIterator<Item = TableRef>,
) -> sea_orm::Select<E> {
    if lock {
        QuerySelect::query(&mut select).lock_with_tables(mode, tables);
    }
    select
}

#[derive(FromQueryResult)]
struct VersionSourceRow {
    version_id: Uuid,
    project_id: Uuid,
    agent_id: Uuid,
    display_name: String,
    version_number: i64,
    content_digest: String,
    catalog_release_id: String,
    catalog_release_digest: String,
    organization_id: Uuid,
    canonical_document: serde_json::Value,
}

/// The published version with its agent and project, locked `FOR KEY SHARE OF version, agent,
/// project` when `lock` is set.
pub(super) async fn version_source(
    db: &impl ConnectionTrait,
    version_id: Uuid,
    lock: bool,
) -> Result<Option<VersionSource>, DbErr> {
    let select = agent_versions::Entity::find()
        .join(JoinType::InnerJoin, agent_versions::Relation::Agents.def())
        .join(JoinType::InnerJoin, agents::Relation::Projects.def())
        .filter(agent_versions::Column::Id.eq(version_id))
        .select_only()
        .column_as(agent_versions::Column::Id, "version_id")
        .column_as(projects::Column::Id, "project_id")
        .column_as(agents::Column::Id, "agent_id")
        .column(agents::Column::DisplayName)
        .column(agent_versions::Column::VersionNumber)
        .column(agent_versions::Column::ContentDigest)
        .column(agent_versions::Column::CatalogReleaseId)
        .column(agent_versions::Column::CatalogReleaseDigest)
        .column(projects::Column::OrganizationId)
        .column(agent_versions::Column::CanonicalDocument);
    let row = locking_tables(
        select,
        lock,
        LockType::KeyShare,
        [
            agent_versions::Entity.into_table_ref(),
            agents::Entity.into_table_ref(),
            projects::Entity.into_table_ref(),
        ],
    )
    .into_model::<VersionSourceRow>()
    .one(db)
    .await?;
    Ok(row.map(|row| VersionSource {
        id: row.version_id,
        project_id: row.project_id,
        agent_id: row.agent_id,
        agent_display_name: row.display_name,
        version_number: row.version_number,
        content_digest: row.content_digest,
        catalog_release_id: row.catalog_release_id,
        catalog_release_digest: row.catalog_release_digest,
        organization_id: row.organization_id,
        canonical_document: row.canonical_document.to_string(),
    }))
}

/// The environment definition version, pinned to the version's own catalog release.
pub(super) async fn environment(
    db: &impl ConnectionTrait,
    environment_id: Uuid,
    release_id: &str,
) -> Result<Option<EnvironmentDefinition>, DbErr> {
    Ok(
        environment_definition_versions::Entity::find_by_id(environment_id)
            .filter(environment_definition_versions::Column::CatalogReleaseId.eq(release_id))
            .one(db)
            .await?
            .map(|row| EnvironmentDefinition {
                id: row.id,
                stable_definition_id: row.stable_definition_id,
                version: row.version,
                display_name: row.display_name,
                logical_environment_class: row.logical_environment_class.to_value(),
                catalog_release_id: row.catalog_release_id,
                catalog_release_digest: row.catalog_release_digest,
                content_digest: row.content_digest,
            }),
    )
}

#[derive(FromQueryResult)]
struct PolicySourceRow {
    id: Uuid,
    revision: i64,
    digest: String,
    matrix: serde_json::Value,
}

/// The project's approval policy at its current revision, locked `FOR SHARE OF policy, version`
/// when `lock` is set.
pub(super) async fn policy(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    lock: bool,
) -> Result<Option<PolicySource>, DbErr> {
    let current_version: sea_orm::RelationDef =
        project_approval_policies::Entity::has_many(project_approval_policy_versions::Entity)
            .from(project_approval_policies::Column::Id)
            .to(project_approval_policy_versions::Column::PolicyId)
            .on_condition(|policy, version| {
                Condition::all().add(
                    Expr::col((version, project_approval_policy_versions::Column::Revision)).eq(
                        Expr::col((policy, project_approval_policies::Column::CurrentRevision)),
                    ),
                )
            })
            .into();
    let select = project_approval_policies::Entity::find()
        .join(JoinType::InnerJoin, current_version)
        .filter(project_approval_policies::Column::ProjectId.eq(project_id))
        .select_only()
        .column(project_approval_policies::Column::Id)
        .column(project_approval_policy_versions::Column::Revision)
        .column(project_approval_policy_versions::Column::Digest)
        .column(project_approval_policy_versions::Column::Matrix);
    let row = locking_tables(
        select,
        lock,
        LockType::Share,
        [
            project_approval_policies::Entity.into_table_ref(),
            project_approval_policy_versions::Entity.into_table_ref(),
        ],
    )
    .into_model::<PolicySourceRow>()
    .one(db)
    .await?;
    Ok(row.map(|row| PolicySource {
        id: row.id,
        revision: row.revision,
        digest: row.digest,
        matrix: row.matrix.to_string(),
    }))
}

#[derive(FromQueryResult)]
struct ActiveTargetRow {
    target_digest: Option<String>,
    canonical_document: serde_json::Value,
    id: Uuid,
    agent_version_id: Uuid,
    version_number: i64,
    requested_at: DateTimeWithTimeZone,
}

/// The newest `ACTIVE` deployment of this agent in this environment, locked
/// `FOR SHARE OF deployment` when `lock` is set.
pub(super) async fn active_target(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    agent_id: Uuid,
    environment_id: Uuid,
    lock: bool,
) -> Result<Option<ActiveTarget>, DbErr> {
    let select = deployments::Entity::find()
        .join(
            JoinType::InnerJoin,
            deployments::Relation::DeploymentPlanVersions
                .def()
                .on_condition(|_left, right| {
                    Condition::all().add(
                        Expr::col((right, deployment_plan_versions::Column::VersionNumber))
                            .eq(1_i64),
                    )
                }),
        )
        .join(
            JoinType::InnerJoin,
            deployments::Relation::AgentVersions.def(),
        )
        .filter(deployments::Column::ProjectId.eq(project_id))
        .filter(deployments::Column::AgentId.eq(agent_id))
        .filter(deployments::Column::EnvironmentDefinitionVersionId.eq(environment_id))
        .filter(deployments::Column::LifecycleStatus.eq(EntityLifecycleStatus::Active))
        .order_by_desc(deployments::Column::UpdatedAt)
        .order_by_desc(deployments::Column::Id)
        .limit(1)
        .select_only()
        .column(deployment_plan_versions::Column::TargetDigest)
        .column(agent_versions::Column::CanonicalDocument)
        .column(deployments::Column::Id)
        .column(deployments::Column::AgentVersionId)
        .column(agent_versions::Column::VersionNumber)
        .column(deployments::Column::RequestedAt);
    let row = locking_tables(
        select,
        lock,
        LockType::Share,
        [deployments::Entity.into_table_ref()],
    )
    .into_model::<ActiveTargetRow>()
    .one(db)
    .await?;
    Ok(row.map(|row| ActiveTarget {
        target_digest: row.target_digest.unwrap_or_default(),
        canonical_document: row.canonical_document.to_string(),
        deployment_id: row.id,
        agent_version_id: row.agent_version_id,
        agent_version_number: row.version_number,
        requested_at: row.requested_at.to_utc(),
    }))
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

pub(super) fn canonical_target_version(value: Option<&str>) -> Option<String> {
    let value = value?;
    match Uuid::parse_str(value) {
        Ok(id) => Some(id.to_string()),
        Err(_) => Some(value.trim().to_string()),
    }
}

/// The version a rollback would return to: the newest prior `ACTIVE` deployment of this agent in
/// this environment that has observed runtime health, optionally pinned to a requested version.
pub(super) async fn rollback_target_version(
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
    // `(deployment.requested_at, deployment.id) < ($5, $6)`.
    let before = Condition::any()
        .add(deployments::Column::RequestedAt.lt(source.requested_at))
        .add(
            Condition::all()
                .add(deployments::Column::RequestedAt.eq(source.requested_at))
                .add(deployments::Column::Id.lt(source.id)),
        );
    let mut select = deployments::Entity::find()
        .join(
            JoinType::InnerJoin,
            deployments::Relation::DeploymentRuntimeHealth.def(),
        )
        .filter(deployments::Column::ProjectId.eq(source.project_id))
        .filter(deployments::Column::AgentId.eq(source.agent_id))
        .filter(deployments::Column::EnvironmentDefinitionVersionId.eq(source.environment.id))
        .filter(deployments::Column::LifecycleStatus.eq(EntityLifecycleStatus::Active))
        .filter(deployments::Column::Id.ne(source.id))
        .filter(before);
    if let Some(requested_id) = requested_id {
        select = select.filter(deployments::Column::AgentVersionId.eq(requested_id));
    }
    select
        .order_by_desc(deployments::Column::RequestedAt)
        .order_by_desc(deployments::Column::Id)
        .select_only()
        .column(deployments::Column::AgentVersionId)
        .into_tuple::<Uuid>()
        .one(db)
        .await
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
    // The deleted statement expanded `policy.required_evidence` with `CROSS JOIN LATERAL
    // jsonb_array_elements_text(...)` and left-joined each kind to its snapshot. The same three
    // tables are read as three entity queries and the expansion, the left join and the six-way
    // `CASE` run in Rust. Each of them is a read outside any transaction, exactly as the deleted
    // statement was, so no lock is dropped and no guard weakens.
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
                    state: "MISSING".to_string(),
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

async fn approval_decision_previews(
    db: &impl ConnectionTrait,
    requirement_ids: &[Uuid],
) -> Result<HashMap<Uuid, ApprovalDecisionPreview>, DbErr> {
    if requirement_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut by_requirement: HashMap<Uuid, Vec<ApprovalDecisionRow>> =
        requirement_ids.iter().map(|id| (*id, Vec::new())).collect();
    // The deleted statement was one batched top-51-per-requirement
    // (`unnest($1) CROSS JOIN LATERAL (... LIMIT 51)`); sea-query has no lateral builder, so each
    // requirement's own bounded page is one ordered query. It is a preview read outside any
    // transaction that held no lock, and `approvalInbox` always asks for it with
    // `include_decision_preview: false`, so this loop does not run on the console's path.
    for requirement_id in requirement_ids {
        let page = deployment_approval_decisions::Entity::find()
            .filter(
                deployment_approval_decisions::Column::ApprovalRequirementId.eq(*requirement_id),
            )
            .order_by_asc(deployment_approval_decisions::Column::DecidedAt)
            .order_by_asc(deployment_approval_decisions::Column::Id)
            .limit(51)
            .all(db)
            .await?;
        for row in page {
            let decision = rows::decision_row(row);
            by_requirement
                .entry(decision.requirement_id)
                .or_default()
                .push(decision);
        }
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
    Ok(deployment_approval_decisions::Entity::find()
        .filter(deployment_approval_decisions::Column::ActorPrincipalId.eq(principal_id))
        .filter(
            deployment_approval_decisions::Column::ApprovalRequirementId
                .is_in(requirement_ids.to_vec()),
        )
        .select_only()
        .column(deployment_approval_decisions::Column::ApprovalRequirementId)
        .distinct()
        .into_tuple::<Uuid>()
        .all(db)
        .await?
        .into_iter()
        .collect())
}

/// Whether the project is active, with its row locked `FOR SHARE` when `lock` is set.
pub(super) async fn active_project_check(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let select = projects::Entity::find_by_id(project_id)
        .filter(projects::Column::LifecycleStatus.eq(LifecycleStatus::Active))
        .select_only()
        .column(projects::Column::Id);
    Ok(locking_tables(
        select,
        lock,
        LockType::Share,
        [projects::Entity.into_table_ref()],
    )
    .into_tuple::<Uuid>()
    .one(db)
    .await?
    .is_some())
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
        // `eligibility_checked_at` has no column default; the deleted statement bound
        // `CURRENT_TIMESTAMP` for it, and `decided_at`'s own default is the same instant.
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

/// `<table>.started_at <= CURRENT_TIMESTAMP AND <table>.ended_at IS NULL`, on a named table.
fn active_membership<E, C>(entity: E, started_at: C, ended_at: C) -> Condition
where
    E: EntityTrait + Copy,
    C: ColumnTrait,
{
    Condition::all()
        .add(Expr::col((entity, started_at)).lte(Expr::current_timestamp()))
        .add(Expr::col((entity, ended_at)).is_null())
}

/// `SELECT project_id FROM deployment_approval_principal_project_scopes WHERE principal_id = $1
/// AND valid_after <= CURRENT_TIMESTAMP`. Kept as a subquery rather than a fetched set: a
/// principal's approval project-scope cache is unbounded (the access suite drives it to 100,000
/// rows), and binding one parameter per row would exceed the protocol's parameter limit.
fn project_scope_projects(principal_id: Uuid) -> SelectStatement {
    use deployment_approval_principal_project_scopes::{Column, Entity};
    Query::select()
        .column((Entity, Column::ProjectId))
        .from(Entity)
        .and_where(Expr::col((Entity, Column::PrincipalId)).eq(principal_id))
        .and_where(Expr::col((Entity, Column::ValidAfter)).lte(Expr::current_timestamp()))
        .to_owned()
}

/// `SELECT organization_id FROM deployment_approval_principal_organization_scopes WHERE
/// principal_id = $1 AND valid_after <= CURRENT_TIMESTAMP [AND organization_id = $2]`.
fn organization_scope_organizations(
    principal_id: Uuid,
    organization_id: Option<Uuid>,
) -> SelectStatement {
    use deployment_approval_principal_organization_scopes::{Column, Entity};
    let mut select = Query::select();
    select
        .column((Entity, Column::OrganizationId))
        .from(Entity)
        .and_where(Expr::col((Entity, Column::PrincipalId)).eq(principal_id))
        .and_where(Expr::col((Entity, Column::ValidAfter)).lte(Expr::current_timestamp()));
    if let Some(organization_id) = organization_id {
        select.and_where(Expr::col((Entity, Column::OrganizationId)).eq(organization_id));
    }
    select.to_owned()
}

/// `SELECT organization_id FROM deployment_approval_principal_organization_membership_scopes
/// WHERE principal_id = $1`.
fn membership_scope_organizations(principal_id: Uuid) -> SelectStatement {
    use deployment_approval_principal_organization_membership_scopes::{Column, Entity};
    Query::select()
        .column((Entity, Column::OrganizationId))
        .from(Entity)
        .and_where(Expr::col((Entity, Column::PrincipalId)).eq(principal_id))
        .to_owned()
}

/// `SELECT organization_id FROM organization_memberships JOIN organization_membership_roles ...`:
/// the organizations the principal holds one of `roles` in on an active membership. With
/// `exclude_scoped` the deleted branch's `NOT EXISTS (organization scope for this principal and
/// organization)` anti-join is kept as a `NOT IN` over the same subquery.
fn organization_role_organizations(
    principal_id: Uuid,
    roles: [OrganizationRoleCode; 2],
    organization_id: Option<Uuid>,
    exclude_scoped: bool,
) -> SelectStatement {
    use organization_memberships::{Column, Entity};
    let mut select = Query::select();
    select
        .column((Entity, Column::OrganizationId))
        .from(Entity)
        .inner_join(
            organization_membership_roles::Entity,
            Expr::col((
                organization_membership_roles::Entity,
                organization_membership_roles::Column::MembershipId,
            ))
            .eq(Expr::col((Entity, Column::Id))),
        )
        .and_where(Expr::col((Entity, Column::PrincipalId)).eq(principal_id))
        .cond_where(active_membership(
            Entity,
            Column::StartedAt,
            Column::EndedAt,
        ))
        .and_where(
            Expr::col((
                organization_membership_roles::Entity,
                organization_membership_roles::Column::RoleCode,
            ))
            .is_in(roles.map(|role| role.to_value())),
        );
    if let Some(organization_id) = organization_id {
        select.and_where(Expr::col((Entity, Column::OrganizationId)).eq(organization_id));
    }
    if exclude_scoped {
        select.and_where(
            Expr::col((Entity, Column::OrganizationId))
                .not_in_subquery(organization_scope_organizations(principal_id, None)),
        );
    }
    select.to_owned()
}

/// `SELECT project_id FROM project_memberships JOIN project_membership_roles JOIN projects JOIN
/// organization_memberships ...`: the projects the principal holds one of the three
/// approval-facing project roles in, on an active project membership whose organization membership
/// is also active. With `exclude_scoped` the deleted branch's `NOT EXISTS (project scope for this
/// principal and project)` anti-join is kept as a `NOT IN` over the same subquery.
fn project_role_projects(
    principal_id: Uuid,
    organization_id: Option<Uuid>,
    exclude_scoped: bool,
) -> SelectStatement {
    use project_memberships::{Column, Entity};
    let mut select = Query::select();
    select
        .column((Entity, Column::ProjectId))
        .from(Entity)
        .inner_join(
            project_membership_roles::Entity,
            Expr::col((
                project_membership_roles::Entity,
                project_membership_roles::Column::MembershipId,
            ))
            .eq(Expr::col((Entity, Column::Id))),
        )
        .inner_join(
            organization_memberships::Entity,
            Expr::col((
                organization_memberships::Entity,
                organization_memberships::Column::PrincipalId,
            ))
            .eq(Expr::col((Entity, Column::PrincipalId))),
        )
        .inner_join(
            projects::Entity,
            Condition::all()
                .add(
                    Expr::col((projects::Entity, projects::Column::Id))
                        .eq(Expr::col((Entity, Column::ProjectId))),
                )
                .add(
                    Expr::col((projects::Entity, projects::Column::OrganizationId)).eq(Expr::col(
                        (
                            organization_memberships::Entity,
                            organization_memberships::Column::OrganizationId,
                        ),
                    )),
                ),
        )
        .and_where(Expr::col((Entity, Column::PrincipalId)).eq(principal_id))
        .cond_where(active_membership(
            Entity,
            Column::StartedAt,
            Column::EndedAt,
        ))
        .cond_where(active_membership(
            organization_memberships::Entity,
            organization_memberships::Column::StartedAt,
            organization_memberships::Column::EndedAt,
        ))
        .and_where(
            Expr::col((
                project_membership_roles::Entity,
                project_membership_roles::Column::RoleCode,
            ))
            .is_in([
                ProjectRoleCode::ProjectAdmin.to_value(),
                ProjectRoleCode::DeploymentApprover.to_value(),
                ProjectRoleCode::Auditor.to_value(),
            ]),
        );
    if let Some(organization_id) = organization_id {
        select.and_where(
            Expr::col((projects::Entity, projects::Column::OrganizationId)).eq(organization_id),
        );
    }
    if exclude_scoped {
        select.and_where(
            Expr::col((Entity, Column::ProjectId))
                .not_in_subquery(project_scope_projects(principal_id)),
        );
    }
    select.to_owned()
}

/// `SELECT id FROM projects WHERE organization_id IN (<organizations>)`, the branch subquery that
/// replaces the deleted `JOIN projects project ON project.organization_id = membership.organization_id`.
fn projects_of(organizations: SelectStatement) -> SelectStatement {
    Query::select()
        .column(projects::Column::Id)
        .from(projects::Entity)
        .and_where(Expr::col(projects::Column::OrganizationId).in_subquery(organizations))
        .to_owned()
}

/// The lowest project identifier in any project the `organizations` subquery names — the deleted
/// branches' `CROSS JOIN LATERAL (SELECT id FROM projects WHERE organization_id =
/// scope.organization_id ORDER BY id LIMIT 1) ... ORDER BY project.id LIMIT 1`, which is the
/// minimum over the whole set.
async fn lowest_project_of(
    db: &impl ConnectionTrait,
    organizations: SelectStatement,
) -> Result<Option<Uuid>, DbErr> {
    projects::Entity::find()
        .filter(projects::Column::OrganizationId.in_subquery(organizations))
        .order_by_asc(projects::Column::Id)
        .select_only()
        .column(projects::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await
}

/// Ports `deployment_approval_has_inbox_scope()`. The deleted statement's `params` CTE
/// comma-cross-joined into five `ORDER BY/LIMIT 1` branches is five ordered single-row entity
/// queries whose results are unioned in Rust; the two `CROSS JOIN LATERAL` "first project of this
/// organization" subqueries are the minimum project identifier over the branch's organizations,
/// which is what the outer `ORDER BY project.id LIMIT 1` selected. Every read here is outside any
/// transaction and took no lock before.
async fn has_approval_inbox_scope(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Option<Uuid>,
) -> Result<bool, DbErr> {
    if let Some(organization_id) = organization_id {
        if organizations::Entity::find_by_id(organization_id)
            .one(db)
            .await?
            .is_none()
        {
            return Ok(false);
        }
    }
    let administrator = capability_queries::has_platform_admin(db, principal_id, false).await?;
    let mut candidates: Vec<Uuid> = Vec::new();
    // Branch 1: the project-scope cache. It carries no organization filter of its own; the outer
    // `JOIN projects ... WHERE scoped_organization IS NULL OR project.organization_id = ...` below
    // is what narrows it.
    candidates.extend(
        deployment_approval_principal_project_scopes::Entity::find()
            .filter(
                deployment_approval_principal_project_scopes::Column::PrincipalId.eq(principal_id),
            )
            .filter(
                Expr::col(deployment_approval_principal_project_scopes::Column::ValidAfter)
                    .lte(Expr::current_timestamp()),
            )
            .order_by_asc(deployment_approval_principal_project_scopes::Column::ProjectId)
            .select_only()
            .column(deployment_approval_principal_project_scopes::Column::ProjectId)
            .into_tuple::<Uuid>()
            .one(db)
            .await?,
    );
    // Branch 2: a direct project membership in one of the three approval-facing roles.
    candidates.extend(
        project_memberships::Entity::find()
            .filter(
                project_memberships::Column::ProjectId.in_subquery(project_role_projects(
                    principal_id,
                    organization_id,
                    false,
                )),
            )
            .order_by_asc(project_memberships::Column::ProjectId)
            .select_only()
            .column(project_memberships::Column::ProjectId)
            .into_tuple::<Uuid>()
            .one(db)
            .await?,
    );
    // Branch 3: the organization-scope cache.
    candidates.extend(
        lowest_project_of(
            db,
            organization_scope_organizations(principal_id, organization_id),
        )
        .await?,
    );
    // Branch 4: an organization admin or auditor membership.
    candidates.extend(
        lowest_project_of(
            db,
            organization_role_organizations(
                principal_id,
                [
                    OrganizationRoleCode::OrganizationAdmin,
                    OrganizationRoleCode::Auditor,
                ],
                organization_id,
                false,
            ),
        )
        .await?,
    );
    // Branch 5: a platform administrator.
    if administrator {
        let mut select = projects::Entity::find();
        if let Some(organization_id) = organization_id {
            select = select.filter(projects::Column::OrganizationId.eq(organization_id));
        }
        candidates.extend(
            select
                .order_by_asc(projects::Column::Id)
                .select_only()
                .column(projects::Column::Id)
                .into_tuple::<Uuid>()
                .one(db)
                .await?,
        );
    }
    // The outer `SELECT DISTINCT ... JOIN projects ... WHERE scoped_organization IS NULL OR
    // project.organization_id = scoped_organization`.
    let mut seen: HashSet<Uuid> = HashSet::new();
    candidates.retain(|project_id| seen.insert(*project_id));
    if candidates.is_empty() {
        return Ok(false);
    }
    let mut visible = projects::Entity::find().filter(projects::Column::Id.is_in(candidates));
    if let Some(organization_id) = organization_id {
        visible = visible.filter(projects::Column::OrganizationId.eq(organization_id));
    }
    let project_ids = visible
        .select_only()
        .column(projects::Column::Id)
        .into_tuple::<Uuid>()
        .all(db)
        .await?;
    for project_id in project_ids {
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
    // `LEAST(GREATEST($1, 1), 51)`, on the value this process already holds.
    let rows = u64::try_from((first + 1).clamp(1, 51)).unwrap_or(1);
    // The six branches' shared filters: the optional organization and project narrowing and the
    // `(requested_at, id) < (cursor)` tuple keyset, as a tuple comparison over the two non-null
    // key columns.
    let shared = || {
        use deployment_approval_requirements::Column;
        let mut condition = Condition::all();
        if let Some(organization_id) = organization_id {
            condition = condition.add(Column::OrganizationId.eq(organization_id));
        }
        if let Some(project_id) = project_id {
            condition = condition.add(Column::ProjectId.eq(project_id));
        }
        if let Some(cursor) = cursor.as_ref() {
            condition = condition.add(
                Expr::tuple([Expr::col(Column::RequestedAt), Expr::col(Column::Id)]).lt(
                    Expr::tuple([Expr::value(cursor.requested_at), Expr::value(cursor.id)]),
                ),
            );
        }
        condition
    };
    let keyed = |select: sea_orm::Select<deployment_approval_requirements::Entity>| {
        use deployment_approval_requirements::Column;
        select
            .filter(shared())
            .select_only()
            .column(Column::Id)
            .column(Column::RequestedAt)
            .column(Column::ProjectId)
    };
    let ordered = |select: sea_orm::Select<deployment_approval_requirements::Entity>| {
        use deployment_approval_requirements::Column;
        keyed(select)
            .order_by_desc(Column::RequestedAt)
            .order_by_desc(Column::Id)
            .limit(rows)
    };
    type Candidate = (Uuid, DateTimeWithTimeZone, Uuid);
    let mut candidates: Vec<Candidate> = Vec::new();

    // Branch 1, `WHERE $2` — a bare boolean parameter: the branch contributes nothing at all
    // unless the principal is a platform administrator.
    if administrator {
        candidates.extend(
            ordered(deployment_approval_requirements::Entity::find())
                .into_tuple::<Candidate>()
                .all(db)
                .await?,
        );
    }
    // Branch 2: the organization-scope cache, unbounded, as the deleted branch was.
    candidates.extend(
        keyed(
            deployment_approval_requirements::Entity::find().filter(
                deployment_approval_requirements::Column::OrganizationId
                    .in_subquery(organization_scope_organizations(principal_id, None)),
            ),
        )
        .into_tuple::<Candidate>()
        .all(db)
        .await?,
    );
    // Branch 3: an organization admin or auditor membership whose organization the scope cache
    // does *not* already cover (the `NOT EXISTS` anti-join), bounded.
    candidates.extend(
        ordered(deployment_approval_requirements::Entity::find().filter(
            deployment_approval_requirements::Column::ProjectId.in_subquery(projects_of(
                organization_role_organizations(
                    principal_id,
                    [
                        OrganizationRoleCode::OrganizationAdmin,
                        OrganizationRoleCode::Auditor,
                    ],
                    None,
                    true,
                ),
            )),
        ))
        .into_tuple::<Candidate>()
        .all(db)
        .await?,
    );
    // Branch 4: the organization-membership scope cache crossed with the project scope cache,
    // unbounded.
    candidates.extend(
        keyed(
            deployment_approval_requirements::Entity::find()
                .filter(
                    deployment_approval_requirements::Column::ProjectId
                        .in_subquery(project_scope_projects(principal_id)),
                )
                .filter(
                    deployment_approval_requirements::Column::OrganizationId
                        .in_subquery(membership_scope_organizations(principal_id)),
                ),
        )
        .into_tuple::<Candidate>()
        .all(db)
        .await?,
    );
    // Branch 5: a direct project membership in one of the three approval-facing roles whose
    // project the scope cache does *not* already cover (the second `NOT EXISTS`), bounded.
    candidates.extend(
        ordered(deployment_approval_requirements::Entity::find().filter(
            deployment_approval_requirements::Column::ProjectId.in_subquery(project_role_projects(
                principal_id,
                None,
                true,
            )),
        ))
        .into_tuple::<Candidate>()
        .all(db)
        .await?,
    );
    // Branch 6: the project-scope cache, unbounded.
    candidates.extend(
        keyed(
            deployment_approval_requirements::Entity::find().filter(
                deployment_approval_requirements::Column::ProjectId
                    .in_subquery(project_scope_projects(principal_id)),
            ),
        )
        .into_tuple::<Candidate>()
        .all(db)
        .await?,
    );

    let mut viewable_projects: HashMap<Uuid, bool> = HashMap::new();
    let mut viewable: Vec<(Uuid, chrono::DateTime<chrono::Utc>)> = Vec::new();
    for (id, requested_at, project_id) in candidates {
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
            viewable.push((id, requested_at.to_utc()));
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
    let mut select = deployment_approval_decisions::Entity::find().filter(
        deployment_approval_decisions::Column::ApprovalRequirementId.eq(approval_requirement_id),
    );
    if let Some(cursor) = cursor.as_ref() {
        // `(decided_at, id) > ($2, $3)`.
        select = select.filter(
            Expr::tuple([
                Expr::col(deployment_approval_decisions::Column::DecidedAt),
                Expr::col(deployment_approval_decisions::Column::Id),
            ])
            .gt(Expr::tuple([
                Expr::value(cursor.decided_at),
                Expr::value(cursor.id),
            ])),
        );
    }
    let rows_found = select
        .order_by_asc(deployment_approval_decisions::Column::DecidedAt)
        .order_by_asc(deployment_approval_decisions::Column::Id)
        .limit(u64::try_from(first + 1).unwrap_or(1))
        .all(db)
        .await?;
    let mut values: Vec<ApprovalDecision> = rows_found
        .into_iter()
        .map(|row| approval_decision_from_row(rows::decision_row(row)))
        .collect();
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
