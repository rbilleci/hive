# GSR-PHASE-3: dynamic engine becomes the default

Status: done. Follows `evidence/2026-09-19-seaography-phase2-deployment/` (Phase 2 completion).

## What this step did

- Fixed the `Subscription`-root SDL defect the Phase 2 risk table flagged as a phase-3 blocker:
  `engine.rs::strip_dangling_subscription_root` strips the dangling `schema { ... subscription:
  Subscription }` block Seaography's `Builder::new` hard-codes with no public way to suppress,
  guarded to panic loudly if the expected block ever goes missing (a future Seaography fix, or a
  real subscription being added, both need this workaround revisited, not silently miscompiled).
  Unit-tested directly (`engine::tests`).
- Flipped `HIVE_GRAPHQL_ENGINE`'s default from `static` to `dynamic` in both places it's read:
  `hive-api::engine::Engine::from_env` and `hive`'s `SchemaSdl` CLI subcommand default.
- Regenerated `schema/hive.graphql` from the dynamic engine (`npm run generate:schema`) and added
  matching `.description(...)` calls to the `JSON`/`Long` scalar registrations so `check:schema:
  contract`'s scalar-description comparison (a check this port's own throwaway comparison scripts
  never replicated) passes too.
- Confirmed `check:console`/`check:console:operations` still pass unchanged against the
  dynamic-engine-generated schema (cynic's generated Rust types are structurally identical, as
  expected — the frozen contract is what both engines target).

## What this step found — the single most consequential defect in the whole port

`graphql_schema::build()` never inserted the sqlx `PgPool` into the dynamic schema's global data —
only `.data(db)` (the `sea_orm::DatabaseConnection`, backing the generated `organization_read`
entity tier). Every hand-written resolver across all nine domain modules (organization.rs through
deployment.rs) calls `ctx.data::<PgPool>()` to build its `Pg*Repository` and reach the existing
sqlx-based application/persistence layers. Missing that one line meant every one of those resolvers
would fail at request time with `Data \`sqlx_core::pool::Pool<sqlx_postgres::database::Postgres>\`
does not exist.`

**Why nothing in phase 2 caught this despite ~30 real-execution tests already existing:** every
phase-2 verification fell into one of two categories, and neither ever exercised the *production*
`graphql_schema::build()` function with a *real* resolver call:

1. SDL-shape/interface-declaration comparisons (`assert.deepEqual` against the frozen contract) —
   these inspect the composed schema's *structure*, and structure composition (`Builder::finish()`
   succeeding) has nothing to do with whether `ctx.data::<PgPool>()` will find anything at request
   time.
