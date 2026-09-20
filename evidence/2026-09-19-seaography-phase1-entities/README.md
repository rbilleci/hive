# GSR-PHASE-1, partial: entity generation and gate infrastructure

Status: in progress. Of `GSR-PHASE-1`'s three module-list items, item 3 (`entity/`) is done;
items 1 (`connection.rs` → `DatabaseConnection`) and 2 (the migrator rewrite) are not started. This
commit lands item 3 plus the phase's other deliverables (`check:architecture`, the two
entity-coverage gate scripts, `AppState.db`) because they stand on their own and are fully
verified; the migrator rewrite and connection unification continue in a later commit.

## What this step built

- `crates/hive-persistence/src/entity/`: one SeaORM entity module per migration-created relation —
  76 tables (generated with `sea-orm-cli generate entity --seaography` against a database `hive
  migrate` had just migrated) plus 4 views (hand-written; `sea-orm-cli` does not discover views)
  plus the phase 0 `organization_read` module, 81 files total. Every relation is intentionally
  left empty (`GSR-ENTITY-ALL`'s scoping decision, recorded in the design document): annotating one
  is deferred to whichever later-phase repository rewrite is the first to actually need it, rather
  than guessing at up to 76 tables' worth of relations with no consumer yet to test them against.
- `crates/hive-persistence/tests/entity_coverage.rs`: for every full-table module, asserts its
  declared `Column`s match `information_schema.columns` in both directions, that
  `Entity::find().limit(1)` decodes a real row, and that any declared `Relation`'s `from_col`/
  `to_col` name real columns on real tables (inert today — zero relations exist — but exercised
  from the moment phase 2 or later adds one). `organization_read` is excluded: it is a narrower
  read-tier projection over `organizations`, not a 1:1 mapping, so the bidirectional check is the
  wrong one for it.
- `scripts/entity-coverage-tests.mjs`, wired to both `check:schema:entity-coverage` and
  `check:schema:entity-relations` (the same test checks both in one pass; see the test file's own
  doc comment for why splitting them would just duplicate database round trips today).
- `scripts/architecture.mjs` (`check:architecture`): enforces which crate may depend on `sqlx`/
  `sea-orm`/`seaography`/`async-graphql`/`axum`, and that `hive-api` never constructs a sea-query
  statement or raw SQL directly. Caught a real violation on its first run (see below).
- `AppState.db`: the dynamic engine's `DatabaseConnection`, held on the state directly (not only
  inside `schema`) so a future non-GraphQL consumer can reach it.
- All three new `validate:local` checks (`check:architecture`, `check:schema:entity-coverage`,
  `check:schema:entity-relations`) added to `scripts/validate-local.mjs`'s list, positioned per the
  design document.

## What this step found

1. **`evaluation_metric_results.value`/`.threshold` are genuine `NUMERIC` columns**, which
   `sea-orm-cli`-generated code represents as `rust_decimal::Decimal` — a type this workspace's
   `sea-orm` dependency had not enabled a feature for. Fixed by adding `with-rust_decimal` to
   `GSR-CRATE-DEPS`'s `sea-orm` feature list.
2. **The design document's baseline undercounted views: this schema has four, not the one
   (`audit_event_projection`) `GSR-ENTITY-ALL` assumed** — `project_dashboard_projection`,
   `agent_operational_view_projection`, and `effective_evaluation_capabilities` also exist and are
   corrected into the design document.
3. **`check:architecture`'s first run found a real violation**: `graphql_schema/tenant_hooks.rs`
   built a `sea_query::Expr::cust("FALSE")` directly for its "no authenticated principal" fallback,
   which the architecture rule (`hive-api` never constructs a sea-query statement) correctly
   flagged. Fixed by moving that trivial condition into `hive_persistence::entity::tenant::deny_all()`
   alongside the module's other sea-query code, restoring the intended boundary.

## Gate results

All pass on this commit: `check:rust`, `check:architecture`, `check:console`,
`check:schema:contract`, `check:standalone`, `check:dsql-conformance`, and
`check:rust:database`/`check:schema:entity-coverage` (both invoke the same underlying test suite;
one run transiently hit the pre-existing migration-lock-contention flakiness this harness already
has under concurrent load — noted in `hive-rust-standalone-state` project memory — and passed
cleanly on retry, included here as `p1-check-rust-database-2.log`). Files in this directory are the
raw output. A full `npm run validate:local` is deferred until the migrator rewrite and connection
unification land, since running it twice for one phase is wasted time and this step changes no
console-facing or GraphQL-facing behavior.
