# GSR-PHASE-0: spike and pins

Status: done, September 19, 2026. Gate: `cargo build --workspace --exclude hive-console` (this
repository excludes `hive-console` from every native `cargo build`/`cargo test` invocation — it
targets `wasm32-unknown-unknown` and is built by Trunk, per the workspace's own
`default-members` comment), `cargo run -p hive -- schema-sdl --engine dynamic`,
`HIVE_GRAPHQL_ENGINE=dynamic cargo test -p hive-api --test http_integration -- --ignored current_principal`,
plus (beyond the plan's stated gate, added after the spike surfaced real defects)
`npm run check:rust:database` for the full 65-test `http_integration` suite against both engines,
`npm run check:rust`, and `npm run check:console`. All pass; files in this directory are the raw
output.

## What this phase built

- `seaography 2.0.0-rc.9`, `sea-orm 2.0.3`, `async-graphql` (`dynamic-schema` feature, resolved to
  `7.0.19`) pinned per `GSR-CRATE-DEPS`, with one correction: `hive-persistence` needs a direct
  `seaography` dependency too, not only `hive-api` — `sea_orm::DeriveRelatedEntity` (the
  `RelatedEntity` enum a registered entity needs) expands code that names `seaography::` directly.
- `crates/hive-persistence/src/entity/`: `organization_read` (the `organizations` table as a
  SeaORM entity, no relations yet) and `tenant::organization_membership_exists`, the row-scoping
  predicate `GSR-TENANT-HOOKS` needs, mirroring `PgAccessibleOrganizationRepository`'s existing
  `EXISTS` clause.
- `crates/hive-api/src/graphql_schema/`: a `seaography::Builder` composition registering
  `organization_read`, `currentPrincipal` (`#[CustomFields]`, the one custom-tier operation this
  phase ports), a throwaway `SpikeProblem` interface with one implementor, and one hand-built
  default-argument field (`spikeAccessibleOrganizationSlugs`).
- `crates/hive-api/src/engine.rs`: `HIVE_GRAPHQL_ENGINE=static|dynamic` (default `static`) selects
  between the existing static schema and the new one behind one `execute`/`sdl` surface
  (`GSR-SWITCH`), and `hive schema-sdl --engine <static|dynamic>` prints either.
- `ConnectionFactory` opens a second, independent `sea_orm::DatabaseConnection` alongside its
  existing `sqlx::PgPool` (see "What the spike found" below for why, and why it is lazy).

## What the spike found (the reason a phase 0 exists)

1. **`sea-orm 2.0.3` vendors `sqlx 0.9` internally**, a structurally different type from this
   crate's own direct `sqlx 0.8` dependency. `SqlxPostgresConnector::from_sqlx_postgres_pool`
   cannot bridge a `sqlx 0.8` `PgPool` to it — confirmed by `cargo check`'s "multiple different
   versions of crate `sqlx_core`" diagnostic. `ConnectionFactory` opens a second, independent
   connection until `GSR-PHASE-1` replaces `pool` with a `DatabaseConnection` entirely and this
   split disappears.
2. **A concurrent test run exhausted the local Postgres connection limit** the first time that
   second connection was eager: every one of ~65 `http_integration` tests opened two real
   connections instead of one, even though only two of those tests touch the dynamic engine at
   all. Fixed by making the spare connection lazy (`ConnectOptions::connect_lazy(true)`, both in
   `ConnectionFactory` and in the test harness's equivalent) — a lazy connection still opens
   correctly on the first real query a test that needs it sends.
3. **`#[CustomFields]` builds only the field, not its return type's object definition.** The first
   version of `currentPrincipal` returned `Principal` without a matching
   `builder.register_custom_output::<Principal>()` call, and `finish()` failed with
   `SchemaError("Type \"Principal\" not found")`. Every custom-tier output type in the real
   migration needs this second registration call; `GSR-DEFAULTS`'s unit test asserting the
   hand-built field set should also assert every custom output type used anywhere has a matching
   `register_custom_output`/`register_complex_custom_output` call, so this can't recur silently at
   the 74-operation scale.
4. **Seaography names a generated type from the entity's SQL `table_name`, not its Rust module
   path.** Before `entity_object.type_name` was overridden, the emitted names were
   `organizations`/`Organizations`/`OrganizationsConnection` (the default `UpperCamelCase` of the
   table name), not `organizationRead`/`OrganizationRead` as `GSR-READ-TIER` specifies. The
   override (`BuilderContext.entity_object.type_name`) fixes this, but it must be applied — the
   plan named this override correctly; this spike is the first time it was actually written and
   verified against the real printed SDL.
5. **`TenantHooks::entity_filter`'s `entity: &str` argument is the *overridden* type name
   ("OrganizationRead"), not the Rust module path ("organization_read").** A first version matched
   on the wrong string and silently applied no filter at all — confirmed by tracing
   `entity_object.type_name::<T>()`'s call site in `query/entity_query_field.rs`, which is exactly
   where `object_name` (passed to every hook) comes from.
6. **The tenant filter's own correlation was wrong, and it failed toward "returns nothing" rather
   than "returns everything."** `organization_membership_exists` originally compared
   `organization_memberships.organization_id` to a bare, unqualified `Expr::col(organization_id_column)`
   inside a subquery — which resolved against `organization_memberships`'s *own* `id` column
   (also named `id`), not the outer row. The `EXISTS` clause was therefore always false, and even a
   real Product member got zero organizations back. Fixed with `ColumnTrait::as_column_ref()`,
   which returns the entity-qualified `(table, column)` pair `Expr::col` needs for a correlated
   reference. Caught only because the added integration test asserted a *known member* gets their
   organization back, not only that a stranger gets none — a filter that fails toward "matches
   nothing" reads exactly like a correct, strict filter until checked against a case that should
   pass. This is now a standing risk-table item for `GSR-PHASE-2`, which repeats this same pattern
   for `project_read` and `agent_read`.

None of these six were found by inspection or by trusting the design document's prose — all six
surfaced only by actually compiling against the real prerelease crate and running a real query
against a real database with two different principals. `docs/graphql-seaography-rewrite-plan.md`
is corrected in place for (1), (4), and (5); (3) and (6) are recorded here as risks the later
phases' own gates must specifically re-check at full scale, since nothing currently makes either
failure mode impossible to reintroduce — only a passing test makes it visible in one instance.

## Printed SDL

`dynamic-schema.graphql` is `hive schema-sdl --engine dynamic`'s exact output on this commit. The
default engine's `schema/hive.graphql` (which the console and `check:schema:contract` actually use)
is untouched by this phase.
