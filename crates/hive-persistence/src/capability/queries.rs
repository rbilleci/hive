//! The evaluator's primitive checks. Each is one SeaORM statement over the entities. With `lock`
//! set, a check takes the row lock named on it, so its answer holds until the caller's transaction
//! ends.

use super::locks;
use crate::entity::enums::{
    ConsoleRoleCode, LifecycleStatus, OrganizationRoleCode, PlatformRoleCode, ProjectRoleCode,
};
use crate::entity::{
    console_role_assignments, organization_membership_roles, organization_memberships,
    organizations, platform_role_assignments, principals, project_membership_roles,
    project_memberships, projects,
};
use sea_orm::sea_query::{Expr, ExprTrait, IntoTableRef, LockType, TableRef, UnionType};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, JoinType, QueryFilter,
    QuerySelect, QueryTrait, RelationDef, Select,
};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    Organization,
    Project,
}

/// Whether `select` matches a row. Every matching row is fetched, not only the first, so a
/// locking select locks every row it matches.
async fn any_row<E: EntityTrait>(
    db: &impl ConnectionTrait,
    select: Select<E>,
    id: E::Column,
) -> Result<bool, DbErr> {
    let rows = select
        .select_only()
        .column(id)
        .into_tuple::<Uuid>()
        .all(db)
        .await?;
    Ok(!rows.is_empty())
}

/// `select`, locking every row it reads when `lock` is set.
fn locking<E: EntityTrait>(select: Select<E>, lock: bool, lock_type: LockType) -> Select<E> {
    if lock {
        select.lock(lock_type)
    } else {
        select
    }
}

/// `select`, locking the rows it reads from `tables` only: `FOR UPDATE OF tables`.
pub(crate) fn for_update_of<E: EntityTrait>(
    mut select: Select<E>,
    tables: impl IntoIterator<Item = TableRef>,
) -> Select<E> {
    QuerySelect::query(&mut select).lock_with_tables(LockType::Update, tables);
    select
}

/// [`for_update_of`] when `lock` is set.
fn locking_tables<E: EntityTrait>(
    select: Select<E>,
    lock: bool,
    tables: impl IntoIterator<Item = TableRef>,
) -> Select<E> {
    if lock {
        for_update_of(select, tables)
    } else {
        select
    }
}

/// A membership is active once it has started and until it ends.
fn active<C: ColumnTrait>(started_at: C, ended_at: C) -> Condition {
    Condition::all()
        .add(Expr::col(started_at.as_column_ref()).lte(Expr::current_timestamp()))
        .add(ended_at.is_null())
}

fn active_organization_membership_now() -> Condition {
    active(
        organization_memberships::Column::StartedAt,
        organization_memberships::Column::EndedAt,
    )
}

fn active_project_membership_now() -> Condition {
    active(
        project_memberships::Column::StartedAt,
        project_memberships::Column::EndedAt,
    )
}

/// From a project to the memberships of its organization. The two tables share
/// `organization_id`; the entities relate them only through `organizations`, which the evaluator
/// does not read here.
pub(crate) fn organization_memberships_of_project() -> RelationDef {
    projects::Entity::belongs_to(organization_memberships::Entity)
        .from(projects::Column::OrganizationId)
        .to(organization_memberships::Column::OrganizationId)
        .into()
}

/// [`organization_memberships_of_project`], narrowed to the membership held by the principal in
/// `principal`, a column of a table already in the statement.
fn organization_membership_of_project_for<C: ColumnTrait>(principal: C) -> RelationDef {
    organization_memberships_of_project().on_condition(move |_project, membership| {
        Condition::all().add(
            Expr::col((membership, organization_memberships::Column::PrincipalId))
                .equals(principal.as_column_ref()),
        )
    })
}

