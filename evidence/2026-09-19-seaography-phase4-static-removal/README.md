# GSR-PHASE-4: static tier removed, dynamic tier becomes `schema/`

Status: done. Follows `evidence/2026-09-19-seaography-phase3-dynamic-default/` (Phase 3 completion,
including the explicit-null-argument fix).

## What this step did

- Relocated the three engine-agnostic request-context types (`RequestPrincipal`,
  `RequestCorrelationId`, `DependencyUnavailable`) out of the static `schema/mod.rs` before deleting
  it, into what was `graphql_schema/mod.rs` (now `schema/mod.rs`) — every dynamic-tier resolver file
  already depended on them (`ctx.data::<RequestPrincipal>()` etc.), and `graphql.rs` (the `/graphql`
  HTTP handler) needed them to keep resolving from the same `crate::schema::{...}` import path with
  no changes of its own.
- Eliminated `deployment.rs`'s temporary `wire_principal()`/`WirePrincipal` bridge: `approval_principal`
  now constructs `principal::Principal` (the dynamic tier's own wire type) directly instead of
  building the static tier's `Principal` first and reshaping it.
- Deleted `crates/hive-api/src/schema/` (the 13 static files, 6,521 lines) and `engine.rs` in one
  commit, then renamed `graphql_schema/` to `schema/` — 22 files' `crate::graphql_schema::` imports
  mechanically updated to `crate::schema::`.
- Removed the `Engine`/`HiveSchema` wrapper enum, `HIVE_GRAPHQL_ENGINE`, and `hive schema-sdl
  --engine`: `hive_api::schema_sdl()` and `hive_api::test_state()` now take no engine parameter;
  `lib.rs`'s `state_with_engine`/`state_from_env` collapsed into one `build_state()`;
  `test_state_with_engine` deleted entirely. `AppState.schema` is now `async_graphql::dynamic::Schema`
  directly, not a wrapper enum.
- `strip_dangling_subscription_root` (the Seaography SDL workaround) and its two unit tests moved
  from `engine.rs` into `schema/mod.rs` as a `pub fn sdl(schema: &Schema) -> String` helper, since
  there is now only one schema-building module to own it.
- `http_integration.rs`: `build_test_router_with_engine`/`test_state_with_engine` collapsed into the
  no-argument `build_test_router()`. `current_principal_resolves_through_whichever_engine_is_selected`
  deleted outright — with only one engine left, its body was byte-for-byte identical to
  `graphql_accepts_a_cookie_minted_by_local_dev_login`, a genuine duplicate rather than a test to
  keep. `organization_read_is_scoped_to_the_requesting_principals_memberships` simplified to call
  `build_test_router()` with no engine argument; its doc comment's "runs only against the dynamic
  engine" framing dropped since dynamic is now the only engine.
- Confirmed `async-graphql`'s default features were **already** disabled workspace-wide
  (`Cargo.toml`: `default-features = false, features = ["dynamic-schema"]`) from `GSR-PHASE-0` — no
  change needed there.

## Verification

- `cargo build -p hive-api --lib`, `cargo build -p hive`, `cargo test --workspace --exclude
  hive-console --no-run`: all compile clean after each mechanical step (types moved, tier deleted,
  rename, engine removed), not just at the end.
- `cargo fmt --all` + `cargo clippy --workspace --exclude hive-console --all-targets -- -D warnings`:
  clean.
- `cargo test -p hive-api --lib`: 26 tests pass (28 from the end of phase 3, minus the deleted
  static `Long` scalar's own 2 tests — no dynamic-tier test was lost, confirmed by name-for-name
  comparison against the phase-3 test list).
- `npm run check:rust`: clean (fmt + clippy + full workspace test run).
- `npm run check:architecture`: 22 hive-api source files (down from 36), 0 findings.
- `npm run check:schema:contract`: 220 console-reachable types, 0 differences — unchanged, since
  this phase only moves Rust module paths and never touches schema composition logic.
- `npm run check:rust:database`: 64/65 `http_integration` tests pass with 0 failures (65 minus the
  one deleted duplicate test, matching exactly).
- The design doc's own stated phase-4 gates, run directly:
  - `grep -rn "async_graphql::\(Object\|SimpleObject\|Enum\|InputObject\|Interface\|MergedObject\|ComplexObject\|Scalar\)\b" crates/hive-api/src` — empty.
  - `grep -rn "EmptySubscription" crates/` — empty.
- `hive schema-sdl` (the simplified, no-argument CLI subcommand) runs standalone and prints the full
  1928-line SDL with no reachable database.
- Full `validate:local`: see the final log line below.

## Gate results

Full `validate:local` against commit `5ed1167` (this phase's implementation commit):

```
validate:local candidate=5ed116727c2c36f86850578686e1409a2ae86683 tree=7932732f16b14eb1c35dc27766d1ec537627bf24 state=clean checks=49 verified-after-checks
```

All 49 checks passed, confirming this phase's own stated gate.