2. Real-execution unit tests (`console.rs`'s interface tests, `scalars.rs`'s round-trip tests,
   `administration.rs`'s matrix-parsing tests) — these all built their *own* small throwaway
   `Schema::build(...)...finish()` inline in the test, register only what that specific test needs,
   and never called the production `graphql_schema::build()` at all, so the same wiring gap was
   invisible to every one of them by construction.

The gap surfaced the moment `GSR-PHASE-3` flipped the default engine and the full 65-test
`http_integration.rs` suite (which exercises `build_test_router()` → `test_state()` →
`state_from_env()` → `Engine::from_env()`, previously resolving to `Static` and therefore never
touching this code path) ran against the dynamic engine for the first time: 43 of 65 tests failed.
`check-rust-database-before-fix.log` is the full 43-failure run; a separate throwaway debug test
(added, run once, and reverted — not part of this commit) confirmed the exact panic text on one
representative failure (`organization_by_id_returns_the_overview_for_a_visible_organization`) was
`Data \`sqlx_core::pool::Pool<sqlx_postgres::database::Postgres>\` does not exist.`, the same root
cause underlying all 43 — not 43 independent resolver defects, one wiring gap surfacing 43 ways.

**Fix**: `graphql_schema::build(pool: PgPool, db: DatabaseConnection)` now calls
`.data(db).data(pool)`, mirroring the static engine's own `build_schema(pool)`
(`schema/mod.rs`'s `.data(pool)`) exactly. `engine.rs::HiveSchema::build` threads `pool` through to
the one call site. Confirmed with three consecutive clean `check:rust:database` runs (65/65 passed
each time — `check-rust-database-after-fix-run1.log`, `-run2.log`; a third clean run is referenced
in the design doc but not separately saved) to rule out a flake explaining the first success.

**The lesson, recorded in the design doc's risk table**: a dynamic schema composing successfully
and matching the frozen contract's shape exactly proves *nothing* about whether its resolvers can
actually reach their data dependencies at request time. Only a real end-to-end query execution
through the *production* `build()` path demonstrates that — and no amount of shape-comparison or
isolated-unit-test rigor substitutes for it, because by construction neither approach exercises the
production wiring.

## Second finding: a full `validate:local` run caught a leftover Seaography artifact

Running the full `validate:local` gate against this commit (required for phase 3's own completion,
not just `check:rust:database`) surfaced a second, smaller defect the targeted checks above never
would have: `check:integration:agent-draft-editor` — a pre-existing script, not written by this
port — does an exhaustive introspection comparison of `Mutation`'s complete field set against a
hardcoded list, and failed on an unexpected `_ping` field. `Builder::new` (`seaography-2.0.0-rc.9/
src/builder.rs`) bakes this field onto the initial `Mutation` object at construction — a
Seaography-side liveness-probe artifact, present in every dynamic-engine build since phase 0 but
invisible to every check this port had run: `check:schema:contract`'s reachability walk only
verifies fields the console's own operations select (nothing ever queries `_ping`), and this port's
own whole-schema comparisons always checked specific named fields' shapes, never asserted "no extra
fields exist" on the whole type. Fixed with one line — `builder.mutation = Object::new("Mutation");`
right after `Builder::new`, before any mutation gets registered (every real mutation lands in
`builder.mutations: Vec<Field>` and is only folded onto `builder.mutation` inside
`schema_builder()`, so replacing the base object early drops `_ping` without losing anything real).
Confirmed by re-running `check:integration:agent-draft-editor`, `check:schema:contract`,
`cargo test -p hive-api --lib`, and `check:architecture` clean, then a fresh full `validate:local`.

This is the second time in as many phases that an *exhaustive* comparison (this port's own
whole-schema check for `Principal.id`; a pre-existing exhaustive integration script for `_ping`)
caught something a *curated* comparison against a hand-picked field list could not, by
construction: a curated list only tells you the fields you already knew to check are correct, never
whether something you didn't think to look for crept in.

## Gate results (before the third finding below)

`check:schema:contract`: 220 console-reachable types, 0 differences (first clean pass against the
dynamic engine). `check:console`: cynic compiles and all 5 `hive-console` unit tests pass unchanged.
`check:console:operations`: 74 operations match. `check:rust:database`: 65/65 `http_integration`
tests pass (3 consecutive runs). `check:architecture`: 36 hive-api source files, 0 findings.
`cargo test -p hive-api --lib`: 25 tests pass (2 new: `engine::tests`, proving the SDL-stripping
fix directly). Full `validate:local`: failed once on the `_ping` defect above; passing on the
following clean run against this directory's final commit.

## Third finding: explicit-`null` arguments crashed 24 resolvers across five files

A subsequent full `validate:local` run (needed because the `_ping` fix changed the tree) failed at
`check:integration:approval`:

```
AssertionError [ERR_ASSERTION]: [{"message":"internal: not a string","locations":[{"line":1,"column":115}]}]
```

The failing query (`scripts/approval-access.mjs`'s `DecisionHistory`) passed `{ after: null }` as a
GraphQL variable — an *explicit* null, not an omitted argument — into
`decisions(after: $after, first: 1)`, a nested field this port hand-built in `deployment.rs`.

Root cause, confirmed by reading `async-graphql-7.2.1/src/dynamic/value_accessor.rs` directly:
`ObjectAccessor::get(name)` is `self.0.get(name).map(ValueAccessor)` — it returns
`Some(ValueAccessor(Value::Null))`, not `None`, when the key is present with an explicit null value;
only a truly *absent* key yields `None` (`try_get` is the one that errors on absence).
`ValueAccessor::string()` — like `.i64()`/`.boolean()`/`.object()` — has no null case in its match:
`if let Value::String(value) = self.0 { Ok(value) } else { Err(Error::new("internal: not a
string")) }`. Every hand-built resolver in this port that read an optional argument used
`ctx.args.get(name).map(|value| value.TYPE()...).transpose()?`, which assumes `Some(...)` always
wraps a real value of the right kind — so an explicit `null` (a common client pattern: many GraphQL
client libraries always send `null` for an unset variable rather than omitting the key) crashed the
resolver instead of being treated as absent.

This was not confined to `deployment.rs`'s one field: grepping the same pattern across the whole
phase-2 port found 24 call sites in `organization.rs`, `project.rs`, `audit.rs`, `evaluation.rs`,
and `deployment.rs` — every nullable `after`/`before`/`filter`/`organizationId`/`projectId` argument
this port hand-parsed, all sharing the identical defect. **Why phase 2's own real-execution tests
never caught this**: none of those tests happened to pass an explicit `null` for an optional
argument — they either omitted it (the common case, which was always handled correctly) or supplied
a real value; explicit-null was a client behavior no test in this port simulated until
`check:integration:approval`, a pre-existing script this port did not write, did.

**Fix**: three shared helpers added to `scalars.rs` — `defined(value)` (filters `Value::Null` out
of `Option<ValueAccessor>`, treating it the same as an absent key), and `optional_string`/
`optional_i64`/`optional_boolean` (each applies `defined` before the matching accessor). All 24 call
sites rewritten to use them; the `filter`-argument match arms (which pass a `ValueAccessor` on into
a derived `CustomInputType::parse_value` or a manual `.object()?` call) route through `defined(...)`
directly, since the same explicit-null gap exists one level down in the derived `parse_value`'s own
`input.object()?` call (confirmed by reading `custom_input_type.rs`'s generated code: no null check
there either).

Three new unit tests in `scalars.rs` prove the fix directly against a real schema execution: an
omitted argument, an explicit `null` literal, and an explicit `null` via a GraphQL variable (the
exact shape `approval-access.mjs` sent) all resolve identically; a present value still round-trips.

## Gate results (final)

`cargo test -p hive-api --lib`: 28 tests pass (3 new, proving the null-argument fix). `cargo clippy
-p hive-api --all-targets -- -D warnings`: clean. `check:integration:approval`: passes (exit 0).
`check:architecture`: 36 hive-api source files, 0 findings, unchanged. `check:rust:database`: 65/65
`http_integration` tests pass, 3 other suites clean. `check:schema:contract`: 220 console-reachable
types, 0 differences, unchanged (this fix touches only resolver internals, not the schema shape).