/// The principal's organization memberships that are active now, one row per role held.
pub(crate) fn active_organization_roles(
    principal_id: Uuid,
) -> Select<organization_memberships::Entity> {
    organization_memberships::Entity::find()
        .inner_join(organization_membership_roles::Entity)
        .filter(organization_memberships::Column::PrincipalId.eq(principal_id))
        .filter(active_organization_membership_now())
}

/// The principal's project memberships that are active now and backed by an active membership of
/// the project's organization, one row per role held.
pub(crate) fn active_project_roles(principal_id: Uuid) -> Select<project_memberships::Entity> {
    project_memberships::Entity::find()
        .inner_join(project_membership_roles::Entity)
        .inner_join(projects::Entity)
        .join(
            JoinType::InnerJoin,
            organization_membership_of_project_for(project_memberships::Column::PrincipalId),
        )
        .filter(project_memberships::Column::PrincipalId.eq(principal_id))
        .filter(active_project_membership_now())
        .filter(active_organization_membership_now())
}

/// Locks the principal row `FOR KEY SHARE`.
pub async fn known_principal(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let principal = principals::Entity::find_by_id(principal_id);
    any_row(
        db,
        locking(principal, lock, LockType::KeyShare),
        principals::Column::Id,
    )
    .await
}

/// Locks the organization or project row `FOR KEY SHARE`.
pub async fn scope_exists(
    db: &impl ConnectionTrait,
    kind: ScopeKind,
    id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    match kind {
        ScopeKind::Organization => {
            let organization = organizations::Entity::find_by_id(id);
            any_row(
                db,
                locking(organization, lock, LockType::KeyShare),
                organizations::Column::Id,
            )
            .await
        }
        ScopeKind::Project => {
            let project = projects::Entity::find_by_id(id);
            any_row(
                db,
                locking(project, lock, LockType::KeyShare),
                projects::Column::Id,
            )
            .await
        }
    }
}

/// Locks the platform administrator assignment `FOR UPDATE`.
pub async fn has_platform_admin(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let assignment = platform_role_assignments::Entity::find_by_id((
        principal_id,
        PlatformRoleCode::PlatformAdmin,
    ));
    any_row(
        db,
        locking(assignment, lock, LockType::Update),
        platform_role_assignments::Column::PrincipalId,
    )
    .await
}

/// Locks the active membership `FOR UPDATE`.
pub async fn active_organization_membership(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let membership = organization_memberships::Entity::find()
        .filter(organization_memberships::Column::PrincipalId.eq(principal_id))
        .filter(organization_memberships::Column::OrganizationId.eq(organization_id))
        .filter(active_organization_membership_now());
    any_row(
        db,
        locking(membership, lock, LockType::Update),
        organization_memberships::Column::Id,
    )
    .await
}

/// Locks the principal's memberships of the organization and their roles `FOR UPDATE` first.
pub async fn has_active_organization_role(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Uuid,
    role: OrganizationRoleCode,
    lock: bool,
) -> Result<bool, DbErr> {
    if lock {
        locks::lock_organization_role_authority(db, principal_id, organization_id).await?;
    }
    any_row(
        db,
        active_organization_roles(principal_id)
            .filter(organization_memberships::Column::OrganizationId.eq(organization_id))
            .filter(organization_membership_roles::Column::RoleCode.eq(role)),
        organization_memberships::Column::Id,
    )
    .await
}

/// Locks the principal's organization and project memberships for the project, and their roles,
/// `FOR UPDATE` first.
pub async fn has_active_project_role(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    role: ProjectRoleCode,
    lock: bool,
) -> Result<bool, DbErr> {
    if lock {
        locks::lock_project_role_authority(db, principal_id, project_id).await?;
    }
    any_row(
        db,
        active_project_roles(principal_id)
            .filter(project_memberships::Column::ProjectId.eq(project_id))
            .filter(project_membership_roles::Column::RoleCode.eq(role)),
        project_memberships::Column::Id,
    )
    .await
}

