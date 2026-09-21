//! The two SQLSTATE classifications the write paths retry or report on.
//!
//! These take a `&sea_orm::DbErr` and answer a boolean: they are error handling, not SQL. They
//! live next to the commands that branch on them — `agent::draft`, `configuration::mutations`,
//! `deployment::mutations`, `evaluation::mutations` and the two `rows` modules — which is why
//! they moved here when `sql.rs` was deleted (plan, G6). `agent::draft`'s module doc comment
//! explains the pattern they serve: a write path that locks a row `FOR UPDATE` and then writes
//! to it, relying on Aurora DSQL's commit-time SQLSTATE 40001 rather than a `WHERE`-clause
//! revision check.

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
