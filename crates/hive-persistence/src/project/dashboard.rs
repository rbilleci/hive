//! Ports `PostgresProjectDashboardRepository`: a single-row read from the
//! `project_dashboard_projection` view (created in migration V004, already
//! COALESCE-defaulted and cost-availability-gated), inner-joined against an
//! active `organization_memberships` row for the tenant check.
//!
//! `GSR-PERSISTENCE`: runs through `sea_orm::ConnectionTrait` via
//! `Statement::from_sql_and_values` + `query_one_raw`, preserving the original SQL text verbatim
//! (same idiom as `capability`/`console`, `GSR-PHASE-P5`).

use hive_application::project::{
    ProjectCostSummary, ProjectDashboard, ProjectDashboardRepository,
    ProjectDashboardRepositoryError as RepositoryError,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use uuid::Uuid;

pub struct PgProjectDashboardRepository {
    db: DatabaseConnection,
}

impl PgProjectDashboardRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl ProjectDashboardRepository for PgProjectDashboardRepository {
    async fn find_dashboard(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
    ) -> Result<Option<ProjectDashboard>, RepositoryError> {
        let statement = Statement::from_sql_and_values(
            self.db.get_database_backend(),
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
            [project_id.into(), principal_id.into()],
        );
        let row = self
            .db
            .query_one_raw(statement)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;

        row.map(|row| {
            Ok(ProjectDashboard {
                id: row.try_get_by("project_id")?,
                slug: row.try_get_by("slug")?,
                display_name: row.try_get_by("display_name")?,
                lifecycle_status: row.try_get_by("lifecycle_status")?,
                active_agents: row.try_get_by("active_agents")?,
                active_deployments: row.try_get_by("active_deployments")?,
                failed_deployments: row.try_get_by("failed_deployments")?,
                pending_approvals: row.try_get_by("pending_approvals")?,
                unhealthy_resources: row.try_get_by("unhealthy_resources")?,
                current_period_cost: ProjectCostSummary {
                    availability: row.try_get_by("cost_availability")?,
                    period_start: row.try_get_by("cost_period_start")?,
                    period_end: row.try_get_by("cost_period_end")?,
                    currency: row.try_get_by("cost_currency")?,
                    amount_cents: row.try_get_by("current_period_cost_cents")?,
                    data_as_of: row.try_get_by("cost_data_as_of")?,
                },
            })
        })
        .transpose()
        .map_err(|error: sea_orm::DbErr| RepositoryError::Other(error.into()))
    }
}