/// Every organization role the principal holds at `organization_id`, in one statement. Takes no
/// lock: the locked evaluation asks role by role through [`has_active_organization_role`], which
/// locks the rows it reads first.
pub(crate) async fn active_organization_role_codes(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Uuid,
) -> Result<HashSet<OrganizationRoleCode>, DbErr> {
    let codes = active_organization_roles(principal_id)
        .filter(organization_memberships::Column::OrganizationId.eq(organization_id))
        .select_only()
        .column(organization_membership_roles::Column::RoleCode)
        .into_tuple::<OrganizationRoleCode>()
        .all(db)
        .await?;
    Ok(codes.into_iter().collect())
}

/// Every project role the principal holds at `project_id`, in one statement. Takes no lock, for
/// the reason [`active_organization_role_codes`] gives.
pub(crate) async fn active_project_role_codes(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<HashSet<ProjectRoleCode>, DbErr> {
    let codes = active_project_roles(principal_id)
        .filter(project_memberships::Column::ProjectId.eq(project_id))
        .select_only()
        .column(project_membership_roles::Column::RoleCode)
        .into_tuple::<ProjectRoleCode>()
        .all(db)
        .await?;
    Ok(codes.into_iter().collect())
}

pub async fn project_visible(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    Ok(
        active_organization_for_project(db, principal_id, project_id, lock).await?
            || has_active_project_role(
                db,
                principal_id,
                project_id,
                ProjectRoleCode::ProjectAdmin,
                lock,
            )
            .await?
            || has_active_project_role(
                db,
                principal_id,
                project_id,
                ProjectRoleCode::AgentDeveloper,
                lock,
            )
            .await?
            || has_active_project_role(
                db,
                principal_id,
                project_id,
                ProjectRoleCode::Operator,
                lock,
            )
            .await?
            || has_active_project_role(
                db,
                principal_id,
                project_id,
                ProjectRoleCode::DeploymentApprover,
                lock,
            )
            .await?
            || has_active_project_role(
                db,
                principal_id,
                project_id,
                ProjectRoleCode::Auditor,
                lock,
            )
            .await?
            || has_platform_admin(db, principal_id, lock).await?,
    )
}

/// Locks the membership, not the project, `FOR UPDATE`.
pub(crate) async fn active_organization_for_project(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let membership = projects::Entity::find()
        .join(JoinType::InnerJoin, organization_memberships_of_project())
        .filter(projects::Column::Id.eq(project_id))
        .filter(organization_memberships::Column::PrincipalId.eq(principal_id))
        .filter(active_organization_membership_now());
    any_row(
        db,
        locking_tables(
            membership,
            lock,
            [organization_memberships::Entity.into_table_ref()],
        ),
        projects::Column::Id,
    )
    .await
}

/// Locks the console role assignment and the organization membership behind it `FOR UPDATE`.
pub async fn legacy_or_developer(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    Ok(
        legacy_console_assignment(db, principal_id, project_id, lock).await?
            || has_active_project_role(
                db,
                principal_id,
                project_id,
                ProjectRoleCode::ProjectAdmin,
                lock,
            )
            .await?
            || has_active_project_role(
                db,
                principal_id,
                project_id,
                ProjectRoleCode::AgentDeveloper,
                lock,
            )
            .await?,
    )
}

/// The retained console role assignment that grants authoring on a project before the membership
/// tables carried it. Locks the assignment and the organization membership behind it `FOR UPDATE`.
pub(crate) async fn legacy_console_assignment(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let assignment = console_role_assignments::Entity::find()
        .inner_join(projects::Entity)
        .join(
            JoinType::InnerJoin,
            organization_membership_of_project_for(console_role_assignments::Column::PrincipalId),
        )
        .filter(console_role_assignments::Column::ProjectId.eq(project_id))
        .filter(console_role_assignments::Column::PrincipalId.eq(principal_id))
        .filter(console_role_assignments::Column::RoleCode.is_in([
            ConsoleRoleCode::ProjectAdmin,
            ConsoleRoleCode::AgentDeveloper,
        ]))
        .filter(active_organization_membership_now());
    any_row(
        db,
        locking_tables(
            assignment,
            lock,
            [
                console_role_assignments::Entity.into_table_ref(),
                organization_memberships::Entity.into_table_ref(),
            ],
        ),
        console_role_assignments::Column::Id,
    )
    .await
}

/// Locks the project row `FOR KEY SHARE`.
pub async fn project_organization(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    lock: bool,
) -> Result<Option<Uuid>, DbErr> {
    locking(
        projects::Entity::find_by_id(project_id),
        lock,
        LockType::KeyShare,
    )
    .select_only()
    .column(projects::Column::OrganizationId)
    .into_tuple::<Uuid>()
    .one(db)
    .await
}

/// Locks the project row `FOR KEY SHARE`.
pub async fn active_project(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let project = projects::Entity::find_by_id(project_id)
        .filter(projects::Column::LifecycleStatus.eq(LifecycleStatus::Active));
    any_row(
        db,
        locking(project, lock, LockType::KeyShare),
        projects::Column::Id,
    )
    .await
}

/// The principal's approval authority, one row per granting role: organization roles grant view
/// on every project of the organization, project roles grant view and, for a deployment approver,
/// decide. One statement, so both halves are read from the same snapshot.
pub async fn authority_assignments(
    db: &impl ConnectionTrait,
    principal: Uuid,
    scoped_organization: Option<Uuid>,
    scoped_project: Option<Uuid>,
) -> Result<Vec<super::AuthorityAssignment>, DbErr> {
    let mut organization_roles = active_organization_roles(principal)
        .select_only()
        .column(organization_memberships::Column::OrganizationId)
        .expr(Expr::val(None::<Uuid>).cast_as("uuid"))
        .expr(Expr::val(true))
        .expr(Expr::val(false))
        .filter(organization_membership_roles::Column::RoleCode.is_in([
            OrganizationRoleCode::OrganizationAdmin,
            OrganizationRoleCode::Auditor,
        ]));
    let mut project_roles = active_project_roles(principal)
        .select_only()
        .column(projects::Column::OrganizationId)
        .column(project_memberships::Column::ProjectId)
        .expr(Expr::val(true))
        .expr(project_membership_roles::Column::RoleCode.eq(ProjectRoleCode::DeploymentApprover))
        .filter(project_membership_roles::Column::RoleCode.is_in([
            ProjectRoleCode::ProjectAdmin,
            ProjectRoleCode::DeploymentApprover,
            ProjectRoleCode::Auditor,
        ]));
    if let Some(organization_id) = scoped_organization {
        organization_roles = organization_roles
            .filter(organization_memberships::Column::OrganizationId.eq(organization_id));
        project_roles = project_roles.filter(projects::Column::OrganizationId.eq(organization_id));
    }
    if let Some(project_id) = scoped_project {
        let project_in_organization = projects::Entity::find()
            .select_only()
            .column(projects::Column::Id)
            .filter(projects::Column::Id.eq(project_id))
            .filter(
                Expr::col(projects::Column::OrganizationId.as_column_ref())
                    .equals(organization_memberships::Column::OrganizationId.as_column_ref()),
            )
            .into_query();
        organization_roles = organization_roles.filter(Expr::exists(project_in_organization));
        project_roles = project_roles.filter(project_memberships::Column::ProjectId.eq(project_id));
    }
    QuerySelect::query(&mut organization_roles).union(UnionType::All, project_roles.into_query());
    let rows = organization_roles
        .into_tuple::<(Uuid, Option<Uuid>, bool, bool)>()
        .all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(
            |(organization_id, project_id, approval_view, approval_decide)| {
                super::AuthorityAssignment {
                    organization_id,
                    project_id,
                    approval_view,
                    approval_decide,
                }
            },
        )
        .collect())
}
