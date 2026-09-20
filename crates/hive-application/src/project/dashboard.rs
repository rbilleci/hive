use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Governed current-period cost facts that distinguish available data from
/// unavailable or unknown data. Ports `ProjectCostSummary` (the SDL's `CostSummary`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCostSummary {
    pub availability: String,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    pub currency: Option<String>,
    pub amount_cents: Option<i32>,
    pub data_as_of: Option<DateTime<Utc>>,
}

/// Browser-safe, server-owned project identity and aggregate metrics for the
/// dashboard route. Ports `ProjectDashboard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDashboard {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub active_agents: i32,
    pub active_deployments: i32,
    pub failed_deployments: i32,
    pub pending_approvals: i32,
    pub unhealthy_resources: i32,
    pub current_period_cost: ProjectCostSummary,
}
