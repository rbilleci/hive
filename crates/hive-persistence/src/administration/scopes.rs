//! What an administration write keeps current for the deployment approval domain: the
//! per-principal scope caches (`deployment_approval_principal_*_scopes`) that say since when a
//! principal may view approvals in an organization or a project, and the cascade that a project
//! archive runs over the project's pending approvals.
//!
//! A scope cache is rebuilt inside the command's transaction, right after the membership or role
//! write it follows from: the principal's rows are deleted, then re-derived by one
//! `INSERT ... SELECT` built with sea-query, so the statement reads the transaction's own writes.

use crate::audit::context::request_metadata;
use crate::capability::queries::{for_update_of, organization_memberships_of_project};
use crate::entity::enums::{
    ApprovalInvalidationCode, ApprovalRequirementStatus, DeploymentAuditAction,
    DeploymentLifecycleStatus, DeploymentRuntimeHealthStatus, OrganizationRoleCode,
    ProjectRoleCode,
};
use crate::entity::{
    deployment_approval_principal_organization_membership_scopes as membership_scopes,
    deployment_approval_principal_organization_scopes as organization_scopes,
    deployment_approval_principal_project_scopes as project_scopes,
    deployment_approval_project_archive_events, deployment_approval_requirements,
    deployment_audit_events, deployment_runtime_health, deployment_timeline_counters, deployments,
    organization_membership_roles, organization_memberships, project_membership_roles,
    project_memberships, projects,
};
use hive_application::administration::AdministrationScope;
use sea_orm::sea_query::{Expr, ExprTrait, Func, IntoTableRef, OnConflict, Query, SelectStatement};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, JoinType, NotSet, QueryFilter,
    QueryOrder, QuerySelect, QueryTrait, Select, Set,
};
use uuid::Uuid;

/// After a membership starts or ends.
pub async fn refresh_membership_scope(
    db: &impl ConnectionTrait,
    scope: AdministrationScope,
    principal: Uuid,
    scope_id: Uuid,
) -> Result<(), DbErr> {
    match scope {
        AdministrationScope::Organization => {
            refresh_organization_membership_scope(db, principal, scope_id).await?;
            refresh_organization_scope(db, principal, scope_id).await?;
            refresh_organization_project_scopes(db, principal, scope_id).await
        }
        AdministrationScope::Project => refresh_project_scope(db, principal, scope_id).await,
    }
}

/// After a membership's roles change.
pub async fn refresh_role_scope(
    db: &impl ConnectionTrait,
    scope: AdministrationScope,
    principal: Uuid,
    scope_id: Uuid,
) -> Result<(), DbErr> {
    match scope {
        AdministrationScope::Organization => {
            refresh_organization_scope(db, principal, scope_id).await
        }
        AdministrationScope::Project => refresh_project_scope(db, principal, scope_id).await,
    }
}

async fn insert_selected<C: sea_orm::Iden + 'static>(
    db: &impl ConnectionTrait,
    table: impl IntoTableRef,
    columns: [C; 3],
    select: SelectStatement,
) -> Result<(), DbErr> {
    let mut insert = Query::insert();
    insert
        .into_table(table)
        .columns(columns)
        .select_from(select)
        .map_err(|error| DbErr::Custom(error.to_string()))?;
    db.execute(&insert).await?;
    Ok(())
}

/// Since when the principal has been an active member of the organization.
async fn refresh_organization_membership_scope(
    db: &impl ConnectionTrait,
    principal: Uuid,
    organization: Uuid,
) -> Result<(), DbErr> {
    membership_scopes::Entity::delete_many()
        .filter(membership_scopes::Column::PrincipalId.eq(principal))
        .filter(membership_scopes::Column::OrganizationId.eq(organization))
        .exec(db)
        .await?;
    let active = organization_memberships::Entity::find()
        .select_only()
        .expr(Expr::val(principal))
        .expr(Expr::val(organization))
        .column(organization_memberships::Column::StartedAt)
        .filter(organization_memberships::Column::PrincipalId.eq(principal))
        .filter(organization_memberships::Column::OrganizationId.eq(organization))
        .filter(organization_memberships::Column::EndedAt.is_null())
        .filter(
            Expr::col(organization_memberships::Column::StartedAt).lte(Expr::current_timestamp()),
        )
        .into_query();
    insert_selected(
        db,
        membership_scopes::Entity,
        [
            membership_scopes::Column::PrincipalId,
            membership_scopes::Column::OrganizationId,
            membership_scopes::Column::ValidAfter,
        ],
        active,
    )
    .await
}

