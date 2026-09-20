//! What a principal may read, loaded once per GraphQL request through the ORM and turned into the
//! row conditions Seaography's `entity_filter` hook applies to every generated query
//! (`docs/idiomatic-seaography-plan.md`, A3).
//!
//! Visibility rules (from the capability evaluator): an organization is visible to its active
//! members; a project and everything under it is visible to active members of its organization
//! (every project role also requires that membership); a platform administrator sees everything.
//! The catalog is one shared set of rows with no owner: `CATALOG.VIEW` is held at an organization
//! by its active members and by a platform administrator, so the catalog is visible to a principal
//! who is an active member of any organization, or a platform administrator.
//!
//! Administration rows follow the evaluator's view capabilities, not plain membership:
//! organization memberships and their roles need `ORGANIZATION_MEMBERSHIP.VIEW` (an active
//! `ORGANIZATION_ADMIN`); project memberships, their roles and the budget policy need
//! `PROJECT_MEMBERSHIP.VIEW` / `PROJECT_BUDGET.VIEW` (an active `ORGANIZATION_ADMIN` or `AUDITOR`
//! of the project's organization, or an active `PROJECT_ADMIN` or `AUDITOR` of the project); the
//! approval policy needs `PROJECT_APPROVAL_POLICY.VIEW` (the same, plus `DEPLOYMENT_APPROVER`). A
//! platform administrator holds all of them. Settings connections follow the project. A principal
//! row is visible to itself and to whoever may view a membership it holds.

use crate::capability::queries::{active_organization_roles, active_project_roles};
use crate::entity::enums::{OrganizationRoleCode, PlatformRoleCode, ProjectRoleCode};
use crate::entity::{
    agent_drafts, agent_operational_view_projection, agent_versions, agents,
    organization_membership_roles, organization_memberships, organizations,
    platform_role_assignments, principal_display_preferences, principals,
    project_approval_policies, project_approval_policy_versions, project_budget_policies,
    project_budget_policy_versions, project_dashboard_projection, project_membership_roles,
    project_memberships, project_settings_connections, project_tool_connections, projects,
    reusable_resource_drafts, reusable_resource_versions, reusable_resources,
};
use sea_orm::sea_query::{Expr, ExprTrait, SelectStatement};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect,
    QueryTrait,
};
use uuid::Uuid;

/// The requesting principal's read authority, inserted into the GraphQL request data by the
/// `/graphql` handler. `None` when it could not be loaded. It lives here, not in `hive-api`,
/// because the entities' computed fields read it too.
pub struct RequestAuthority(pub Option<Authority>);

#[derive(Debug, Clone)]
pub struct Authority {
    pub principal_id: Uuid,
    pub platform_admin: bool,
    pub organization_ids: Vec<Uuid>,
}

impl Authority {
    pub async fn load(db: &impl ConnectionTrait, principal_id: Uuid) -> Result<Self, DbErr> {
        let platform_admin = platform_role_assignments::Entity::find_by_id((
            principal_id,
            PlatformRoleCode::PlatformAdmin,
        ))
        .one(db)
        .await?
        .is_some();
        let organization_ids = organization_memberships::Entity::find()
            .select_only()
            .column(organization_memberships::Column::OrganizationId)
            .filter(organization_memberships::Column::PrincipalId.eq(principal_id))
            .filter(
                Expr::col(organization_memberships::Column::StartedAt)
                    .lte(Expr::current_timestamp()),
            )
            .filter(organization_memberships::Column::EndedAt.is_null())
            .into_tuple::<Uuid>()
            .all(db)
            .await?;
        Ok(Self {
            principal_id,
            platform_admin,
            organization_ids,
        })
    }

