//! The row locks a locked evaluation takes before it reads a principal's roles. Every lock is
//! `FOR UPDATE` and covers rows whether or not the membership is active, so a membership or role
//! cannot change under the caller's transaction once it has been evaluated.

use super::queries::{for_update_of, organization_memberships_of_project};
use crate::entity::{
    organization_membership_roles, organization_memberships, platform_role_assignments,
    project_membership_roles, project_memberships, projects,
};
use sea_orm::sea_query::{Expr, IntoTableRef};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, JoinType, QueryFilter, QueryOrder,
    QuerySelect, Select,
};
use uuid::Uuid;

/// Runs a locking select to its end. A row is locked as it is fetched, so every row is read.
async fn take<E: EntityTrait>(
    db: &impl ConnectionTrait,
    select: Select<E>,
    id: E::Column,
) -> Result<(), DbErr> {
    select
        .select_only()
        .column(id)
        .into_tuple::<Uuid>()
        .all(db)
        .await?;
    Ok(())
}

/// The projects a lock covers: the one being evaluated, or a page of them.
#[derive(Clone, Copy)]
enum Projects<'a> {
    One(Uuid),
    Page(&'a [Uuid]),
}

impl Projects<'_> {
    fn contain(self, project_id: impl ColumnTrait) -> Expr {
        match self {
            Projects::One(id) => project_id.eq(id),
            Projects::Page(ids) => project_id.eq_any(ids.iter().copied()),
        }
    }
}

fn platform_assignments(principal_id: Uuid) -> Select<platform_role_assignments::Entity> {
    platform_role_assignments::Entity::find()
        .filter(platform_role_assignments::Column::PrincipalId.eq(principal_id))
        .lock_exclusive()
}

/// The principal's memberships of the organizations that own `projects`; only the memberships
/// are locked.
fn organization_memberships(
    principal_id: Uuid,
    projects: Projects,
) -> Select<organization_memberships::Entity> {
    let memberships = organization_memberships::Entity::find()
        .join(
            JoinType::InnerJoin,
            organization_memberships_of_project().rev(),
        )
        .filter(projects.contain(projects::Column::Id))
        .filter(organization_memberships::Column::PrincipalId.eq(principal_id));
    for_update_of(
        memberships,
        [organization_memberships::Entity.into_table_ref()],
    )
}

/// The roles on [`organization_memberships`]; only the roles are locked.
fn organization_roles(
    principal_id: Uuid,
    projects: Projects,
) -> Select<organization_membership_roles::Entity> {
    let roles = organization_membership_roles::Entity::find()
        .inner_join(organization_memberships::Entity)
        .join(
            JoinType::InnerJoin,
            organization_memberships_of_project().rev(),
        )
        .filter(projects.contain(projects::Column::Id))
        .filter(organization_memberships::Column::PrincipalId.eq(principal_id));
    for_update_of(
        roles,
        [organization_membership_roles::Entity.into_table_ref()],
    )
}

/// The principal's memberships of `projects`.
fn project_memberships(
    principal_id: Uuid,
    projects: Projects,
) -> Select<project_memberships::Entity> {
    project_memberships::Entity::find()
        .filter(projects.contain(project_memberships::Column::ProjectId))
        .filter(project_memberships::Column::PrincipalId.eq(principal_id))
        .lock_exclusive()
}

/// The roles on [`project_memberships`]; only the roles are locked.
fn project_roles(
    principal_id: Uuid,
    projects: Projects,
) -> Select<project_membership_roles::Entity> {
    let roles = project_membership_roles::Entity::find()
        .inner_join(project_memberships::Entity)
        .filter(projects.contain(project_memberships::Column::ProjectId))
        .filter(project_memberships::Column::PrincipalId.eq(principal_id));
    for_update_of(roles, [project_membership_roles::Entity.into_table_ref()])
}

/// Locks the principal's memberships of one organization, then their roles.
pub async fn lock_organization_role_authority(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Uuid,
) -> Result<(), DbErr> {
    let memberships = organization_memberships::Entity::find()
        .filter(organization_memberships::Column::PrincipalId.eq(principal_id))
        .filter(organization_memberships::Column::OrganizationId.eq(organization_id))
        .lock_exclusive();
    take(db, memberships, organization_memberships::Column::Id).await?;
    let roles = organization_membership_roles::Entity::find()
        .inner_join(organization_memberships::Entity)
        .filter(organization_memberships::Column::PrincipalId.eq(principal_id))
        .filter(organization_memberships::Column::OrganizationId.eq(organization_id));
    take(
        db,
        for_update_of(
            roles,
            [organization_membership_roles::Entity.into_table_ref()],
        ),
        organization_membership_roles::Column::MembershipId,
    )
    .await
}

/// Locks, in this order, the principal's memberships of the project's organization, their roles,
/// the principal's memberships of the project, and their roles.
pub async fn lock_project_role_authority(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<(), DbErr> {
    let project = Projects::One(project_id);
    take(
        db,
        organization_memberships(principal_id, project),
        organization_memberships::Column::Id,
    )
    .await?;
    take(
        db,
        organization_roles(principal_id, project),
        organization_membership_roles::Column::MembershipId,
    )
    .await?;
    take(
        db,
        project_memberships(principal_id, project),
        project_memberships::Column::Id,
    )
    .await?;
    take(
        db,
        project_roles(principal_id, project),
        project_membership_roles::Column::MembershipId,
    )
    .await
}

/// Locks the principal's platform assignments, then [`lock_project_role_authority`].
pub async fn lock_deployment_authority(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<(), DbErr> {
    take(
        db,
        platform_assignments(principal_id),
        platform_role_assignments::Column::PrincipalId,
    )
    .await?;
    lock_project_role_authority(db, principal_id, project_id).await
}

/// [`lock_deployment_authority`] for a page of projects. Rows are locked in a fixed order so two
/// pages that overlap cannot deadlock each other.
pub async fn lock_deployment_approval_authority_page(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_ids: &[Uuid],
) -> Result<(), DbErr> {
    let page = Projects::Page(project_ids);
    take(
        db,
        platform_assignments(principal_id),
        platform_role_assignments::Column::PrincipalId,
    )
    .await?;
    take(
        db,
        organization_memberships(principal_id, page)
            .order_by_asc(organization_memberships::Column::OrganizationId)
            .order_by_asc(organization_memberships::Column::Id),
        organization_memberships::Column::Id,
    )
    .await?;
    take(
        db,
        organization_roles(principal_id, page)
            .order_by_asc(organization_membership_roles::Column::MembershipId)
            .order_by_asc(organization_membership_roles::Column::RoleCode),
        organization_membership_roles::Column::MembershipId,
    )
    .await?;
    take(
        db,
        project_memberships(principal_id, page)
            .order_by_asc(project_memberships::Column::ProjectId)
            .order_by_asc(project_memberships::Column::Id),
        project_memberships::Column::Id,
    )
    .await?;
    take(
        db,
        project_roles(principal_id, page)
            .order_by_asc(project_memberships::Column::ProjectId)
            .order_by_asc(project_membership_roles::Column::MembershipId)
            .order_by_asc(project_membership_roles::Column::RoleCode),
        project_membership_roles::Column::MembershipId,
    )
    .await
}
