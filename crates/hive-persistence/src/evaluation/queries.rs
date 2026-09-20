//! What the evaluation *commands* and the outbox worker read, on SeaORM entities: the definition
//! and its draft under lock, a published version, a run under lock, the run's frozen target, and
//! the small lookups `mutations`/`worker` decide on.
//!
//! Every GraphQL read of the evaluation domain is a generated Seaography entity query
//! (`docs/idiomatic-seaography-plan.md`, A2); what is left here is the command tier's own reads.
//! Each takes the same row locks the statements it replaces took: `FOR UPDATE` on the definition
//! row, on its draft row, on a run row, and on the project row `project_active` tests.

use crate::entity::enums::{EvaluationTargetKind, LifecycleStatus, LogicalEnvironmentClass};
use crate::entity::{
    agent_versions, agents, deployment_policy_snapshots, deployments,
    environment_definition_versions, evaluation_definition_drafts, evaluation_definition_versions,
    evaluation_definitions, evaluation_runs, evaluation_target_snapshots, projects,
};
use sea_orm::sea_query::{Expr, Func};
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, ExprTrait,
    FromQueryResult, JoinType, QueryFilter, QuerySelect, RelationTrait,
};
use uuid::Uuid;

use super::rows::Target;
use crate::capability::tx;

pub(super) async fn can(
    db: &impl ConnectionTrait,
    principal: Uuid,
    capability: &str,
    project: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    tx::has_evaluation_capability(db, principal, capability, project, lock).await
}

/// `select`, locking every row it reads `FOR UPDATE` when `lock` is set.
fn locking<E: EntityTrait>(select: sea_orm::Select<E>, lock: bool) -> sea_orm::Select<E> {
    if lock {
        select.lock_exclusive()
    } else {
        select
    }
}

/// The definition row itself.
pub async fn definition_row(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
    lock: bool,
) -> Result<Option<evaluation_definitions::Model>, DbErr> {
    locking(
        evaluation_definitions::Entity::find_by_id(definition_id),
        lock,
    )
    .one(db)
    .await
}

/// The definition the principal may view, or `None`. With `lock` the definition row and its draft
/// row are both locked `FOR UPDATE`, the two rows the deleted
/// `... FOR UPDATE OF definition, draft` locked, in that order.
pub async fn definition(
    db: &impl ConnectionTrait,
    principal: Uuid,
    definition_id: Uuid,
    lock: bool,
) -> Result<Option<evaluation_definitions::Model>, DbErr> {
    let Some(row) = definition_row(db, definition_id, lock).await? else {
        return Ok(None);
    };
    if !can(
        db,
        principal,
        tx::EVALUATION_DEFINITION_VIEW,
        row.project_id,
        lock,
    )
    .await?
    {
        return Ok(None);
    }
    if lock {
        draft(db, definition_id, true).await?;
    }
    Ok(Some(row))
}

/// The definition's one draft row; every definition has exactly one from its creation on.
pub async fn draft(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
    lock: bool,
) -> Result<evaluation_definition_drafts::Model, DbErr> {
    locking(
        evaluation_definition_drafts::Entity::find_by_id(definition_id),
        lock,
    )
    .one(db)
    .await?
    .ok_or_else(|| {
        DbErr::RecordNotFound(format!("no evaluation draft of definition {definition_id}"))
    })
}

pub async fn version(
    db: &impl ConnectionTrait,
    version_id: Uuid,
) -> Result<Option<evaluation_definition_versions::Model>, DbErr> {
    evaluation_definition_versions::Entity::find_by_id(version_id)
        .one(db)
        .await
}

pub async fn raw_run(
    db: &impl ConnectionTrait,
    id: Uuid,
    lock: bool,
) -> Result<Option<evaluation_runs::Model>, DbErr> {
    locking(evaluation_runs::Entity::find_by_id(id), lock)
        .one(db)
        .await
}

/// The run the principal may view, or `None`.
pub async fn run(
    db: &impl ConnectionTrait,
    principal: Uuid,
    run_id: Uuid,
    lock: bool,
) -> Result<Option<evaluation_runs::Model>, DbErr> {
    let Some(raw) = raw_run(db, run_id, lock).await? else {
        return Ok(None);
    };
    if !can(db, principal, tx::EVALUATION_RUN_VIEW, raw.project_id, lock).await? {
        return Ok(None);
    }
    Ok(Some(raw))
}