    /// The row condition for a generated read of `entity` (Seaography's GraphQL type name), or
    /// `None` when the entity has no rule. Callers must treat `None` as deny.
    pub fn read_condition(&self, entity: &str) -> Option<Condition> {
        let condition = match entity {
            "Organizations" => self.organizations(),
            "Projects" => self.projects(),
            "Agents" => self.agents(),
            "AgentVersions" => self.agent_versions(),
            "AgentDrafts" => self.agent_drafts(),
            "ProjectDashboardProjection" => self.project_dashboard_projection(),
            "AgentOperationalViewProjection" => self.agent_operational_view_projection(),
            "Principals" => self.principals(),
            "PrincipalDisplayPreferences" => self.principal_display_preferences(),
            "CatalogReleases"
            | "CatalogDefinitions"
            | "CatalogEnvironments"
            | "CatalogProjectionHeads" => self.catalog(),
            "ReusableResources" => self.reusable_resources(),
            "ReusableResourceDrafts" => self.reusable_resource_drafts(),
            "ReusableResourceVersions" => self.reusable_resource_versions(),
            "ProjectToolConnections" => self.project_tool_connections(),
            "OrganizationMemberships" => self.organization_memberships(),
            "OrganizationMembershipRoles" => self.organization_membership_roles(),
            "ProjectMemberships" => self.project_memberships(),
            "ProjectMembershipRoles" => self.project_membership_roles(),
            "ProjectBudgetPolicies" => self.project_budget_policies(),
            "ProjectBudgetPolicyVersions" => self.project_budget_policy_versions(),
            "ProjectApprovalPolicies" => self.project_approval_policies(),
            "ProjectApprovalPolicyVersions" => self.project_approval_policy_versions(),
            "ProjectSettingsConnections" => self.project_settings_connections(),
            _ => return None,
        };
        Some(condition)
    }

    fn organizations(&self) -> Condition {
        self.unless_platform_admin(|| {
            organizations::Column::Id.is_in(self.organization_ids.clone())
        })
    }

    fn projects(&self) -> Condition {
        self.unless_platform_admin(|| {
            projects::Column::OrganizationId.is_in(self.organization_ids.clone())
        })
    }

    fn agents(&self) -> Condition {
        self.unless_platform_admin(|| agents::Column::ProjectId.in_subquery(self.project_ids()))
    }

    fn agent_versions(&self) -> Condition {
        self.unless_platform_admin(|| {
            agent_versions::Column::AgentId.in_subquery(
                agents::Entity::find()
                    .select_only()
                    .column(agents::Column::Id)
                    .filter(agents::Column::ProjectId.in_subquery(self.project_ids()))
                    .into_query(),
            )
        })
    }

    /// A draft is visible with its agent.
    fn agent_drafts(&self) -> Condition {
        self.unless_platform_admin(|| {
            agent_drafts::Column::AgentId.in_subquery(
                agents::Entity::find()
                    .select_only()
                    .column(agents::Column::Id)
                    .filter(agents::Column::ProjectId.in_subquery(self.project_ids()))
                    .into_query(),
            )
        })
    }

