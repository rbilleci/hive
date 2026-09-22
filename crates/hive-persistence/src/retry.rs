//! The two SQLSTATE classifications the write paths retry or report on, and the lost-race
//! reporting they share.
//!
//! The classifications take a `&sea_orm::DbErr` and answer a boolean: they are error handling,
//! not SQL. They live next to the commands that branch on them — `agent::mutations`,
//! `configuration::mutations`, `deployment::mutations`, `evaluation::mutations` and the two
//! `rows` modules — which is why they moved here when `sql.rs` was deleted (plan, G6).
//! `agent::mutations`'s module doc comment explains the pattern they serve: a write path that locks a
//! row `FOR UPDATE` and then writes to it, relying on Aurora DSQL's commit-time SQLSTATE 40001
//! rather than a `WHERE`-clause revision check.
//!
//! `committed` and `reread` are the two halves of what a command does when it loses that race.
//! They differ in where the 40001 surfaces, which is not the same at every call site: a command
//! that runs its whole body inline reaches `committed`, while one that runs the body in a
//! separate `_tx` function and matches on its result already holds the error and needs only
//! `reread`.

use sea_orm::{DatabaseConnection, DatabaseTransaction, DbErr, TransactionTrait};

/// Reads, in a transaction of its own, the state the winner of a lost race left behind.
///
/// The transaction that lost is spent — every statement on it fails — so the revision a conflict
/// reports has to come from a new one. A read that fails leaves the transaction to roll back on
/// drop and reports the failure; the caller was already answering a conflict, not retrying.
pub async fn reread<R>(
    db: &DatabaseConnection,
    read: impl AsyncFnOnce(&DatabaseTransaction) -> Result<R, DbErr>,
) -> Result<R, DbErr> {
    let txn = db.begin().await?;
    let value = read(&txn).await?;
    txn.commit().await?;
    Ok(value)
}

/// Commits `txn`, answering `Some(reread(…))` when the commit lost to SQLSTATE 40001 and `None`
/// when it did not.
///
/// Aurora DSQL reports a lost optimistic race at `commit`, where PostgreSQL would have blocked
/// the loser on the row lock it took instead. `Some` therefore carries whatever the caller needs
/// to phrase its own revision conflict — the commands differ in which row they re-read and in
/// which refusal they build, not in this control flow.
pub async fn committed<R>(
    db: &DatabaseConnection,
    txn: DatabaseTransaction,
    raced: impl AsyncFnOnce(&DatabaseTransaction) -> Result<R, DbErr>,
) -> Result<Option<R>, DbErr> {
    match txn.commit().await {
        Ok(()) => Ok(None),
        Err(error) if is_serialization_failure_db(&error) => reread(db, raced).await.map(Some),
        Err(error) => Err(error),
    }
}

/// `true` for Postgres SQLSTATE 40001 (serialization failure) — the error
/// Aurora DSQL's optimistic concurrency control returns at commit when a
/// `FOR UPDATE` read this transaction took was invalidated by a concurrent
/// writer, in place of a lock that would have blocked that writer instead.
/// `DbErr::sql_err()` only classifies `23505`/`23503`, not `40001`, so this extracts the SQLSTATE
/// the same way the migrator's own `sqlstate` helper does.
pub fn is_serialization_failure_db(error: &sea_orm::DbErr) -> bool {
    use sea_orm::RuntimeErr;
    let (sea_orm::DbErr::Exec(RuntimeErr::SqlxError(inner))
    | sea_orm::DbErr::Query(RuntimeErr::SqlxError(inner))) = error
    else {
        return false;
    };
    matches!(
        inner.as_ref(),
        sea_orm::sqlx::Error::Database(database_error) if database_error.code().as_deref() == Some("40001")
    )
}

/// `true` for Postgres SQLSTATE 23505 (unique constraint violation), via `sea_orm`'s own portable
/// `DbErr::sql_err()` classification.
pub fn is_unique_violation_db(error: &sea_orm::DbErr) -> bool {
    matches!(
        error.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    )
}
