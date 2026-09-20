//! Ports `PostgresProjectDashboardRepository`: a single-row read from the
//! `project_dashboard_projection` view (created in migration V004, already
//! COALESCE-defaulted and cost-availability-gated), inner-joined against an
//! active `organization_memberships` row for the tenant check.

use hive_application::project::{
    ProjectCostSummary, ProjectDashboard, ProjectDashboardRepository,
    ProjectDashboardRepositoryError as RepositoryError,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub struct PgProjectDashboardRepository {
    pool: PgPool,
}

impl PgProjectDashboardRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ProjectDashboardRepository for PgProjectDashboardRepository {
    async fn find_dashboard(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
    ) -> Result<Option<ProjectDashboard>, RepositoryError> {
        let row = sqlx::query(
            "SELECT dashboard.project_id, dashboard.slug, dashboard.display_name, dashboard.lifecycle_status, \
                    dashboard.active_agents, dashboard.active_deployments, dashboard.failed_deployments, \
                    dashboard.pending_approvals, dashboard.unhealthy_resources, dashboard.cost_availability, \
                    dashboard.current_period_cost_cents, dashboard.cost_period_start, dashboard.cost_period_end, \
                    dashboard.cost_currency, dashboard.cost_data_as_of \
             FROM project_dashboard_projection dashboard \
             INNER JOIN organization_memberships membership \
               ON membership.organization_id = dashboard.organization_id \
             WHERE dashboard.project_id = $1 \
               AND membership.principal_id = $2 \
               AND membership.started_at <= CURRENT_TIMESTAMP \
               AND membership.ended_at IS NULL",
        )
        .bind(project_id)
        .bind(principal_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;

        Ok(row.map(|row| ProjectDashboard {
            id: row.get("project_id"),
            slug: row.get("slug"),
            display_name: row.get("display_name"),
            lifecycle_status: row.get("lifecycle_status"),
            active_agents: row.get("active_agents"),
            active_deployments: row.get("active_deployments"),
            failed_deployments: row.get("failed_deployments"),
            pending_approvals: row.get("pending_approvals"),
            unhealthy_resources: row.get("unhealthy_resources"),
            current_period_cost: ProjectCostSummary {
                availability: row.get("cost_availability"),
                period_start: row.get("cost_period_start"),
                period_end: row.get("cost_period_end"),
                currency: row.get("cost_currency"),
                amount_cents: row.get("current_period_cost_cents"),
                data_as_of: row.get("cost_data_as_of"),
            },
        }))
    }
}
