mod sql;
mod steps;

use sqlx::{PgPool, Row};
use std::time::{Duration, Instant};
use steps::Gate;
use thiserror::Error;
use tracing::info;

const LOCK_STALE_AFTER_SECONDS: i64 = 60;
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(200);
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const ASYNC_JOB_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const ASYNC_JOB_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Error)]
pub enum MigratorError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("could not acquire the migration lock within {0:?}")]
    LockTimeout(Duration),
    #[error("Aurora DSQL asynchronous job {job_id} failed: {detail}")]
    AsyncJobFailed { job_id: String, detail: String },
    #[error("Aurora DSQL asynchronous job {0} did not complete within {1:?}")]
    AsyncJobTimeout(String, Duration),
}

/// The two database dialects the migrator supports. `RTD-MIGRATOR-PARITY`: the Java
/// migrator this crate ports (`DatabaseMigrator.java`) applies its Aurora DSQL
/// statement rewrites (`CREATE INDEX ASYNC`, the `NOT VALID` + `VALIDATE CONSTRAINT
/// ASYNC` two-phase check-constraint form) unconditionally, which plain PostgreSQL
/// rejects outright. This dialect probe is the one behavioral addition beyond a
/// literal port, so the same migrator and the same unmodified migration files work
/// against both a local PostgreSQL container and real Aurora DSQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    AuroraDsql,
}

async fn probe_dialect(pool: &PgPool) -> Result<Dialect, MigratorError> {
    match sqlx::query("SELECT 1 FROM sys.jobs LIMIT 0")
        .execute(pool)
        .await
    {
        Ok(_) => Ok(Dialect::AuroraDsql),
        Err(sqlx::Error::Database(db_error)) if db_error.code().as_deref() == Some("42P01") => {
            Ok(Dialect::Postgres)
        }
        Err(other) => Err(other.into()),
    }
}

/// Applies every migration and seed file in the order `DatabaseMigrator.migrateLocked`
/// hardcodes, then returns. Safe to call from every process that touches the
/// database (`serve`, `deployment-worker`, `evaluation-worker`, and the standalone
/// `migrate` subcommand), matching `DatabaseLifecycle`'s startup ordering guarantee.
pub async fn migrate_and_seed(pool: &PgPool) -> Result<(), MigratorError> {
    ensure_control_tables(pool).await?;
    let dialect = probe_dialect(pool).await?;
    info!(?dialect, "migrator: dialect detected");

    acquire_lock(pool).await?;
    let result = run_steps(pool, dialect).await;
    release_lock(pool).await?;
    result
}

async fn ensure_control_tables(pool: &PgPool) -> Result<(), MigratorError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS hive_schema_migrations (\
            version TEXT PRIMARY KEY, \
            applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS hive_schema_migration_lock (\
            id INTEGER PRIMARY KEY, \
            locked_at TIMESTAMPTZ NOT NULL)",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn acquire_lock(pool: &PgPool) -> Result<(), MigratorError> {
    let deadline = Instant::now() + LOCK_WAIT_TIMEOUT;
    loop {
        let claimed = sqlx::query(&format!(
            "INSERT INTO hive_schema_migration_lock (id, locked_at) VALUES (1, CURRENT_TIMESTAMP) \
             ON CONFLICT (id) DO UPDATE SET locked_at = CURRENT_TIMESTAMP \
             WHERE hive_schema_migration_lock.locked_at < CURRENT_TIMESTAMP - INTERVAL '{LOCK_STALE_AFTER_SECONDS} seconds'"
        ))
        .execute(pool)
        .await?;

        if claimed.rows_affected() == 1 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(MigratorError::LockTimeout(LOCK_WAIT_TIMEOUT));
        }
        tokio::time::sleep(LOCK_POLL_INTERVAL).await;
    }
}

async fn release_lock(pool: &PgPool) -> Result<(), MigratorError> {
    sqlx::query("DELETE FROM hive_schema_migration_lock WHERE id = 1")
        .execute(pool)
        .await?;
    Ok(())
}

async fn run_steps(pool: &PgPool, dialect: Dialect) -> Result<(), MigratorError> {
    let mut async_jobs: Vec<String> = Vec::new();

    for step in steps::steps() {
        let already_applied = match &step.gate {
            Gate::Unconditional => false,
            Gate::Ledgered(version) => is_applied(pool, version).await?,
        };
        if already_applied {
            continue;
        }

        info!(step = step.label, "migrator: applying");
        for statement in sql::split_statements(step.sql) {
            run_statement(pool, &statement, dialect, &mut async_jobs).await?;
        }

        if let Gate::Ledgered(version) = &step.gate {
            mark_applied(pool, version).await?;
        }
    }

    await_async_jobs(pool, dialect, async_jobs).await
}

