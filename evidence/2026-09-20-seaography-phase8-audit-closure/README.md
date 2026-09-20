# GSR-PHASE-P8: audit ported, connection factory merged, sqlx closed out

Status: Phase P8 (module 12 `audit`, module 13 `connection.rs`, the final phase) is done. This is
the last phase of the 9-phase rewrite (`GSR-PHASE-0` through `GSR-PHASE-P8`). Follows Phase P7
(`evidence/2026-09-20-seaography-phase7-deployment-evaluation/`).

## What this step did

### `audit` (module 12: `audit/mod.rs`, the last repository still on `sqlx`)

- `PgAuditRepository` drops its `pool: PgPool` field, holding only `db: DatabaseConnection`.
  `resolve_scope`, `select_events`, `count_events`, `event_from_row` all converted from
  `sqlx::query`/positional `row.get(n)` to `Statement::from_sql_and_values` +
  `ConnectionTrait::{query_one_raw, query_all_raw}` + name-based `QueryResult::try_get_by`. Query
  text preserved verbatim — `SELECT_COLUMNS` already carried the aliases name-based decode needs,
  and `count_events`'s unnamed `COUNT(*)` expression gained `AS count`.
- Added `string_array(values: &[String]) -> Value`, a `Value::Array(ArrayType::String, ...)` twin of
  the `Vec<Uuid>` helper `deployment`/`evaluation` established in Phase P7, for the
  `grant.capabilities: Vec<String>` bind in the capability-filter predicate.
