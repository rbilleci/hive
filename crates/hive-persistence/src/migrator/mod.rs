mod sql;
mod steps;

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbErr, QueryResult, RuntimeErr, Statement, Value,
};
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
    Database(#[from] DbErr),
    #[error("could not acquire the migration lock within {0:?}")]
    LockTimeout(Duration),
    #[error("Aurora DSQL asynchronous job {job_id} failed: {detail}")]
    AsyncJobFailed { job_id: String, detail: String },
    #[error("Aurora DSQL asynchronous job {0} did not complete within {1:?}")]
    AsyncJobTimeout(String, Duration),
}

/// The SQLSTATE of a failed statement, when the underlying error is a Postgres error sea-orm
/// passed through from its vendored `sqlx`. Same extraction as `retry::is_serialization_failure_db`,
/// here for the dialect probe's `42P01` rather than `40001`; `DbErr::sql_err()` classifies neither,
/// only `23505`/`23503`.
fn sqlstate(error: &DbErr) -> Option<String> {
    let (DbErr::Exec(RuntimeErr::SqlxError(inner)) | DbErr::Query(RuntimeErr::SqlxError(inner))) =
        error
    else {
        return None;
    };
    match inner.as_ref() {
        sea_orm::sqlx::Error::Database(database_error) => {
            database_error.code().map(|code| code.into_owned())
        }
        _ => None,
    }
}

/// The two database dialects the migrator supports. The Java
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

async fn probe_dialect(db: &DatabaseConnection) -> Result<Dialect, MigratorError> {
    match db
        .execute_unprepared("SELECT 1 FROM sys.jobs LIMIT 0")
        .await
    {
        Ok(_) => Ok(Dialect::AuroraDsql),
        Err(error) if sqlstate(&error).as_deref() == Some("42P01") => Ok(Dialect::Postgres),
        Err(other) => Err(other.into()),
    }
}

/// Applies every migration and seed file in the order `DatabaseMigrator.migrateLocked`
/// hardcodes, then returns. Safe to call from every process that touches the
/// database (`serve`, `deployment-worker`, `evaluation-worker`, and the standalone
/// `migrate` subcommand), matching `DatabaseLifecycle`'s startup ordering guarantee.
pub async fn migrate_and_seed(db: &DatabaseConnection) -> Result<(), MigratorError> {
    ensure_control_tables(db).await?;
    let dialect = probe_dialect(db).await?;
    info!(?dialect, "migrator: dialect detected");

    acquire_lock(db).await?;
    let result = run_steps(db, dialect).await;
    release_lock(db).await?;
    result
}

async fn ensure_control_tables(db: &DatabaseConnection) -> Result<(), MigratorError> {
    db.execute_unprepared(
        "CREATE TABLE IF NOT EXISTS hive_schema_migrations (\
            version TEXT PRIMARY KEY, \
            applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP)",
    )
    .await?;
    db.execute_unprepared(
        "CREATE TABLE IF NOT EXISTS hive_schema_migration_lock (\
            id INTEGER PRIMARY KEY, \
            locked_at TIMESTAMPTZ NOT NULL)",
    )
    .await?;
    Ok(())
}

async fn acquire_lock(db: &DatabaseConnection) -> Result<(), MigratorError> {
    let deadline = Instant::now() + LOCK_WAIT_TIMEOUT;
    loop {
        let claimed = db
            .execute_unprepared(&format!(
                "INSERT INTO hive_schema_migration_lock (id, locked_at) VALUES (1, CURRENT_TIMESTAMP) \
                 ON CONFLICT (id) DO UPDATE SET locked_at = CURRENT_TIMESTAMP \
                 WHERE hive_schema_migration_lock.locked_at < CURRENT_TIMESTAMP - INTERVAL '{LOCK_STALE_AFTER_SECONDS} seconds'"
            ))
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

async fn release_lock(db: &DatabaseConnection) -> Result<(), MigratorError> {
    db.execute_unprepared("DELETE FROM hive_schema_migration_lock WHERE id = 1")
        .await?;
    Ok(())
}

async fn run_steps(db: &DatabaseConnection, dialect: Dialect) -> Result<(), MigratorError> {
    let mut async_jobs: Vec<String> = Vec::new();

    for step in steps::steps() {
        let already_applied = match &step.gate {
            Gate::Unconditional => false,
            Gate::Ledgered(version) => is_applied(db, version).await?,
        };
        if already_applied {
            continue;
        }

        info!(step = step.label, "migrator: applying");
        for statement in sql::split_statements(step.sql) {
            run_statement(db, &statement, dialect, &mut async_jobs).await?;
        }

        if let Gate::Ledgered(version) = &step.gate {
            mark_applied(db, version).await?;
        }
    }

    await_async_jobs(db, dialect, async_jobs).await
}

async fn is_applied(db: &DatabaseConnection, version: &str) -> Result<bool, MigratorError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT 1 FROM hive_schema_migrations WHERE version = $1",
        [Value::from(version)],
    );
    Ok(db.query_one_raw(statement).await?.is_some())
}

