use crate::schema::connection::PageInfo;
use crate::schema::RequestPrincipal;
use async_graphql::{Context, InputObject, Object, SimpleObject};
use hive_application::agent::{
    AgentCursor, AgentFilter as AppAgentFilter, ProjectAgentDirectoryQueryService,
};
use hive_domain::java_offset_date_time_string;
use hive_persistence::agent::PgProjectAgentDirectoryRepository;
use uuid::Uuid;

/// Ports the `Project` type. A full `#[Object]` because `agents` is a
/// lazily-resolved nested connection (`ProjectAgentDirectoryResolver.resolveAgents`).
/// Shared by the root `project(id)` query and `Organization.projects`'s edges: both
/// return the same SDL `Project` type, and `agents` must be reachable through either
/// path, so there is exactly one definition.
pub struct Project {
    pub(crate) id: Uuid,
    pub(crate) slug: String,
    pub(crate) display_name: String,
    pub(crate) lifecycle_status: String,
}

#[Object]
impl Project {
    async fn id(&self) -> async_graphql::ID {
        async_graphql::ID(self.id.to_string())
    }

    async fn slug(&self) -> &str {
        &self.slug
    }

    async fn display_name(&self) -> &str {
        &self.display_name
    }

    async fn lifecycle_status(&self) -> &str {
        &self.lifecycle_status
    }

    /// Ports `ProjectAgentDirectoryResolver.resolveAgents`.
    #[allow(clippy::too_many_arguments)]
    async fn agents(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 25)] first: Option<i32>,
        after: Option<String>,
        last: Option<i32>,
        before: Option<String>,
        filter: Option<AgentDirectoryFilter>,
    ) -> async_graphql::Result<AgentConnection> {
        if after.is_some() && before.is_some() {
            return Err(async_graphql::Error::new(
                "Supply either forward (first/after) or backward (last/before) pagination arguments, not both.",
            ));
        }
        let principal = ctx.data::<RequestPrincipal>()?;
        let repository =
            PgProjectAgentDirectoryRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
        let service = ProjectAgentDirectoryQueryService::new(repository);

        let resolved_filter = match filter {
            Some(filter) => AppAgentFilter::from(filter.lifecycle_status, filter.search)
                .map_err(|error| async_graphql::Error::new(error.to_string()))?,
            None => AppAgentFilter::none(),
        };

        let page = if let Some(before) = &before {
            service
                .find_agents_before(
                    principal.0,
                    self.id,
                    last.unwrap_or(25) as i64,
                    Some(before),
                    Some(resolved_filter.clone()),
                )
                .await
        } else {
            service
                .find_agents(
                    principal.0,
                    self.id,
                    first.unwrap_or(25) as i64,
                    after.as_deref(),
                    Some(resolved_filter.clone()),
                )
                .await
        }
        .map_err(|error| async_graphql::Error::new(error.to_string()))?;

        let edges: Vec<AgentEdge> = page
            .agents
            .iter()
            .map(|agent| AgentEdge {
                cursor: AgentCursor::encode(agent, &resolved_filter),
                node: Agent {
                    id: async_graphql::ID(agent.id.to_string()),
                    slug: agent.slug.clone(),
                    display_name: agent.display_name.clone(),
                    lifecycle_status: agent.lifecycle_status.clone(),
                    latest_published_version: agent.latest_published_version,
                    model: agent.model.clone(),
                },
            })
            .collect();

        Ok(AgentConnection {
            edges,
            page_info: PageInfo {
                end_cursor: page.end_cursor,
                has_next_page: page.has_next_page,
                has_previous_page: page.has_previous_page,
                start_cursor: page.start_cursor,
            },
            total_count: page.total_count as i32,
        })
    }
}

/// `Organization.projects`'s edge and connection wrap the same `Project` this
/// module defines, since the root `project(id)` query and the nested
/// `Organization.projects` field both resolve the identical SDL `Project` type.
#[derive(SimpleObject)]
pub struct ProjectEdge {
    pub cursor: String,
    pub node: Project,
}

#[derive(SimpleObject)]
pub struct ProjectConnection {
    pub edges: Vec<ProjectEdge>,
    pub page_info: PageInfo,
    pub total_count: i32,
}

#[derive(SimpleObject)]
pub struct Agent {
    pub id: async_graphql::ID,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub latest_published_version: Option<i32>,
    pub model: Option<String>,
}

#[derive(SimpleObject)]
pub struct AgentEdge {
    pub cursor: String,
    pub node: Agent,
}

#[derive(SimpleObject)]
pub struct AgentConnection {
    pub edges: Vec<AgentEdge>,
    pub page_info: PageInfo,
    pub total_count: i32,
}

#[derive(InputObject)]
pub struct AgentDirectoryFilter {
    pub lifecycle_status: Option<String>,
    pub search: Option<String>,
}

pub struct ProjectQueries;

#[Object]
impl ProjectQueries {
    /// Ports `ProjectAgentDirectoryResolver.resolveProject`.
    async fn project(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> async_graphql::Result<Option<Project>> {
        let principal = ctx.data::<RequestPrincipal>()?;
        let repository =
            PgProjectAgentDirectoryRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
        let service = ProjectAgentDirectoryQueryService::new(repository);
        let project = service
            .find_project(principal.0, id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(project.map(|project| Project {
            id: project.id,
            slug: project.slug,
            display_name: project.display_name,
            lifecycle_status: project.lifecycle_status,
        }))
    }

    /// Ports `ProjectDashboardResolver.resolve`.
    async fn project_dashboard(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> async_graphql::Result<Option<ProjectDashboard>> {
        let principal = ctx.data::<RequestPrincipal>()?;
        let repository = hive_persistence::project::PgProjectDashboardRepository::new(
            ctx.data::<sqlx::PgPool>()?.clone(),
        );
        let service = hive_application::project::ProjectDashboardQueryService::new(repository);
        let dashboard = service
            .find_dashboard(principal.0, id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(dashboard.map(|dashboard| ProjectDashboard {
            id: async_graphql::ID(dashboard.id.to_string()),
            slug: dashboard.slug,
            display_name: dashboard.display_name,
            lifecycle_status: dashboard.lifecycle_status,
            active_agents: dashboard.active_agents,
            active_deployments: dashboard.active_deployments,
            failed_deployments: dashboard.failed_deployments,
            pending_approvals: dashboard.pending_approvals,
            unhealthy_resources: dashboard.unhealthy_resources,
            current_period_cost: CostSummary {
                availability: dashboard.current_period_cost.availability,
                period_start: dashboard
                    .current_period_cost
                    .period_start
                    .map(java_offset_date_time_string),
                period_end: dashboard
                    .current_period_cost
                    .period_end
                    .map(java_offset_date_time_string),
                currency: dashboard.current_period_cost.currency,
                amount_cents: dashboard.current_period_cost.amount_cents,
                data_as_of: dashboard
                    .current_period_cost
                    .data_as_of
                    .map(java_offset_date_time_string),
            },
        }))
    }
}

#[derive(SimpleObject)]
pub struct CostSummary {
    pub availability: String,
    pub period_start: Option<String>,
    pub period_end: Option<String>,
    pub currency: Option<String>,
    pub amount_cents: Option<i32>,
    pub data_as_of: Option<String>,
}

#[derive(SimpleObject)]
pub struct ProjectDashboard {
    pub id: async_graphql::ID,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub active_agents: i32,
    pub active_deployments: i32,
    pub failed_deployments: i32,
    pub pending_approvals: i32,
    pub unhealthy_resources: i32,
    pub current_period_cost: CostSummary,
}
