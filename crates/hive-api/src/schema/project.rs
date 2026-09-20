//! `projectDashboard(id)` and its `ProjectDashboard`/`CostSummary` types. Organizations, projects
//! and agents themselves are read through the generated Seaography entity fields.

use crate::schema::RequestPrincipal;
use hive_domain::java_offset_date_time_string;
use seaography::{CustomFields, CustomOutputType};

#[allow(non_snake_case)]
mod wire {
    use super::*;

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

pub use wire::{CostSummary, ProjectDashboard, ProjectQueries};

/// This module's contribution to the schema: `CostSummary`/`ProjectDashboard` and
/// `projectDashboard(id)`.
pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_query::<ProjectQueries>();
    builder.register_custom_output::<CostSummary>();
    builder.register_custom_output::<ProjectDashboard>();
}