    fn project_dashboard_projection(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_dashboard_projection::Column::OrganizationId
                .is_in(self.organization_ids.clone())
        })
    }

    fn agent_operational_view_projection(&self) -> Condition {
        self.unless_platform_admin(|| {
            agent_operational_view_projection::Column::OrganizationId
                .is_in(self.organization_ids.clone())
        })
    }

    /// A principal reads its own row, and the row of a principal whose membership it may view:
    /// a member (current or former) of an organization where it holds
    /// `ORGANIZATION_MEMBERSHIP.VIEW`, or of a project where it holds `PROJECT_MEMBERSHIP.VIEW`.
    /// That is what the administration member lists show (name, email, last seen). A platform
    /// administrator holds both capabilities everywhere, so it reads every principal that has a
    /// membership; a principal with no membership stays visible to itself only. The principals an
    /// administrator may add are the computed `availablePrincipals` field, not this rule.
    fn principals(&self) -> Condition {
        let organization_members = organization_memberships::Entity::find()
            .select_only()
            .column(organization_memberships::Column::PrincipalId);
        let project_members = project_memberships::Entity::find()
            .select_only()
            .column(project_memberships::Column::PrincipalId);
        let (organization_members, project_members) = if self.platform_admin {
            (organization_members, project_members)
        } else {
            (
                organization_members.filter(
                    organization_memberships::Column::OrganizationId
                        .in_subquery(self.administered_organization_ids()),
                ),
                project_members.filter(
                    project_memberships::Column::ProjectId
                        .in_subquery(self.membership_view_project_ids()),
                ),
            )
        };
        Condition::any()
            .add(principals::Column::Id.eq(self.principal_id))
            .add(principals::Column::Id.in_subquery(organization_members.into_query()))
            .add(principals::Column::Id.in_subquery(project_members.into_query()))
    }

    fn principal_display_preferences(&self) -> Condition {
        Condition::all()
            .add(principal_display_preferences::Column::PrincipalId.eq(self.principal_id))
    }

    /// The catalog has no owner; whoever holds `CATALOG.VIEW` at any organization reads all of it.
    fn catalog(&self) -> Condition {
        if self.platform_admin || !self.organization_ids.is_empty() {
            Condition::all()
        } else {
            deny_all()
        }
    }

    fn reusable_resources(&self) -> Condition {
        self.unless_platform_admin(|| {
            reusable_resources::Column::ProjectId.in_subquery(self.project_ids())
        })
    }

    /// A draft revision is visible with its resource.
    fn reusable_resource_drafts(&self) -> Condition {
        self.unless_platform_admin(|| {
            reusable_resource_drafts::Column::ResourceId.in_subquery(self.resource_ids())
        })
    }

    /// A published version is visible with its resource.
    fn reusable_resource_versions(&self) -> Condition {
        self.unless_platform_admin(|| {
            reusable_resource_versions::Column::ResourceId.in_subquery(self.resource_ids())
        })
    }

    /// The MCP server descriptors of a project.
    fn project_tool_connections(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_tool_connections::Column::ProjectId.in_subquery(self.project_ids())
        })
    }

    /// `ORGANIZATION_MEMBERSHIP.VIEW`.
    fn organization_memberships(&self) -> Condition {
        self.unless_platform_admin(|| {
            organization_memberships::Column::OrganizationId
                .in_subquery(self.administered_organization_ids())
        })
    }

    /// A role is visible with its membership.
    fn organization_membership_roles(&self) -> Condition {
        self.unless_platform_admin(|| {
            organization_membership_roles::Column::MembershipId.in_subquery(
                organization_memberships::Entity::find()
                    .select_only()
                    .column(organization_memberships::Column::Id)
                    .filter(
                        organization_memberships::Column::OrganizationId
                            .in_subquery(self.administered_organization_ids()),
                    )
                    .into_query(),
            )
        })
    }

    /// `PROJECT_MEMBERSHIP.VIEW`.
    fn project_memberships(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_memberships::Column::ProjectId.in_subquery(self.membership_view_project_ids())
        })
    }

    /// A role is visible with its membership.
    fn project_membership_roles(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_membership_roles::Column::MembershipId.in_subquery(
                project_memberships::Entity::find()
                    .select_only()
                    .column(project_memberships::Column::Id)
                    .filter(
                        project_memberships::Column::ProjectId
                            .in_subquery(self.membership_view_project_ids()),
                    )
                    .into_query(),
            )
        })
    }

    /// `PROJECT_BUDGET.VIEW`, which the evaluator grants to the same roles as
    /// `PROJECT_MEMBERSHIP.VIEW`.
    fn project_budget_policies(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_budget_policies::Column::ProjectId
                .in_subquery(self.membership_view_project_ids())
        })
    }

    fn project_budget_policy_versions(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_budget_policy_versions::Column::ProjectId
                .in_subquery(self.membership_view_project_ids())
        })
    }

    /// `PROJECT_APPROVAL_POLICY.VIEW`.
    fn project_approval_policies(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_approval_policies::Column::ProjectId
                .in_subquery(self.approval_policy_view_project_ids())
        })
    }

    /// A policy version is visible with its policy.
    fn project_approval_policy_versions(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_approval_policy_versions::Column::PolicyId.in_subquery(
                project_approval_policies::Entity::find()
                    .select_only()
                    .column(project_approval_policies::Column::Id)
                    .filter(
                        project_approval_policies::Column::ProjectId
                            .in_subquery(self.approval_policy_view_project_ids()),
                    )
                    .into_query(),
            )
        })
    }

    /// Settings connections carry no secret and have no capability of their own; they are
    /// visible with their project, as `projectAdministration.connections` was.
    fn project_settings_connections(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_settings_connections::Column::ProjectId.in_subquery(self.project_ids())
        })
    }

    /// The organizations where the principal is an active `ORGANIZATION_ADMIN`.
    fn administered_organization_ids(&self) -> SelectStatement {
        self.organization_ids_with_role(&[OrganizationRoleCode::OrganizationAdmin])
    }

    fn organization_ids_with_role(&self, roles: &[OrganizationRoleCode]) -> SelectStatement {
        active_organization_roles(self.principal_id)
            .select_only()
            .column(organization_memberships::Column::OrganizationId)
            .filter(organization_membership_roles::Column::RoleCode.is_in(roles.iter().copied()))
            .into_query()
    }

    /// The projects where the principal holds one of `roles` directly or is an active
    /// `ORGANIZATION_ADMIN` or `AUDITOR` of the owning organization. A project role counts only
    /// while the membership of the owning organization is active too, as in the evaluator.
    fn project_ids_viewed_through(&self, roles: &[ProjectRoleCode]) -> SelectStatement {
        let through_project = active_project_roles(self.principal_id)
            .select_only()
            .column(project_memberships::Column::ProjectId)
            .filter(project_membership_roles::Column::RoleCode.is_in(roles.iter().copied()))
            .into_query();
        let through_organization = self.organization_ids_with_role(&[
            OrganizationRoleCode::OrganizationAdmin,
            OrganizationRoleCode::Auditor,
        ]);
        projects::Entity::find()
            .select_only()
            .column(projects::Column::Id)
            .filter(
                Condition::any()
                    .add(projects::Column::OrganizationId.in_subquery(through_organization))
                    .add(projects::Column::Id.in_subquery(through_project)),
            )
            .into_query()
    }

    fn membership_view_project_ids(&self) -> SelectStatement {
        self.project_ids_viewed_through(&[ProjectRoleCode::ProjectAdmin, ProjectRoleCode::Auditor])
    }

    fn approval_policy_view_project_ids(&self) -> SelectStatement {
        self.project_ids_viewed_through(&[
            ProjectRoleCode::ProjectAdmin,
            ProjectRoleCode::Auditor,
            ProjectRoleCode::DeploymentApprover,
        ])
    }

    /// The ids of every reusable resource in a project the principal can see.
    fn resource_ids(&self) -> SelectStatement {
        reusable_resources::Entity::find()
            .select_only()
            .column(reusable_resources::Column::Id)
            .filter(reusable_resources::Column::ProjectId.in_subquery(self.project_ids()))
            .into_query()
    }

    /// The ids of every project in an organization the principal is an active member of.
    fn project_ids(&self) -> SelectStatement {
        projects::Entity::find()
            .select_only()
            .column(projects::Column::Id)
            .filter(projects::Column::OrganizationId.is_in(self.organization_ids.clone()))
            .into_query()
    }

    fn unless_platform_admin(&self, rule: impl FnOnce() -> Expr) -> Condition {
        if self.platform_admin {
            Condition::all()
        } else {
            Condition::all().add(rule())
        }
    }
}

/// Matches no row: the condition for an entity with no rule, or a request with no authority.
pub fn deny_all() -> Condition {
    Condition::all().add(Expr::val(false))
}
