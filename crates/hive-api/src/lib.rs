mod auth;
mod graphql;
mod health;
mod local_dev_login;
mod maintenance;
mod schema;
mod spa;
mod state;
mod telemetry;

use axum::routing::{get, post};
use axum::Router;
use hive_persistence::{ApprovalMaintenanceState, ConnectionFactory};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use telemetry::GraphqlTelemetry;

pub use state::AppState;

/// Renders the full GraphQL SDL without requiring a reachable database: schema composition only
/// registers types/resolvers and never runs a query, so a lazily-connecting pool (one that never
/// actually dials Postgres) is enough. Lets `hive schema-sdl` (and any script comparing this
/// output against Java's committed schema for functional equivalence) run standalone.
pub fn schema_sdl() -> String {
    let pool = PgPool::connect_lazy("postgres://unused@127.0.0.1/unused")
        .expect("a syntactically valid postgres:// URL never fails to construct a lazy pool");
    schema::build_schema(pool).sdl()
}

fn env_string(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|value| value == "true")
        .unwrap_or(default)
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn state_from_env(pool: PgPool, signing_key: String) -> AppState {
    AppState {
        session_verifier: auth::SessionVerifier::new(signing_key),
        approval_maintenance: ApprovalMaintenanceState::default(),
        telemetry: Arc::new(GraphqlTelemetry::default()),
        schema: schema::build_schema(pool.clone()),
        pool,
        web_dist: PathBuf::from(env_string("HIVE_WEB_DIST", "../hive/web/dist")),
        local_dev_autologin_enabled: env_bool("HIVE_LOCAL_AUTOLOGIN_ENABLED", false),
        deployment_worker_stale_millis: env_i64("HIVE_DEPLOYMENT_WORKER_STALE_MILLIS", 15_000),
    }
}

/// Builds the full route table over an already-constructed `AppState`. Exposed so
/// tests can drive the router in-process with `tower::ServiceExt::oneshot`, with no
/// socket bind required.
pub fn build_router(state: AppState) -> Router {
    let router: Router<AppState> = Router::new()
        .route("/graphql", post(graphql::graphql))
        .route("/health", get(health::health))
        .route(
            "/health/deployment-worker",
            get(health::deployment_worker_status),
        )
        .route(
            "/health/evaluation-worker",
            get(health::evaluation_worker_status),
        )
        .route("/local-dev/login", get(local_dev_login::login));
    let router = spa::merge_spa_routes(router, &state.web_dist);
    router.with_state(state)
}

/// Assembles the router and serves it. `RTD-BIND`, `RTD-SPA-SERVING`.
pub async fn serve(connections: ConnectionFactory) -> anyhow::Result<()> {
    let signing_key = std::env::var("HIVE_IDENTITY_SIGNING_KEY")
        .map_err(|_| anyhow::anyhow!("HIVE_IDENTITY_SIGNING_KEY is required."))?;
    let state = state_from_env(connections.pool().clone(), signing_key);
    let web_dist_for_log = state.web_dist.clone();
    tokio::spawn(maintenance::run(
        state.pool.clone(),
        state.approval_maintenance.clone(),
    ));
    let router = build_router(state);

    let bind_address = env_string("HIVE_BIND_ADDRESS", "127.0.0.1");
    let port: u16 = env_string("HIVE_PORT", "8080").parse().unwrap_or(8080);
    let addr: SocketAddr = format!("{bind_address}:{port}").parse()?;

    tracing::info!(%addr, web_dist = %web_dist_for_log.display(), "hive-api: listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Builds an `AppState` for tests, reading the same `HIVE_WEB_DIST` /
/// `HIVE_LOCAL_AUTOLOGIN_ENABLED` / `HIVE_DEPLOYMENT_WORKER_STALE_MILLIS`
/// environment variables `serve` does, over a caller-supplied pool and signing key.
pub fn test_state(pool: PgPool, signing_key: &str) -> AppState {
    state_from_env(pool, signing_key.to_string())
}
