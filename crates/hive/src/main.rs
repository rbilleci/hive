use clap::{Parser, Subcommand};
use hive_persistence::ConnectionFactory;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "hive")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    #[command(flatten)]
    database: DatabaseArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Applies every migration and seed file, then exits.
    Migrate,
    /// Runs the migrator, then serves HTTP.
    Serve,
    /// Runs the migrator, then drains the deployment outbox.
    DeploymentWorker,
    /// Runs the migrator, then drains the evaluation outbox.
    EvaluationWorker,
    /// Prints the GraphQL SDL to stdout and exits. Needs no reachable database.
    SchemaSdl,
}

#[derive(Parser)]
struct DatabaseArgs {
    #[arg(
        long,
        env = "HIVE_DATABASE_URL",
        default_value = "jdbc:postgresql://127.0.0.1:5432/hive"
    )]
    database_url: String,
    #[arg(long, env = "HIVE_DATABASE_USER", default_value = "hive")]
    database_user: String,
    #[arg(long, env = "HIVE_DATABASE_PASSWORD", default_value = "hive")]
    database_password: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    if matches!(cli.command, Command::SchemaSdl) {
        print!("{}", hive_api::schema_sdl());
        return Ok(());
    }

    let db = cli.database;
    let connections =
        ConnectionFactory::connect(&db.database_url, &db.database_user, &db.database_password)
            .await?;

    match cli.command {
        Command::Migrate => {
            hive_persistence::migrate_and_seed(connections.pool()).await?;
            tracing::info!("migrate: complete");
        }
        Command::Serve => {
            hive_persistence::migrate_and_seed(connections.pool()).await?;
            hive_api::serve(connections).await?;
        }
        Command::DeploymentWorker => {
            hive_persistence::migrate_and_seed(connections.pool()).await?;
            run_deployment_worker(connections.pool().clone()).await;
        }
        Command::EvaluationWorker => {
            hive_persistence::migrate_and_seed(connections.pool()).await?;
            run_evaluation_worker(connections.pool().clone()).await;
        }
        Command::SchemaSdl => unreachable!("handled before a database connection is opened"),
    }

    Ok(())
}

fn env_value(name: &str, fallback: &str) -> String {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => fallback.to_string(),
    }
}

fn positive_millis(name: &str, fallback: u64) -> u64 {
    env_value(name, &fallback.to_string())
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .map(|value| value as u64)
        .unwrap_or(fallback)
}

/// The compatibility claim gate identifies an M14 process by this generated heartbeat subject. A
/// retained M13 worker sharing the configured prefix cannot impersonate this process.
fn configured_m14_worker_id(configured: &str) -> String {
    let mut prefix = configured.trim().to_string();
    if prefix.is_empty() {
        prefix = "local-deployment-worker".to_string();
    }
    let suffix = format!("-{}", uuid::Uuid::new_v4());
    let keep = prefix
        .chars()
        .count()
        .min(120usize.saturating_sub(suffix.chars().count()));
    let truncated_prefix: String = prefix.chars().take(keep).collect();
    format!("{truncated_prefix}{suffix}")
}

/// Ports `LocalDeploymentWorkerServer.main`: RTD-WORKER-PARITY requires this exact stdout line
/// (`hive/scripts/local-service.mjs` blocks on it), the same pre/post-batch heartbeat pair, and the
/// same backoff formula. Runs forever — this subcommand's entire purpose.
async fn run_deployment_worker(pool: sqlx::PgPool) {
    let worker_id = configured_m14_worker_id(&env_value(
        "HIVE_DEPLOYMENT_WORKER_ID",
        "local-deployment-worker",
    ));
    // Two handles over the same pool: `worker` drives `runBatch`'s delivery loop, `heartbeats` is the
    // separate `record_worker_heartbeat`/`repair_pending_delivery_audits` surface Java exposes as
    // plain instance methods beyond the `DeploymentOutboxDelivery` trait `LocalDeploymentOutboxWorker`
    // is generic over.
    let heartbeats = hive_persistence::deployment::PgDeploymentRepository::new(pool.clone());
    let worker = hive_application::deployment::LocalDeploymentOutboxWorker::new(
        hive_persistence::deployment::PgDeploymentRepository::new(pool),
        worker_id.clone(),
    );
    let base_delay = positive_millis("HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS", 100);
    let mut delay = positive_millis("HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS", base_delay);

    println!("component=local-deployment-worker event=started workerId={worker_id}");

    loop {
        tokio::time::sleep(Duration::from_millis(delay)).await;
        delay = tick(&worker, &heartbeats, &worker_id, base_delay, delay).await;
    }
}

