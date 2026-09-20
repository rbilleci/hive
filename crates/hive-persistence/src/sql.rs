//! Small SQL/JSONB helpers shared by any repository that stores a flat list
//! of strings as JSONB (Aurora DSQL has no array type) and by any repository
//! whose write path locks a row `FOR UPDATE` and then writes to it
//! unconditionally, relying on DSQL's commit-time SQLSTATE 40001 rather than
//! a WHERE-clause revision check — see `agent::draft`'s module doc comment
//! for the full explanation of that pattern, first established there.

/// A JSONB array of plain strings with no embedded quotes or commas, decoded
/// without a JSON library. Matches every Java `strings(String json)` helper
/// in this codebase (`PostgresAgentDraftRepository`, `PostgresConfigurationRepository`).
///
/// Trims each element after splitting: PostgreSQL's `jsonb` output always
/// inserts a space after every comma when cast to `text` (confirmed against
/// the real cluster — `'["a","b"]'::jsonb::text` reads back as `["a", "b"]`),
/// regardless of how the value was written, so every element but the first
/// would otherwise carry a leading space.
pub fn parse_string_array(json: &str) -> Vec<String> {
    if json.len() < 3 {
        return Vec::new();
    }
    json[1..json.len() - 1]
        .replace('"', "")
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .collect()
}

/// The inverse of [`parse_string_array`]. Matches every Java `jsonArray(List<String>)` helper.
pub fn json_array(values: &[String]) -> String {
    let quoted: Vec<String> = values
        .iter()
        .map(|value| format!("\"{}\"", value.replace('"', "\\\"")))
        .collect();
    format!("[{}]", quoted.join(","))
}

/// `true` for Postgres SQLSTATE 40001 (serialization failure) — the error
/// Aurora DSQL's optimistic concurrency control returns at commit when a
/// `FOR UPDATE` read this transaction took was invalidated by a concurrent
/// writer, in place of a lock that would have blocked that writer instead.
/// `DbErr::sql_err()` only classifies `23505`/`23503`, not `40001`, so this extracts the SQLSTATE
/// the same way the migrator's own `sqlstate` helper does (`GSR-FACT-SEAORM-OCC`'s pattern).
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
