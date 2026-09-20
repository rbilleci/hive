//! What a principal may read, loaded once per GraphQL request through the ORM and turned into the
//! row conditions Seaography's `entity_filter` hook applies to every generated query
//! (`docs/idiomatic-seaography-plan.md`, A3).
//!
//! Visibility rules (from the capability evaluator): an organization is visible to its active
//! members; a project and everything under it is visible to active members of its organization
//! (every project role also requires that membership); a platform administrator sees everything.

use crate::entity::{
    agent_versions, agents, organization_memberships, organizations, platform_role_assignments,
    project_dashboard_projection, projects,
};
use sea_orm::sea_query::{Expr, ExprTrait, SelectStatement};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect,
    QueryTrait,
};
use uuid::Uuid;

const PLATFORM_ADMIN: &str = "PLATFORM_ADMIN";

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
            PLATFORM_ADMIN.to_string(),
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
            "ProjectDashboardProjection" => self.project_dashboard_projection(),
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

    fn project_dashboard_projection(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_dashboard_projection::Column::OrganizationId
                .is_in(self.organization_ids.clone())
        })
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