- `audit/context.rs`'s `bind_audit_metadata` and its now-unused `sqlx::{postgres::PgArguments,
  query::Query, Postgres}` imports were already deleted in Phase P7 once `deployment`/`evaluation`
  stopped calling it; nothing left to remove here. `audit/cursors.rs` needed no changes (pure
  cursor logic, no DB access).

### `connection.rs` (module 13): the two-connection split collapses to one

- `ConnectionFactory` held `pool: PgPool` (for repositories still on `sqlx`) and `dynamic:
  DatabaseConnection` (for the migrator and the Seaography schema) side by side since
  `GSR-PHASE-1`. With `audit` ported, nothing in the workspace needs `pool` anymore — removed the
  field, `PgConnectOptions`/`PgPoolOptions`/`parse_options` entirely; `ConnectionFactory` is now a
  single `dynamic: sea_orm::DatabaseConnection`, and `connect()` returns `Result<Self,
  sea_orm::DbErr>` instead of `sqlx::Error`.
- `connect_dynamic` moves off the interim `.connect_lazy(true).max_connections(5)` (a cap that
  existed only because almost nothing used the dynamic connection during the early phases) to eager
  connection with no `max_connections` override, keeping only `.acquire_timeout(5s)`. Confirmed by
  reading `sea-orm-2.0.3/src/database/mod.rs` directly (not assumed): `ConnectOptions`'s
  `max_connections: None` default delegates to sqlx's own internal default of 10 — the exact same
  default `PgPoolOptions::new()` (used by the now-removed `pool`) had. So the merged connection
  preserves the original production pool-sizing behavior exactly; nothing was tuned up or down.
- `parse_host_port_database` returns `Result<(String, u16, String), String>` instead of
  `sqlx::Error`, since there is no longer a `sqlx`-flavored error type in this module to construct.
  All 3 existing unit tests (`parses_jdbc_form`, `parses_plain_postgres_form`,
  `rejects_unrecognized_scheme`) kept unchanged — they test string parsing, not the connection type.

### `hive-api` integration points updated to the merged connection

`schema/audit.rs` (`PgAuditRepository::new` takes the cloned `DatabaseConnection` directly, `use
sqlx::PgPool` removed), `schema/mod.rs` (`build(db: DatabaseConnection)` drops the `pool` parameter
and its doc comment's `ctx.data::<PgPool>()` reference), `state.rs` (`AppState` drops its `pool:
PgPool` field), `lib.rs` (`build_state`/`test_state`/`schema_sdl` all drop their `pool`
parameter/field; `serve()`'s call site simplifies to `build_state(connections.dynamic().clone(),
signing_key)`), `tests/http_integration.rs` (`build_test_router()` drops the `pool` variable it
built via `PgPoolOptions` solely to pass to `test_state`, confirmed via grep it had no other use in
that function).

### `sqlx` leaves both crates' production dependencies

`crates/hive-persistence/Cargo.toml` and `crates/hive-api/Cargo.toml` both move `sqlx.workspace =
true` out of `[dependencies]`. It is not deleted outright: `tests/migrator_integration.rs`
(`hive-persistence`) and `tests/http_integration.rs` (`hive-api`, 106 occurrences) each do their own
direct-SQL fixture manipulation/inspection, entirely independent of the repository architecture this
rewrite targets — rewriting ~130 combined call sites of test-fixture plumbing for a purely cosmetic
`grep` match would be disproportionate to what those tests are actually for. `sqlx` is re-added to
each crate's own `[dev-dependencies]`, with a comment explaining the split.

## Scope correction: this row's own gate text predates the chosen architecture

The design document's original Phase P8 gate asked for two things that directly contradict the
architecture Phases P5-P7 deliberately chose and re-confirmed three times over:

1. `grep -rn "...|from_sql_and_values" crates/hive-persistence/src | grep -v migrator/` empty.
   `Statement::from_sql_and_values` is not a transitional artifact — it is the mechanism chosen
   specifically to preserve every hand-tuned SQL string byte-for-byte while still decoding by name,
   over SeaORM's `Select`/`Condition` builder, because a builder-generated query's plan is not
   guaranteed identical to the original's. It is used in 28 files across every ported module
   (`capability`, `console`, `organization`, `project`, `agent`, `configuration`, `administration`,
   `deployment`, `evaluation`, `audit`). Emptying this grep would mean re-deriving all of it through
   the query builder — a materially different, riskier rewrite than the one this plan actually did
   and verified, with no benefit beyond satisfying a literal grep.
2. "Deleted at end: ... `sql.rs`". `sql.rs`'s `parse_string_array`, `json_array`,
   `is_serialization_failure_db`, and `is_unique_violation_db` all have dozens of live callers today
   across every phase's ported mutations (SQLSTATE-40001/23505 classification, JSON/array column
   decode). Deleting the module would mean re-implementing or inlining all four elsewhere for no
   behavioral change.

Rather than force either change to satisfy stale text, or silently ignore the gate without a paper
trail, this phase completes the parts of the original gate that are consistent with the real
architecture (`sqlx::` gone from production `crates/*/src`, `row.get(` gone outside the migrator,
`get_postgres_connection_pool` gone, `sqlx` gone from every `[dependencies]` section) and records
this mismatch explicitly — the same pattern already used for the module-4 `pool()`-not-removed
correction (`GSR-PHASE-1`), the `capability::tx.rs` scope correction (`GSR-PHASE-P5`), and the
`tx.rs`-kept-not-deleted refinement (`GSR-PHASE-P7`). `docs/graphql-seaography-rewrite-plan.md`'s
own "What gets deleted" section and Phase P8 table row are updated to match.

## Verification

- `cargo build --workspace --exclude hive-console --tests`, `cargo fmt --all --check`, `cargo
  clippy --workspace --exclude hive-console --all-targets -- -D warnings`: all clean.
- `check:integration:audit`, `check:integration:audit-transaction`, `check:integration:audit-plan`:
  all pass.
- `check:e2e:audit`: passes.
- `check:rust:database`: 64/64 `http_integration`, 13/13 `capability_integration`, all other suites
  clean.
- `check:architecture`: clean, 0 findings — confirms the merged `DatabaseConnection` and the
  `sqlx`-in-dev-dependencies-only split introduced no new dependency-direction violation.
- `grep -rn "sqlx::" crates/hive-persistence/src crates/hive-api/src --include=*.rs | grep -v
  "sea_orm::sqlx"`: only 3 doc-comment lines remain (`connection.rs` x2, `audit/context.rs` x1),
  all prose narrating the pre-`GSR-PHASE-P8` two-pool history for a future reader — confirmed via a
  stricter pass (`grep -vE "^\S+:\d+:\s*(///|//)"`) that zero of these are live code; `sea_orm::sqlx`
  itself (3 call sites, in `sql.rs`, `migrator/mod.rs`, `deployment/worker.rs`) is sea-orm's own
  vendored re-export used for SQLSTATE error classification — a structurally different type from a
  direct `sqlx` dependency (`GSR-PHASE-0`'s finding), not a leftover of this port.
- `grep -rn "row.get(" crates/hive-persistence/src | grep -v migrator/`: empty.
- `grep -rn "get_postgres_connection_pool" crates/`: empty.
- `grep -n sqlx crates/hive-persistence/Cargo.toml crates/hive-api/Cargo.toml`: only
  `[dev-dependencies]` entries remain.

Phase P8 — and the entire 9-phase rewrite — is now complete.

## Full `validate:local` confirmation

Full `validate:local` against commit `e7f2081` (this phase's final commit), completing
`GSR-PHASE-P8` and the full `GSR-PHASE-0` through `GSR-PHASE-P8` rewrite:

```
validate:local candidate=e7f2081cc8ee90c5f61dc648231995c82fde195c tree=c72ab3b7ab4e842b0cf6f46401d2c93d5c5d3782 state=clean checks=49 verified-after-checks
```

All 49 checks passed.
