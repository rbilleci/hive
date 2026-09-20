# GSR-PHASE-P5 (partial): console/mod.rs ported onto sea_orm

Status: module 5 (`console`) done. Follows `evidence/2026-09-20-seaography-phase5-capability/`
(module 4, `capability`).

## What this step did

`console/mod.rs`'s three sqlx query sites (`principal_display_name`, `visible_organizations`,
`find_preferences`/`update_preferences`) rewritten onto `sea_orm::ConnectionTrait`, same idiom as
`capability` and the migrator: `Statement::from_sql_and_values` + `query_one_raw`/`query_all_raw`/
`execute_raw`, with every hand-written SQL string preserved verbatim. `update_preferences`'s
transaction (lock the principal row `FOR UPDATE`, then upsert `principal_display_preferences`) now
opens via `sea_orm::TransactionTrait::begin()` instead of `sqlx::PgPool::begin()` — a real, safe
transaction port (not deferred like `capability::tx.rs`), since this transaction's lock and its own
write both run on the same `DatabaseTransaction`, with no external caller sharing the connection the
way `capability::tx.rs`'s locked checks do.

Confirmed all six of this module's tables (`organizations`, `organization_memberships`, `projects`,
`platform_role_assignments`, `principals`, `principal_display_preferences`) have generated entities
with an *empty* `Relation` enum (`sea-orm-codegen` generated them without foreign-key relations
configured) — a builder-based `Select::join` would need the same manual join wiring a raw statement
does, with none of the byte-for-byte parity a preserved statement gives for free, so the raw-
statement idiom was the right choice here too, not just a shortcut.

`PgConsoleRepository` no longer holds a `PgPool` field at all (it was added in the prior commit only
to bridge `capability`'s new signature) — now just `db: DatabaseConnection`, its constructor back
to one argument. All three construction sites in `hive-api/src/schema/console.rs` updated to match;
the now-unused `use sqlx::PgPool;` import removed from that file too.

## Verification

- `cargo build --workspace --exclude hive-console --tests`, `cargo fmt --all`, `cargo clippy
  --workspace --exclude hive-console --all-targets -- -D warnings`: all clean.
- `check:integration:console`, `check:e2e:console-context`: both pass.
- `check:rust:database`: 64/64 `http_integration`, 13/13 `capability_integration`, all other suites
  clean — confirming `console`'s port introduced no regression in either its own behavior or
  anything downstream that reaches it (deployment capability fan-out, evaluation capability
  fan-out, the display-preferences read/write cycle).
