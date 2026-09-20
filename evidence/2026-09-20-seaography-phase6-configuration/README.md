# GSR-PHASE-P6 (partial): module 9 (configuration) ported onto sea_orm

Status: module 9 (`configuration`) done — the first write-transaction module in the persistence
rewrite. Follows `evidence/2026-09-20-seaography-phase5-directories/` (`GSR-PHASE-P5` completion).

## The key finding: `capability::tx.rs` needs no port at all

The design doc's module-4 text (`GSR-PHASE-P5`) assumed `capability::tx.rs`'s locked,
transaction-scoped primitives would eventually need rewriting onto `sea_orm` for administration/
agent-draft/configuration (module 7-9) to use. Porting `configuration` first proved this
unnecessary: `capability::{mod,queries}.rs` are already generic over `sea_orm::ConnectionTrait`
(from `GSR-PHASE-P5`'s capability rewrite), and `ConnectionTrait` is implemented by *both*
`DatabaseConnection` and `DatabaseTransaction` — unlike `sqlx`, which has no single executor trait
both a bare connection and a transaction satisfy without the lifetime friction `tx.rs`'s own doc
comment describes. This means a write command that opens a `sea_orm::DatabaseTransaction` can call
`capability::has_capability(&txn, ..., lock: true)` or `capability::queries::active_project(&txn,
..., true)` **directly** — the exact same functions the read path already uses, just with
`lock: true` and a transaction reference instead of `lock: false` and a bare connection. No
tx-scoped duplicate is needed, and none was written.

Concretely: `configuration::mutations`'s `configuration_write`/`active_project` local helpers now
call `capability::has_capability`/`capability::queries::active_project` directly, and
`capability::tx::configuration_write` — the one `tx.rs` function only `configuration` ever
called — became entirely dead code the moment this landed; confirmed via `cargo build`'s own
`dead_code` warning, then verified via `grep` that nothing else in the crate called it, then
deleted outright. `capability::mod.rs`'s `mod queries;` was widened to `pub(crate) mod queries;`
(matching `tx`'s existing visibility) so callers outside `capability` can reach its already-generic
primitives — this is the only visibility change this finding required.

`tx.rs` itself is otherwise untouched: `agent::draft` (module 8, not yet ported) still calls its
`active_project`/`legacy_or_developer`/`has_active_project_role`/etc., so those functions stay
exactly as they are until `agent::draft`'s own port removes their last caller too.

## What else this step did

- `configuration::rows.rs` — every function moved from `conn: &mut PgConnection` to
  `db: &impl ConnectionTrait`, same raw-SQL-via-`Statement` idiom as every module since
  `capability`. `mcp_server_from_row` takes `&sea_orm::QueryResult` instead of `sqlx::postgres::
  PgRow`. `is_unique_violation` now delegates to a new `crate::sql::is_unique_violation_db`
  (`sea_orm::DbErr::sql_err()`'s own portable `UniqueConstraintViolation` classification, cleaner
  than hand-parsing SQLSTATE).
- `crate::sql` gained two `DbErr`-flavored twins of its existing `sqlx::Error`-flavored helpers:
  `is_serialization_failure_db` (SQLSTATE 40001, extracted the same way the migrator's own private
  `sqlstate()` helper does — `DbErr::sql_err()` doesn't classify 40001) and `is_unique_violation_db`
  (23505, via `sql_err()` directly). The `sqlx`-flavored originals stay, since `agent::draft`/
  `administration`/`deployment`/`evaluation` (not yet ported) still need them.
- `crate::audit::context` gained `audit_metadata_values() -> Vec<sea_orm::Value>`, a `sea_orm`-
  flavored twin of the existing `bind_audit_metadata` (which chains onto a live `sqlx::query::
  Query` — incompatible with a hand-built `Statement`'s bind list). Same five values
  (`request_id`/`correlation_id`/`graphql_operation`/`source_ip`/`user_agent`), same order,
  explicitly flattening the nested-`Option` cases rather than relying on `sea_query::Value`'s
  `Option<Option<String>>` conversion behavior (unconfirmed; the `sqlx` original relied on sqlx's
  own such behavior, which is a different, unrelated type). This helper is engine-agnostic and
  reusable by every future domain module's audit-event insert (`administration`, `agent::draft`,
  `deployment`, `evaluation` all need the identical five-column suffix).
- `configuration::queries.rs` and `configuration::mutations.rs` fully ported: every dynamic-arity
  SQL statement's bind list built as a `Vec<sea_orm::Value>`, transactions opened via
  `sea_orm::TransactionTrait::begin`, the SQLSTATE-40001-at-commit retry loops (`updateDraft`,
  `validate`, `publish`, `updateMcpServer`, `saveLegacyTool`) preserved exactly, re-reading the
  locked row via a fresh `db.begin()` the same way the `sqlx` original re-read via a fresh
  `pool.begin()`.
- `PgConfigurationRepository` drops its `PgPool` field entirely (added only in `GSR-PHASE-P5` to
  bridge `capability`'s new signature) — `configuration` no longer touches `sqlx` at all. The one
  construction site in `hive-api/src/schema/configuration.rs` updated to match; the now-unused
  `use sqlx::PgPool;` import removed.

## Verification

- `cargo build --workspace --exclude hive-console --tests`, `cargo fmt --all`, `cargo clippy
  --workspace --exclude hive-console --all-targets -- -D warnings`: all clean (after deleting the
  dead `capability::tx::configuration_write`).
- `check:integration:configuration`, `check:e2e:configuration`: both pass — exercising every
  mutation's write path, including the OCC conflict/retry branches.
- `check:rust:database`: 64/64 `http_integration`, 13/13 `capability_integration`, all other suites
  clean.
- `check:architecture`: 22 hive-api source files, 0 findings, unchanged.