/// Since when the principal has held an organization role that views approvals.
async fn refresh_organization_scope(
    db: &impl ConnectionTrait,
    principal: Uuid,
    organization: Uuid,
) -> Result<(), DbErr> {
    organization_scopes::Entity::delete_many()
        .filter(organization_scopes::Column::PrincipalId.eq(principal))
        .filter(organization_scopes::Column::OrganizationId.eq(organization))
        .exec(db)
        .await?;
    let earliest = organization_memberships::Entity::find()
        .select_only()
        .expr(Expr::val(principal))
        .expr(Expr::val(organization))
        .expr(Func::min(Expr::col((
            organization_memberships::Entity,
            organization_memberships::Column::StartedAt,
        ))))
        .inner_join(organization_membership_roles::Entity)
        .filter(organization_memberships::Column::PrincipalId.eq(principal))
        .filter(organization_memberships::Column::OrganizationId.eq(organization))
        .filter(organization_memberships::Column::EndedAt.is_null())
        .filter(organization_membership_roles::Column::RoleCode.is_in([
            OrganizationRoleCode::OrganizationAdmin,
            OrganizationRoleCode::Auditor,
        ]))
        .group_by(organization_memberships::Column::PrincipalId)
        .into_query();
    insert_selected(
        db,
        organization_scopes::Entity,
        [
            organization_scopes::Column::PrincipalId,
            organization_scopes::Column::OrganizationId,
            organization_scopes::Column::ValidAfter,
        ],
        earliest,
    )
    .await
}

/// The principal's open project memberships that hold a role that views approvals, joined to the
/// principal's open membership of the owning organization. One row per such role.
fn approval_viewing_project_memberships(principal: Uuid) -> Select<project_memberships::Entity> {
    project_memberships::Entity::find()
        .select_only()
        .inner_join(project_membership_roles::Entity)
        .inner_join(projects::Entity)
        .join(JoinType::InnerJoin, organization_memberships_of_project())
        .filter(organization_memberships::Column::PrincipalId.eq(principal))
        .filter(project_memberships::Column::PrincipalId.eq(principal))
        .filter(project_memberships::Column::EndedAt.is_null())
        .filter(organization_memberships::Column::EndedAt.is_null())
        .filter(project_membership_roles::Column::RoleCode.is_in([
            ProjectRoleCode::ProjectAdmin,
            ProjectRoleCode::DeploymentApprover,
            ProjectRoleCode::Auditor,
        ]))
}

/// The later of the project membership's start and the organization membership's start, at its
/// earliest over the group.
fn earliest_joint_start() -> sea_orm::sea_query::FunctionCall {
    Func::min(Func::greatest([
        Expr::col((
            project_memberships::Entity,
            project_memberships::Column::StartedAt,
        )),
        Expr::col((
            organization_memberships::Entity,
            organization_memberships::Column::StartedAt,
        )),
    ]))
}

const PROJECT_SCOPE_COLUMNS: [project_scopes::Column; 3] = [
    project_scopes::Column::PrincipalId,
    project_scopes::Column::ProjectId,
    project_scopes::Column::ValidAfter,
];

/// Since when the principal has held a project role that views approvals in one project.
async fn refresh_project_scope(
    db: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
) -> Result<(), DbErr> {
    project_scopes::Entity::delete_many()
        .filter(project_scopes::Column::PrincipalId.eq(principal))
        .filter(project_scopes::Column::ProjectId.eq(project))
        .exec(db)
        .await?;
    let earliest = approval_viewing_project_memberships(principal)
        .expr(Expr::val(principal))
        .expr(Expr::val(project))
        .expr(earliest_joint_start())
        .filter(project_memberships::Column::ProjectId.eq(project))
        .group_by(project_memberships::Column::PrincipalId)
        .group_by(project_memberships::Column::ProjectId)
        .into_query();
    insert_selected(db, project_scopes::Entity, PROJECT_SCOPE_COLUMNS, earliest).await
}

/// [`refresh_project_scope`] for every project of one organization.
async fn refresh_organization_project_scopes(
    db: &impl ConnectionTrait,
    principal: Uuid,
    organization: Uuid,
) -> Result<(), DbErr> {
    project_scopes::Entity::delete_many()
        .filter(project_scopes::Column::PrincipalId.eq(principal))
        .filter(
            project_scopes::Column::ProjectId.in_subquery(
                projects::Entity::find()
                    .select_only()
                    .column(projects::Column::Id)
                    .filter(projects::Column::OrganizationId.eq(organization))
                    .into_query(),
            ),
        )
        .exec(db)
        .await?;
    let earliest = approval_viewing_project_memberships(principal)
        .expr(Expr::val(principal))
        .column(project_memberships::Column::ProjectId)
        .expr(earliest_joint_start())
        .filter(projects::Column::OrganizationId.eq(organization))
        .group_by(project_memberships::Column::ProjectId)
        .into_query();
    insert_selected(db, project_scopes::Entity, PROJECT_SCOPE_COLUMNS, earliest).await
}