async fn tick(
    worker: &hive_application::deployment::LocalDeploymentOutboxWorker<
        hive_persistence::deployment::PgDeploymentRepository,
    >,
    heartbeats: &hive_persistence::deployment::PgDeploymentRepository,
    worker_id: &str,
    base_delay: u64,
    current_delay: u64,
) -> u64 {
    if let Err(error) = heartbeats
        .record_worker_heartbeat(worker_id, 0, true, None)
        .await
    {
        tracing::warn!(%error, "deployment worker pre-batch heartbeat failed");
    }
    match worker.run_batch(50).await {
        Ok(delivered) => {
            if let Err(error) = heartbeats
                .record_worker_heartbeat(worker_id, delivered as i32, true, None)
                .await
            {
                tracing::warn!(%error, "deployment worker post-batch heartbeat failed");
            }
            if delivered == 50 {
                0
            } else if delivered > 0 {
                base_delay
            } else {
                base_delay.saturating_mul(4).min(5_000)
            }
        }
        Err(error) => {
            let next = base_delay.max(current_delay).saturating_mul(2).min(30_000);
            if let Err(heartbeat_error) = heartbeats
                .record_worker_heartbeat(worker_id, 0, false, Some("LOCAL_WORKER_DELIVERY_FAILED"))
                .await
            {
                tracing::warn!(error = %heartbeat_error, "deployment worker failure heartbeat failed");
            }
            eprintln!("component=local-deployment-worker event=batch-failed workerId={worker_id} failureCode=LOCAL_WORKER_DELIVERY_FAILED retryMillis={next} error={error}");
            next
        }
    }
}

/// Ports `LocalEvaluationWorkerServer.main`. Differs from `run_deployment_worker` in several
/// precise ways `LocalEvaluationWorkerServer.java` establishes (compared line-for-line against
/// `LocalDeploymentWorkerServer.java`): no UUID-suffixed worker id (no M13/M14 compatibility claim
/// concept exists for this domain), no separate initial-delay env var (the very first tick uses
/// `interval` directly), and — most importantly — no heartbeat call in this outer loop at all:
/// `LocalEvaluationWorker::run_once` (via `EvaluationWorkStore`'s `idle`/`delivered`/`failed`/
/// `claim_failed` methods) already records every heartbeat per delivered item, not per batch. The
/// stdout `event=started` line is cosmetic parity only — `hive/scripts/local-service.mjs`'s
/// `startLocalEvaluationWorker` gates readiness on a fresh `READY` row in
/// `evaluation_worker_heartbeats`, not on this line.
async fn run_evaluation_worker(pool: sqlx::PgPool) {
    let worker_id = env_value("HIVE_EVALUATION_WORKER_ID", "local-evaluation-worker");
    let interval = positive_millis("HIVE_EVALUATION_WORKER_INTERVAL_MILLIS", 100);
    let worker = hive_application::evaluation::LocalEvaluationWorker::new(
        hive_persistence::evaluation::PgEvaluationWorkStore::new(pool),
        Box::new(hive_application::evaluation::LocalPromptCaseFixtureAdapter),
        worker_id,
    );

    println!("component=local-evaluation-worker event=started");

    let mut delay = interval;
    loop {
        tokio::time::sleep(Duration::from_millis(delay)).await;
        delay = evaluation_tick(&worker, interval, delay).await;
    }
}

async fn evaluation_tick(
    worker: &hive_application::evaluation::LocalEvaluationWorker<
        hive_persistence::evaluation::PgEvaluationWorkStore,
    >,
    interval: u64,
    current_delay: u64,
) -> u64 {
    match worker.run_batch(50).await {
        Ok(_) => interval,
        Err(error) => {
            let next = interval.max(current_delay).saturating_mul(2).min(5_000);
            eprintln!(
                "component=local-evaluation-worker event=batch-failed failureCode=RUNNER_FAILED"
            );
            tracing::warn!(%error, "evaluation worker batch failed");
            next
        }
    }
}
