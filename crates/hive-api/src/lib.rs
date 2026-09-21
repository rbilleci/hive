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
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use telemetry::GraphqlTelemetry;

pub use state::AppState;

/// Renders the full GraphQL SDL without requiring a reachable database: schema composition only
/// registers types/resolvers and never runs a query, so a lazily-connecting connection (one that
/// never actually dials Postgres) is enough. Lets `hive schema-sdl` (and any script comparing this
/// output against Java's committed schema for functional equivalence) run standalone.
///
/// `.connect_lazy` keeps this from dialing PostgreSQL, and `Builder::register_entity` only ever
/// calls `connection.get_database_backend()` while composing the schema, which a lazy
/// connection answers.
pub async fn schema_sdl() -> String {
    let mut options = sea_orm::ConnectOptions::new("postgres://unused@127.0.0.1/unused");
    options.connect_lazy(true);
    let db = sea_orm::Database::connect(options)
        .await
        .expect("a lazy sea-orm connection never dials PostgreSQL to construct");
    schema::sdl(&schema::build(db))
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

fn build_state(db: sea_orm::DatabaseConnection, signing_key: String) -> AppState {
    AppState {
        session_verifier: auth::SessionVerifier::new(signing_key),
        approval_maintenance: ApprovalMaintenanceState::default(),
        telemetry: Arc::new(GraphqlTelemetry::default()),
        schema: schema::build(db.clone()),
        db,
        web_dist: PathBuf::from(env_string("HIVE_WEB_DIST", "crates/hive-console/dist")),
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

/// Assembles the router (explicit routes plus `spa`'s browser-history fallback) and serves it on
/// `HIVE_BIND_ADDRESS`:`HIVE_PORT`, defaulting to `127.0.0.1:8080`.
pub async fn serve(
    connections: ConnectionFactory,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let signing_key = std::env::var("HIVE_IDENTITY_SIGNING_KEY")
        .map_err(|_| anyhow::anyhow!("HIVE_IDENTITY_SIGNING_KEY is required."))?;
    let state = build_state(connections.dynamic().clone(), signing_key);
    let web_dist_for_log = state.web_dist.clone();
    tokio::spawn(maintenance::run(
        state.db.clone(),
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
    .with_graceful_shutdown(shutdown)
    .await?;
    Ok(())
}

/// Builds an `AppState` for tests, reading the same `HIVE_WEB_DIST` /
/// `HIVE_LOCAL_AUTOLOGIN_ENABLED` / `HIVE_DEPLOYMENT_WORKER_STALE_MILLIS`
/// environment variables `serve` does, over the test database's URL, and a signing key.
pub async fn test_state(database_url: &str, signing_key: &str) -> AppState {
    let db = connect_test_dynamic(database_url).await;
    build_state(db, signing_key.to_string())
}

async fn connect_test_dynamic(database_url: &str) -> sea_orm::DatabaseConnection {
    // Lazy: a concurrent test run opening one eager connection per test instead of a lazy one
    // exhausted the local Postgres connection limit. A lazy connection still opens correctly on
    // the first real query a test sends.
    let mut options = sea_orm::ConnectOptions::new(database_url);
    options.connect_lazy(true);
    sea_orm::Database::connect(options)
        .await
        .expect("connect to the test database")
}
