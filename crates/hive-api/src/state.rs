use crate::auth::SessionVerifier;
use crate::schema::HiveSchema;
use crate::telemetry::GraphqlTelemetry;
use hive_persistence::ApprovalMaintenanceState;
use sqlx::PgPool;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub session_verifier: SessionVerifier,
    pub approval_maintenance: ApprovalMaintenanceState,
    pub telemetry: Arc<GraphqlTelemetry>,
    pub schema: HiveSchema,
    pub web_dist: PathBuf,
    pub local_dev_autologin_enabled: bool,
    pub deployment_worker_stale_millis: i64,
}