pub async fn snapshot(
    db: &impl ConnectionTrait,
    run: Uuid,
) -> Result<Option<evaluation_target_snapshots::Model>, DbErr> {
    evaluation_target_snapshots::Entity::find_by_id(run)
        .one(db)
        .await
}

/// The `AGENT_VERSION` branch of `resolve_target`, as one row.
#[derive(FromQueryResult)]
struct AgentVersionTargetRow {
    agent_version_id: Uuid,
    environment_definition_version_id: Uuid,
    environment_class: LogicalEnvironmentClass,
    agent_digest: String,
    catalog_release_id: String,
    catalog_release_digest: String,
    environment_digest: String,
}

/// The `DEPLOYMENT` branch of `resolve_target`, as one row.
#[derive(FromQueryResult)]
struct DeploymentTargetRow {
    agent_version_id: Uuid,
    deployment_id: Uuid,
    environment_definition_version_id: Uuid,
    environment_class: LogicalEnvironmentClass,
    agent_digest: String,
    target_digest: Option<String>,
    plan_digest: Option<String>,
    package_digest: Option<String>,
    binding_digest: Option<String>,
    catalog_release_id: String,
    catalog_release_digest: String,
    environment_digest: String,
}

/// Resolves an `AGENT_VERSION` target from `agent_versions`, or a `DEPLOYMENT` target from
/// `deployment_policy_snapshots`. A kind outside the two is no target.
pub async fn resolve_target(
    db: &impl ConnectionTrait,
    project: Uuid,
    kind: &str,
    target_id: Uuid,
    environment: Uuid,
) -> Result<Option<Target>, DbErr> {
    match EvaluationTargetKind::try_from_value(&kind.to_string()) {
        Ok(EvaluationTargetKind::AgentVersion) => {
            // The environment is joined on the version's own catalog release and pinned to the
            // requested id, exactly as the deleted statement's `JOIN ... ON environment.id = $1`
            // plus `WHERE ... environment.catalog_release_id = versioned.catalog_release_id` did.
            let environment_of_release: sea_orm::RelationDef =
                agent_versions::Entity::belongs_to(environment_definition_versions::Entity)
                    .from(agent_versions::Column::CatalogReleaseId)
                    .to(environment_definition_versions::Column::CatalogReleaseId)
                    .on_condition(move |_version, environment_version| {
                        Condition::all().add(
                            Expr::col((
                                environment_version,
                                environment_definition_versions::Column::Id,
                            ))
                            .eq(environment),
                        )
                    })
                    .into();
            let row = agent_versions::Entity::find()
                .join(JoinType::InnerJoin, agent_versions::Relation::Agents.def())
                .join(JoinType::InnerJoin, environment_of_release)
                .filter(agent_versions::Column::Id.eq(target_id))
                .filter(agents::Column::ProjectId.eq(project))
                .select_only()
                .column_as(agent_versions::Column::Id, "agent_version_id")
                .column_as(
                    environment_definition_versions::Column::Id,
                    "environment_definition_version_id",
                )
                .column_as(
                    environment_definition_versions::Column::LogicalEnvironmentClass,
                    "environment_class",
                )
                .column_as(agent_versions::Column::ContentDigest, "agent_digest")
                .column(agent_versions::Column::CatalogReleaseId)
                .column(agent_versions::Column::CatalogReleaseDigest)
                .column_as(
                    environment_definition_versions::Column::ContentDigest,
                    "environment_digest",
                )
                .into_model::<AgentVersionTargetRow>()
                .one(db)
                .await?;
            Ok(row.map(|row| Target {
                agent_version_id: row.agent_version_id,
                deployment_id: None,
                environment_definition_version_id: row.environment_definition_version_id,
                environment_class: row.environment_class.to_value(),
                agent_digest: row.agent_digest,
                target_digest: None,
                plan_digest: None,
                package_digest: None,
                binding_digest: None,
                catalog_release_id: row.catalog_release_id,
                catalog_release_digest: row.catalog_release_digest,
                environment_digest: row.environment_digest,
            }))
        }
        Ok(EvaluationTargetKind::Deployment) => {
            let row = deployments::Entity::find()
                .join(
                    JoinType::InnerJoin,
                    deployments::Relation::AgentVersions.def(),
                )
                .join(
                    JoinType::InnerJoin,
                    deployments::Relation::EnvironmentDefinitionVersions.def(),
                )
                .join(
                    JoinType::InnerJoin,
                    deployments::Relation::DeploymentPolicySnapshots.def(),
                )
                .filter(deployments::Column::Id.eq(target_id))
                .filter(deployments::Column::ProjectId.eq(project))
                .filter(deployments::Column::EnvironmentDefinitionVersionId.eq(environment))
                .select_only()
                .column(deployments::Column::AgentVersionId)
                .column_as(deployments::Column::Id, "deployment_id")
                .column_as(
                    environment_definition_versions::Column::Id,
                    "environment_definition_version_id",
                )
                .column_as(
                    environment_definition_versions::Column::LogicalEnvironmentClass,
                    "environment_class",
                )
                .column_as(agent_versions::Column::ContentDigest, "agent_digest")
                .column(deployment_policy_snapshots::Column::TargetDigest)
                .column(deployment_policy_snapshots::Column::PlanDigest)
                .column(deployment_policy_snapshots::Column::PackageDigest)
                .column(deployment_policy_snapshots::Column::BindingDigest)
                .column(agent_versions::Column::CatalogReleaseId)
                .column(agent_versions::Column::CatalogReleaseDigest)
                .column_as(
                    environment_definition_versions::Column::ContentDigest,
                    "environment_digest",
                )
                .into_model::<DeploymentTargetRow>()
                .one(db)
                .await?;
            Ok(row.map(|row| Target {
                agent_version_id: row.agent_version_id,
                deployment_id: Some(row.deployment_id),
                environment_definition_version_id: row.environment_definition_version_id,
                environment_class: row.environment_class.to_value(),
                agent_digest: row.agent_digest,
                target_digest: row.target_digest,
                plan_digest: row.plan_digest,
                package_digest: row.package_digest,
                binding_digest: row.binding_digest,
                catalog_release_id: row.catalog_release_id,
                catalog_release_digest: row.catalog_release_digest,
                environment_digest: row.environment_digest,
            }))
        }
        Err(_) => Ok(None),
    }
}