async fn mark_applied(db: &DatabaseConnection, version: &str) -> Result<(), MigratorError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO hive_schema_migrations (version) VALUES ($1)",
        [Value::from(version)],
    );
    db.execute_raw(statement).await?;
    Ok(())
}

async fn run_statement(
    db: &DatabaseConnection,
    statement: &str,
    dialect: Dialect,
    async_jobs: &mut Vec<String>,
) -> Result<(), MigratorError> {
    if sql::is_create_index_statement(statement) {
        return run_create_index(db, statement, dialect, async_jobs).await;
    }

    if let Some(check) = sql::match_add_check_constraint(statement) {
        return run_add_check_constraint(db, statement, &check, dialect, async_jobs).await;
    }

    db.execute_unprepared(statement).await?;
    Ok(())
}

/// The one column a migration-triggered async job's id comes back as. `query_one` returns
/// `Option<QueryResult>`, unwrapped here because Aurora DSQL's `ASYNC` form always returns exactly
/// one row when the statement itself did not error.
async fn fetch_job_id(db: &DatabaseConnection, statement: &str) -> Result<String, MigratorError> {
    let row = db
        .query_one_raw(Statement::from_string(db.get_database_backend(), statement))
        .await?
        .expect("an ASYNC statement returns exactly one row (the job id) when it does not error");
    Ok(row.try_get_by::<String, _>(0)?)
}

async fn run_create_index(
    db: &DatabaseConnection,
    statement: &str,
    dialect: Dialect,
    async_jobs: &mut Vec<String>,
) -> Result<(), MigratorError> {
    match dialect {
        Dialect::Postgres => {
            db.execute_unprepared(statement).await?;
        }
        Dialect::AuroraDsql => {
            let rewritten = sql::strip_index_sort_order(&sql::inject_async_keyword(statement));
            async_jobs.push(fetch_job_id(db, &rewritten).await?);
        }
    }
    Ok(())
}

async fn run_add_check_constraint(
    db: &DatabaseConnection,
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
    db.execute_unprepared(&format!(
        "ALTER TABLE {} DROP CONSTRAINT IF EXISTS {}",
        check.table, check.name
    ))
    .await?;

    match dialect {
        Dialect::Postgres => {
            db.execute_unprepared(statement).await?;
        }
        Dialect::AuroraDsql => {
            db.execute_unprepared(&sql::ensure_not_valid(statement))
                .await?;
            let job_id = fetch_job_id(
                db,
                &format!(
                    "ALTER TABLE ASYNC {} VALIDATE CONSTRAINT {}",
                    check.table, check.name
                ),
            )
            .await?;
            async_jobs.push(job_id);
        }
    }
    Ok(())
}

async fn await_async_jobs(
    db: &DatabaseConnection,
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
            let statement = Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT status, details FROM sys.jobs WHERE job_id = $1",
                [Value::from(job_id.clone())],
            );
            let row: QueryResult = db
                .query_one_raw(statement)
                .await?
                .expect("sys.jobs always has a row for a job id this process itself created");
            let status: String = row.try_get_by(0)?;
            match status.as_str() {
                "SUCCEEDED" | "COMPLETED" => break,
                "FAILED" | "CANCELLED" => {
                    let detail: String = row.try_get_by(1).unwrap_or_default();
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
