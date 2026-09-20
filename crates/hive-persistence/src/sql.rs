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
pub fn is_serialization_failure(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db_error) if db_error.code().as_deref() == Some("40001"))
}
