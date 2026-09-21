//! The read-only deployment reads that stay repository methods: resolving the compile context a
//! deploy, retry or rollback is planned against. The list, find, timeline, detail and environments
//! reads are generated entity queries instead, with their nested structures answered by relations
//! and by `super::computed`.

use super::rows;
use crate::capability::tx;
use crate::entity::enums::{DeploymentLifecycleStatus as EntityLifecycleStatus, LifecycleStatus};
use crate::entity::{
    agent_versions, agents, deployment_plan_versions, deployments, environment_definition_versions,
    project_approval_policies, project_approval_policy_versions, projects,
};
use hive_application::deployment::{
    ActiveTarget, Deployment, DeploymentCompilationContext, DeploymentRecoveryCompilationContext,
    EnvironmentDefinition, PolicySource, VersionSource,
};
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::sea_query::{Expr, ExprTrait, IntoTableRef, LockType, TableRef};
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, FromQueryResult,
    JoinType, QueryFilter, QueryOrder, QuerySelect, RelationTrait,
};
use uuid::Uuid;

async fn can_view(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    tx::has_deployment_capability(db, principal_id, tx::DEPLOYMENT_VIEW, project_id, lock).await
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
                strategy: source.strategy,
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

#[cfg(test)]
mod target_version_tests {
    use super::canonical_target_version;

    /// A version string that parses as a UUID is re-rendered canonically, so an upper-case or
    /// braced spelling of the same identifier compares equal downstream.
    #[test]
    fn a_uuid_target_version_is_rendered_canonically() {
        assert_eq!(
            canonical_target_version(Some("A1B2C3D4-0000-0000-0000-000000000001")),
            Some("a1b2c3d4-0000-0000-0000-000000000001".to_string())
        );
        assert_eq!(
            canonical_target_version(Some("{a1b2c3d4-0000-0000-0000-000000000001}")),
            Some("a1b2c3d4-0000-0000-0000-000000000001".to_string())
        );
    }

    /// Anything that is not a UUID is kept as the caller wrote it, trimmed and not lower-cased.
    #[test]
    fn a_non_uuid_target_version_is_only_trimmed() {
        assert_eq!(
            canonical_target_version(Some("  v2.1-RC  ")),
            Some("v2.1-RC".to_string())
        );
        assert_eq!(canonical_target_version(Some("")), Some(String::new()));
        assert_eq!(canonical_target_version(None), None);
    }
}
