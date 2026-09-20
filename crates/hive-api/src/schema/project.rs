//! Ports `schema/project.rs`. `Project` is complex (`GSR-NESTED-FIELDS`): `agents` is a
//! lazily-resolved nested connection, hand-built and folded onto the derived object. Shared by the
//! root `project(id)` query and `Organization.projects`'s edges (exactly one `Project` definition,
//! reachable through either path), matching the static tier's own module comment.

use crate::schema::scalars;
use crate::schema::RequestPrincipal;
use async_graphql::dynamic::{Field, FieldFuture, InputValue, TypeRef};
use hive_application::agent::{
    AgentCursor, AgentFilter as AppAgentFilter, ProjectAgentDirectoryQueryService,
};
use hive_domain::java_offset_date_time_string;
use hive_persistence::agent::PgProjectAgentDirectoryRepository;
use seaography::{
    BuilderContext, CustomFields, CustomInputType, CustomOutputObject, CustomOutputType,
};
use uuid::Uuid;

#[allow(non_snake_case)]
mod wire {
    use super::*;

    #[derive(CustomOutputType, Clone)]
    pub struct Project {
        pub id: crate::schema::scalars::Id,
        pub slug: String,
        pub displayName: String,
        pub lifecycleStatus: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ProjectEdge {
        pub cursor: String,
        pub node: Project,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ProjectConnection {
        pub edges: Vec<ProjectEdge>,
        pub pageInfo: seaography::PageInfo,
        pub totalCount: i32,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct Agent {
        pub id: crate::schema::scalars::Id,
        pub slug: String,
        pub displayName: String,
        pub lifecycleStatus: String,
        pub latestPublishedVersion: Option<i32>,
        pub model: Option<String>,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentEdge {
        pub cursor: String,
        pub node: Agent,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentConnection {
        pub edges: Vec<AgentEdge>,
        pub pageInfo: seaography::PageInfo,
        pub totalCount: i32,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "AgentDirectoryFilter")]
    pub struct AgentDirectoryFilter {
        pub lifecycleStatus: Option<String>,
        pub search: Option<String>,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct CostSummary {
        pub availability: String,
        pub periodStart: Option<String>,
        pub periodEnd: Option<String>,
        pub currency: Option<String>,
        pub amountCents: Option<i32>,
        pub dataAsOf: Option<String>,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ProjectDashboard {
        pub id: crate::schema::scalars::Id,
        pub slug: String,
        pub displayName: String,
        pub lifecycleStatus: String,
        pub activeAgents: i32,
        pub activeDeployments: i32,
        pub failedDeployments: i32,
        pub pendingApprovals: i32,
        pub unhealthyResources: i32,
        pub currentPeriodCost: CostSummary,
    }

    pub struct ProjectQueries;

    #[CustomFields]
    impl ProjectQueries {
        // Ports `ProjectAgentDirectoryResolver.resolveProject`.
        async fn project(
            ctx: &async_graphql::Context<'_>,
            id: crate::schema::scalars::Id,
        ) -> async_graphql::Result<Option<Project>> {
            let principal = ctx.data::<RequestPrincipal>()?;
            let repository = PgProjectAgentDirectoryRepository::new(
                ctx.data::<sea_orm::DatabaseConnection>()?.clone(),
            );
            let service = ProjectAgentDirectoryQueryService::new(repository);
            let project = service
                .find_project(principal.0, &id.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(project.map(|project| Project {
                id: project.id.to_string().into(),
                slug: project.slug,
                displayName: project.display_name,
                lifecycleStatus: project.lifecycle_status,
            }))
        }

        // Ports `ProjectDashboardResolver.resolve`.
        async fn projectDashboard(
            ctx: &async_graphql::Context<'_>,
            id: crate::schema::scalars::Id,
        ) -> async_graphql::Result<Option<ProjectDashboard>> {
            let principal = ctx.data::<RequestPrincipal>()?;
            let repository = hive_persistence::project::PgProjectDashboardRepository::new(
                ctx.data::<sea_orm::DatabaseConnection>()?.clone(),
            );
            let service = hive_application::project::ProjectDashboardQueryService::new(repository);
            let dashboard = service
                .find_dashboard(principal.0, &id.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(dashboard.map(|dashboard| ProjectDashboard {
                id: dashboard.id.to_string().into(),
                slug: dashboard.slug,
                displayName: dashboard.display_name,
                lifecycleStatus: dashboard.lifecycle_status,
                activeAgents: dashboard.active_agents,
                activeDeployments: dashboard.active_deployments,
                failedDeployments: dashboard.failed_deployments,
                pendingApprovals: dashboard.pending_approvals,
                unhealthyResources: dashboard.unhealthy_resources,
                currentPeriodCost: CostSummary {
                    availability: dashboard.current_period_cost.availability,
                    periodStart: dashboard
                        .current_period_cost
                        .period_start
                        .map(java_offset_date_time_string),
                    periodEnd: dashboard
                        .current_period_cost
                        .period_end
                        .map(java_offset_date_time_string),
                    currency: dashboard.current_period_cost.currency,
                    amountCents: dashboard.current_period_cost.amount_cents,
                    dataAsOf: dashboard
                        .current_period_cost
                        .data_as_of
                        .map(java_offset_date_time_string),
                },
            }))
        }
    }
}

pub use wire::{
    Agent, AgentConnection, AgentDirectoryFilter, AgentEdge, CostSummary, Project,
    ProjectConnection, ProjectDashboard, ProjectEdge, ProjectQueries,
};

fn context() -> &'static BuilderContext {
    crate::schema::context()
}

/// `Project.agents(after, before, filter, first: Int = 25, last)` (`GSR-DEFAULTS`). Ports
/// `ProjectAgentDirectoryResolver.resolveAgents`.
fn agents_field() -> Field {
    Field::new("agents", TypeRef::named_nn("AgentConnection"), |ctx| {
        FieldFuture::new(async move {
            let project = ctx.parent_value.try_downcast_ref::<wire::Project>()?;
            let project_id = Uuid::parse_str(&project.id.0)
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;

            let after = scalars::optional_string(ctx.args.get("after"))?;
            let before = scalars::optional_string(ctx.args.get("before"))?;
            if after.is_some() && before.is_some() {
                return Err(async_graphql::Error::new(
                    "Supply either forward (first/after) or backward (last/before) pagination arguments, not both.",
                ));
            }
            let first = scalars::optional_i64(ctx.args.get("first"))?;
            let last = scalars::optional_i64(ctx.args.get("last"))?;
            let filter = match scalars::defined(ctx.args.get("filter")) {
                Some(filter) => {
                    let filter = AgentDirectoryFilter::parse_value(context(), Some(filter))?;
                    AppAgentFilter::from(filter.lifecycleStatus, filter.search)
                        .map_err(|error| async_graphql::Error::new(error.to_string()))?
                }
                None => AppAgentFilter::none(),
            };

            let principal = ctx.ctx.data::<RequestPrincipal>()?;
            let repository = PgProjectAgentDirectoryRepository::new(
                ctx.ctx.data::<sea_orm::DatabaseConnection>()?.clone(),
            );
            let service = ProjectAgentDirectoryQueryService::new(repository);

            let page = if let Some(before) = &before {
                service
                    .find_agents_before(
                        principal.0,
                        project_id,
                        last.unwrap_or(25),
                        Some(before),
                        Some(filter.clone()),
                    )
                    .await
            } else {
                service
                    .find_agents(
                        principal.0,
                        project_id,
                        first.unwrap_or(25),
                        after.as_deref(),
                        Some(filter.clone()),
                    )
                    .await
            }
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;

            let edges: Vec<AgentEdge> = page
                .agents
                .iter()
                .map(|agent| AgentEdge {
                    cursor: AgentCursor::encode(agent, &filter),
                    node: Agent {
                        id: agent.id.to_string().into(),
                        slug: agent.slug.clone(),
                        displayName: agent.display_name.clone(),
                        lifecycleStatus: agent.lifecycle_status.clone(),
                        latestPublishedVersion: agent.latest_published_version,
                        model: agent.model.clone(),
                    },
                })
                .collect();

            let connection = AgentConnection {
                edges,
                pageInfo: seaography::PageInfo {
                    has_previous_page: page.has_previous_page,
                    has_next_page: page.has_next_page,
                    start_cursor: page.start_cursor,
                    end_cursor: page.end_cursor,
                },
                totalCount: page.total_count as i32,
            };
            Ok(connection.gql_field_value(context()))
        })
    })
    .argument(InputValue::new("after", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new("before", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new("filter", TypeRef::named("AgentDirectoryFilter")))
    .argument(InputValue::new("first", TypeRef::named(TypeRef::INT)).default_value(25i32))
    .argument(InputValue::new("last", TypeRef::named(TypeRef::INT)))
}

/// This module's contribution to the schema: the complex `Project` object (data fields + `agents`),
/// `Agent`/`AgentConnection`, `CostSummary`/`ProjectDashboard`, and `project(id)`/`projectDashboard(id)`.
pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_query::<ProjectQueries>();
    builder
        .outputs
        .push(Project::basic_object(context()).field(agents_field()));
    builder.register_custom_output::<ProjectEdge>();
    builder.register_custom_output::<ProjectConnection>();
    builder.register_custom_input::<AgentDirectoryFilter>();
    builder.register_custom_output::<Agent>();
    builder.register_custom_output::<AgentEdge>();
    builder.register_custom_output::<AgentConnection>();
    builder.register_custom_output::<CostSummary>();
    builder.register_custom_output::<ProjectDashboard>();
}