/// A project archive ends every approval cycle still pending in the project: the requirement is
/// invalidated, the deployment canceled, and the timeline and runtime health say why.
pub async fn invalidate_pending_approvals_for_archived_project(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    actor: Uuid,
) -> Result<(), DbErr> {
    let waiting = [
        DeploymentLifecycleStatus::AwaitingApproval,
        DeploymentLifecycleStatus::Requested,
    ];
    let pending = deployments::Entity::find()
        .select_only()
        .column(deployment_approval_requirements::Column::Id)
        .column(deployments::Column::Id)
        .inner_join(deployment_approval_requirements::Entity)
        .filter(deployments::Column::ProjectId.eq(project_id))
        .filter(deployments::Column::LifecycleStatus.is_in(waiting))
        .filter(
            deployment_approval_requirements::Column::Status.eq(ApprovalRequirementStatus::Pending),
        )
        .order_by_asc(deployments::Column::Id);
    let candidates = for_update_of(
        pending,
        [
            deployments::Entity.into_table_ref(),
            deployment_approval_requirements::Entity.into_table_ref(),
        ],
    )
    .into_tuple::<(Uuid, Uuid)>()
    .all(db)
    .await?;

    for (requirement_id, deployment_id) in candidates {
        let invalidated = deployment_approval_requirements::Entity::update_many()
            .col_expr(
                deployment_approval_requirements::Column::Status,
                Expr::value(ApprovalRequirementStatus::Invalidated.to_value()),
            )
            .col_expr(
                deployment_approval_requirements::Column::Revision,
                Expr::col(deployment_approval_requirements::Column::Revision).add(1),
            )
            .col_expr(
                deployment_approval_requirements::Column::InvalidatedAt,
                Expr::current_timestamp(),
            )
            .col_expr(
                deployment_approval_requirements::Column::InvalidationCode,
                Expr::value(ApprovalInvalidationCode::ProjectArchived.to_value()),
            )
            .filter(deployment_approval_requirements::Column::Id.eq(requirement_id))
            .filter(
                deployment_approval_requirements::Column::Status
                    .eq(ApprovalRequirementStatus::Pending),
            )
            .exec(db)
            .await?;
        if invalidated.rows_affected == 0 {
            continue;
        }

        // The timeline sequence this event takes; the counter holds the next one.
        let counter = deployment_timeline_counters::Entity::insert(
            deployment_timeline_counters::ActiveModel {
                deployment_id: Set(deployment_id),
                attempt_number: Set(0),
                next_sequence: Set(2),
            },
        )
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
        let sequence = counter.next_sequence - 1;

        let metadata = request_metadata();
        deployment_audit_events::Entity::insert(deployment_audit_events::ActiveModel {
            id: Set(Uuid::new_v4()),
            deployment_id: Set(deployment_id),
            actor_principal_id: Set(Some(actor)),
            action: Set(DeploymentAuditAction::ApprovalInvalidated),
            facts: Set(serde_json::json!({
                "requirementId": requirement_id.to_string(),
                "code": "PROJECT_ARCHIVED",
            })),
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

        deployment_runtime_health::Entity::update_many()
            .col_expr(
                deployment_runtime_health::Column::Status,
                Expr::value(DeploymentRuntimeHealthStatus::Canceled.to_value()),
            )
            .col_expr(
                deployment_runtime_health::Column::Summary,
                Expr::val("Project archive terminalized this pending approval cycle."),
            )
            .col_expr(
                deployment_runtime_health::Column::ObservedAt,
                Expr::current_timestamp(),
            )
            .col_expr(
                deployment_runtime_health::Column::Generation,
                Expr::col(deployment_runtime_health::Column::Generation).add(1),
            )
            .filter(deployment_runtime_health::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await?;

        deployments::Entity::update_many()
            .col_expr(
                deployments::Column::LifecycleStatus,
                Expr::value(DeploymentLifecycleStatus::Canceled.to_value()),
            )
            .col_expr(
                deployments::Column::Revision,
                Expr::col(deployments::Column::Revision).add(1),
            )
            .col_expr(
                deployments::Column::ProjectionRevision,
                Expr::col(deployments::Column::ProjectionRevision).add(1),
            )
            .col_expr(deployments::Column::UpdatedAt, Expr::current_timestamp())
            .filter(deployments::Column::Id.eq(deployment_id))
            .filter(deployments::Column::LifecycleStatus.is_in(waiting))
            .exec(db)
            .await?;

        // The projection revision moves a second time, as this cascade always has.
        deployments::Entity::update_many()
            .col_expr(
                deployments::Column::ProjectionRevision,
                Expr::col(deployments::Column::ProjectionRevision).add(1),
            )
            .filter(deployments::Column::Id.eq(deployment_id))
            .exec(db)
            .await?;
    }
    Ok(())
}

/// The event the approval worker reads to settle what the archive left behind.
pub async fn record_project_archive_event(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    actor: Uuid,
    archived_project_revision: i64,
) -> Result<(), DbErr> {
    deployment_approval_project_archive_events::Entity::insert(
        deployment_approval_project_archive_events::ActiveModel {
            id: Set(Uuid::new_v4()),
            project_id: Set(project_id),
            actor_principal_id: Set(Some(actor)),
            archived_at: NotSet,
            processed_at: NotSet,
            archived_project_revision: Set(Some(archived_project_revision)),
        },
    )
    .exec_without_returning(db)
    .await?;
    Ok(())
}