async fn is_applied(pool: &PgPool, version: &str) -> Result<bool, MigratorError> {
    let row = sqlx::query("SELECT 1 FROM hive_schema_migrations WHERE version = $1")
        .bind(version)
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

async fn mark_applied(pool: &PgPool, version: &str) -> Result<(), MigratorError> {
    sqlx::query("INSERT INTO hive_schema_migrations (version) VALUES ($1)")
        .bind(version)
        .execute(pool)
        .await?;
    Ok(())
}

async fn run_statement(
    pool: &PgPool,
    statement: &str,
    dialect: Dialect,
    async_jobs: &mut Vec<String>,
) -> Result<(), MigratorError> {
    if sql::is_create_index_statement(statement) {
        return run_create_index(pool, statement, dialect, async_jobs).await;
    }

    if let Some(check) = sql::match_add_check_constraint(statement) {
        return run_add_check_constraint(pool, statement, &check, dialect, async_jobs).await;
    }

    sqlx::query(statement).execute(pool).await?;
    Ok(())
}

async fn run_create_index(
    pool: &PgPool,
    statement: &str,
    dialect: Dialect,
    async_jobs: &mut Vec<String>,
) -> Result<(), MigratorError> {
    match dialect {
        Dialect::Postgres => {
            sqlx::query(statement).execute(pool).await?;
        }
        Dialect::AuroraDsql => {
            let rewritten = sql::strip_index_sort_order(&sql::inject_async_keyword(statement));
            let row = sqlx::query(&rewritten).fetch_one(pool).await?;
            async_jobs.push(row.try_get::<String, _>(0)?);
        }
    }
    Ok(())
}

async fn run_add_check_constraint(
    pool: &PgPool,
    statement: &str,
    check: &sql::CheckConstraint<'_>,
    dialect: Dialect,
    async_jobs: &mut Vec<String>,
) -> Result<(), MigratorError> {
    // Always drop first, on both dialects: a check constraint the CREATE TABLE that
    // introduced this table already declared inline (matching Postgres's own
    // implicit-name convention) collides by name with a later explicit ADD
    // CONSTRAINT of the same shape. The Java migrator applies this drop
    // unconditionally, and it is the mechanism that makes safely re-declaring a
    // constraint across separate migrations (for example, V015, V016, and V016_1
    // each re-asserting `deployments_projection_revision_check`) idempotent.
    sqlx::query(&format!(
        "ALTER TABLE {} DROP CONSTRAINT IF EXISTS {}",
        check.table, check.name
    ))
    .execute(pool)
    .await?;

    match dialect {
        Dialect::Postgres => {
            sqlx::query(statement).execute(pool).await?;
        }
        Dialect::AuroraDsql => {
            sqlx::query(&sql::ensure_not_valid(statement))
                .execute(pool)
                .await?;
            let row = sqlx::query(&format!(
                "ALTER TABLE ASYNC {} VALIDATE CONSTRAINT {}",
                check.table, check.name
            ))
            .fetch_one(pool)
            .await?;
            async_jobs.push(row.try_get::<String, _>(0)?);
        }
    }
    Ok(())
}

async fn await_async_jobs(
    pool: &PgPool,
    dialect: Dialect,
    job_ids: Vec<String>,
) -> Result<(), MigratorError> {
    if dialect == Dialect::Postgres {
        debug_assert!(
            job_ids.is_empty(),
            "PostgreSQL never produces async job ids"
        );
        return Ok(());
    }

    for job_id in job_ids {
        let deadline = Instant::now() + ASYNC_JOB_WAIT_TIMEOUT;
        loop {
            let row = sqlx::query("SELECT status, details FROM sys.jobs WHERE job_id = $1")
                .bind(&job_id)
                .fetch_one(pool)
                .await?;
            let status: String = row.try_get(0)?;
            match status.as_str() {
                "SUCCEEDED" | "COMPLETED" => break,
                "FAILED" | "CANCELLED" => {
                    let detail: String = row.try_get(1).unwrap_or_default();
                    return Err(MigratorError::AsyncJobFailed { job_id, detail });
                }
                _ => {
                    if Instant::now() >= deadline {
                        return Err(MigratorError::AsyncJobTimeout(
                            job_id,
                            ASYNC_JOB_WAIT_TIMEOUT,
                        ));
                    }
                    tokio::time::sleep(ASYNC_JOB_POLL_INTERVAL).await;
                }
            }
        }
    }
    Ok(())
}