/// The target a run already froze, as a rerun's own target.
pub async fn target_from_snapshot(
    db: &impl ConnectionTrait,
    run: Uuid,
) -> Result<Option<Target>, DbErr> {
    Ok(snapshot(db, run).await?.map(|row| Target {
        agent_version_id: row.agent_version_id,
        deployment_id: row.deployment_id,
        environment_definition_version_id: row.environment_definition_version_id,
        environment_class: row.logical_environment_class.to_value(),
        agent_digest: row.agent_content_digest,
        target_digest: row.target_digest,
        plan_digest: row.plan_digest,
        package_digest: row.package_digest,
        binding_digest: row.binding_digest,
        catalog_release_id: row.catalog_release_id,
        catalog_release_digest: row.catalog_release_digest,
        environment_digest: row.environment_content_digest,
    }))
}

/// The already published version of this definition with exactly this content, if there is one.
pub async fn version_for_digest(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
    digest: &str,
) -> Result<Option<evaluation_definition_versions::Model>, DbErr> {
    evaluation_definition_versions::Entity::find()
        .filter(evaluation_definition_versions::Column::DefinitionId.eq(definition_id))
        .filter(evaluation_definition_versions::Column::ContentDigest.eq(digest))
        .one(db)
        .await
}

/// `COALESCE(MAX(version_number), 0) + 1`, read as the highest stored number.
pub async fn next_version_number(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
) -> Result<i64, DbErr> {
    let highest: Option<Option<i64>> = evaluation_definition_versions::Entity::find()
        .filter(evaluation_definition_versions::Column::DefinitionId.eq(definition_id))
        .select_only()
        .expr_as(
            Func::max(Expr::col(
                evaluation_definition_versions::Column::VersionNumber,
            )),
            "highest",
        )
        .into_tuple::<Option<i64>>()
        .one(db)
        .await?;
    Ok(highest.flatten().unwrap_or(0) + 1)
}

/// Whether the project is active, with its row locked `FOR UPDATE`.
pub async fn project_active(db: &impl ConnectionTrait, project: Uuid) -> Result<bool, DbErr> {
    Ok(projects::Entity::find_by_id(project)
        .lock_exclusive()
        .one(db)
        .await?
        .is_some_and(|project| project.lifecycle_status == LifecycleStatus::Active))
}
