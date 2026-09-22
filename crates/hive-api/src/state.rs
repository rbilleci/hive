use crate::auth::SessionVerifier;
use crate::telemetry::GraphqlTelemetry;
use hive_persistence::ApprovalMaintenanceState;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    /// Held here too, not only inside `schema`, so a future non-GraphQL consumer (a health probe,
    /// a maintenance task) can reach it directly.
    pub db: sea_orm::DatabaseConnection,
    pub session_verifier: SessionVerifier,
    pub approval_maintenance: ApprovalMaintenanceState,
    pub telemetry: Arc<GraphqlTelemetry>,
    pub schema: async_graphql::dynamic::Schema,
    pub web_dist: PathBuf,
    pub local_dev_autologin_enabled: bool,
    pub deployment_worker_stale_millis: i64,
}
