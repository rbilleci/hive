# GSR-PHASE-P5: module 6 (organization/project/agent read directories) ported onto sea_orm

Status: module 6 done, completing `GSR-PHASE-P5`. Follows `evidence/2026-09-20-seaography-phase5-console/`
(module 5).

## What this step did

All six files ported onto `sea_orm::ConnectionTrait` via `Statement::from_sql_and_values` +
`query_one_raw`/`query_all_raw` (same idiom as `capability`/`console`, `GSR-PHASE-P5`), every SQL
string preserved verbatim:

- `organization/overview.rs` — single-row tenant-scoped lookup, no dynamic parameters.
- `organization/accessible_organization.rs` — keyset-paginated list + count, two query variants
  (cursor present/absent) matching the original's `match &cursor { None => ..., Some(cursor) => ...
  }` branching exactly; `count(*)` gained a `total_count` alias for name-based decoding (the
  original decoded it positionally via `sqlx::query_as`'s single-column tuple).
- `organization/project_directory.rs` — forward/backward keyset pagination with optional
  lifecycle/search/cursor predicates appended to the `WHERE` clause and bind list at runtime
  (dynamic parameter count and position, not fixed arity); the bind list is now built as a
  `Vec<sea_orm::Value>` in the exact same conditional order the original's chained `.bind()` calls
  ran in, rather than a fixed-shape statement.
- `project/dashboard.rs` — single-row tenant-scoped view read.
- `agent/project_agent_directory.rs` — the largest of the six: the same dynamic keyset-pagination
  shape as `project_directory.rs`, plus a batched "latest published version" lookup
  (`latest_versions`) using `= ANY($1)` over a `Vec<Uuid>`, needing the same hand-built
  `Value::Array(ArrayType::Uuid, ...)` construction `capability::locks::
  lock_deployment_approval_authority_page` already established (`evidence/2026-09-20-seaography-
  phase5-capability/`) — sea-query has no blanket `Vec<T> -> Value` conversion for arbitrary
  bindable types. Also decodes a `JSONB` column (`canonical_document`) via `serde_json::Value`,
  confirming `with-json` decode works through the raw-statement path the same as it does through
  entities.
- `agent/operational_view.rs` — single-row tenant-scoped projection read.

Every one of these six repositories' constructors dropped their `PgPool` field entirely (unlike
`capability`'s five rippled-into callers, which still need `PgPool` for their own not-yet-ported
queries) — these six had no other sqlx usage left once their own queries moved, so each went
straight from `pool: PgPool` to `db: DatabaseConnection` with no interim dual-field state. All
`hive-api/src/schema/{organization,project,agent}.rs` construction sites updated to match, and the
now-unused `use sqlx::PgPool;` import removed from `organization.rs` and `project.rs` (`agent.rs`
keeps it: `PgAgentDraftRepository`, not yet ported, still needs both `db` and `pool`).

## A flake, confirmed and dismissed

The first `check:rust:database` run after this change failed one test:
`project_dashboard_is_null_for_a_project_the_principal_cannot_see` (expected `null`, got a real
dashboard object). Given this test exercises exactly the file just changed
(`project/dashboard.rs`), it was investigated directly rather than assumed away:

1. Diffed the ported file against `git diff HEAD` — the SQL text and bind-value order are
   byte-for-byte identical to the pre-port version; only the execution mechanism (`sqlx` calls vs.
   `Statement`/`query_one_raw`) changed.
2. Re-ran the single failing test in isolation, single-threaded: passed immediately.
3. Re-ran the full `check:rust:database` suite twice more: 64/64 clean both times.

This matches the project's documented pre-existing concurrency-flake signature (`http_integration.
rs`'s own top-of-file comment: tests touching Product's projects can observe another concurrently-
running test's transient state without serializing against `PRODUCT_PROJECTS_LOCK`) — not a
regression in this port. Recorded here per the standing verification discipline (confirm a flake
by ruling out a real cause, never assume).

## Verification

- `cargo build --workspace --exclude hive-console --tests`, `cargo fmt --all`, `cargo clippy
  --workspace --exclude hive-console --all-targets -- -D warnings`: all clean.
- `check:integration:organization`, `organization-overview`, `organization-project-directory`,
  `project-agent-list`, `project-dashboard`, `agent-operational-view`: all pass.
- `check:e2e:organization-selector`, `organization-overview`, `organization-project-list`,
  `project-dashboard`, `project-agent-list`, `console-navigation`: all pass.
- `check:rust:database`: 64/64 `http_integration` (after ruling out the flake above), 13/13
  `capability_integration`, all other suites clean, across three total runs.
- Full `validate:local` against commit `eff1abd` (this phase's final commit), completing
  `GSR-PHASE-P5`:

  ```
  validate:local candidate=eff1abd308fd0bf4149f142924593af40c652ec0 tree=643e7a0f0cb61487319778b43c0d161d3a1504af state=clean checks=49 verified-after-checks
  ```

  All 49 checks passed.
