# GSR-PHASE-1, completion: migrator rewrite and connection plumbing

Status: done, with one scope correction to the design document. Together with
`evidence/2026-09-19-seaography-phase1-entities/`, this completes `GSR-PHASE-1`.

## What this step built

- `crates/hive-persistence/src/migrator/mod.rs` fully rewritten from `sqlx::PgPool` onto
  `sea_orm::DatabaseConnection`/`ConnectionTrait`: `execute_unprepared` for plain DDL text,
  `execute_raw`/`query_one_raw` for the two parameterized statements (the migration ledger's
  applied-check and insert), and `try_get_by` for positional row decoding. The Aurora DSQL dialect
  probe now classifies the `42P01` ("relation does not exist") SQLSTATE through the same
  `DbErr::Exec(RuntimeErr::SqlxError(..))` → `sqlx::Error::Database` → `.code()` pattern
  `GSR-OCC` already specifies for `40001`, since `DbErr::sql_err()` classifies neither.
  `steps.rs`/`sql.rs` needed no changes — they were already pure text transformation with no `sqlx`
  dependency.
- Every caller updated: the `hive` binary's four subcommands, and four test files
  (`http_integration.rs`, `migrator_integration.rs`, `capability_integration.rs`,
  `entity_coverage.rs`) that each open their own connection to migrate a test database before
  running their own `sqlx`-based assertions.
- `AppState.db` and the dynamic engine's connection now flow from the migrator's own
  `DatabaseConnection`, not a separately-constructed one.

## What this step found

**A second connection-pool-exhaustion regression, same root cause as `GSR-PHASE-0`'s, different
trigger.** The first version of every test-side migration call opened a fresh, *eager*
`sea_orm::Database::connect(url)` with no connection-count limit. `http_integration.rs` alone runs
65 tests concurrently, each opening one of these in addition to its existing `sqlx` pool and its
existing lazy dynamic-engine connection; 61 of 65 failed with `ConnectionAcquire(Timeout)`. Fixed by
setting `max_connections(1)` on every one of these single-use, short-lived connections (a migration
followed by dropping the handle, or — for `entity_coverage.rs` — a migration followed by a purely
sequential test that never needs more than one connection at a time). The production
`ConnectionFactory::connect_dynamic` gets the same treatment (`max_connections(5)`) for the same
reason, though production runs one process, not sixty-five, so this is precautionary rather than a
fix for an observed failure there.

**Correction to `GSR-CONNECTION`'s own text**: it had assumed `pool()` would be removed at this
phase. It is not — 73 of the 76 tables' repositories are still built on `sqlx` and are not rewritten
until `GSR-PHASE-P8`. What actually moves onto `DatabaseConnection` now is exactly the migrator, the
dynamic schema, and `AppState.db`; `pool()` and `dynamic()` coexist on `ConnectionFactory` until the
persistence phases finish. The design document is corrected in place.

## Gate results

All pass on this commit: `check:rust`, `check:rust:database` (all four test files, including the
migrator's own dedicated regression test — a fresh migration, an idempotent replay, and the exact
30-row ledger count both times), `check:architecture`, `check:schema:entity-coverage`,
`check:schema:entity-relations`, `check:console`, `check:schema:contract`, `check:standalone`,
`check:dsql-conformance`, and the full `npm run validate:local` (46 checks) on the committed tree.
Files in this directory are the raw output from the smaller checks; `validate:local`'s own result is
recorded by its own candidate/tree hashes at the end of its log, generated after this commit lands.
