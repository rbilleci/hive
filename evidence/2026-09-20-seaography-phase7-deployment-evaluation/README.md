# GSR-PHASE-P7: deployment and evaluation ported onto sea_orm

Status: both modules of Phase P7 (module 10 `deployment`, module 11 `evaluation` — the largest
remaining area, ~11,200 lines across the two) are done. Follows Phase P6
(`evidence/2026-09-20-seaography-phase6-agent-draft/`).

## `capability::tx.rs` is not deleted — it is ported, and shrinks to what Phase P7 still needs

Phase P5's own note assumed `capability::tx.rs`'s remaining consumers (`agent::draft`,
`configuration`, `administration` in P6; `deployment`, `evaluation` here) would each let their own
`tx::*` call sites go dead by switching to `capability::queries::*` directly, the same way
`configuration`/`administration`/`agent::draft` did in Phase P6. That held for three of `tx.rs`'s
original functions (`has_platform_admin`, `has_active_organization_role`, `has_active_project_role`,
`project_organization`, `active_project`, and their `_read` twins) — all deleted here, since
`capability::queries.rs` already exposes the identical primitive with a runtime `lock: bool`
parameter, eliminating the need for a hardcoded-locked/hardcoded-unlocked pair.

But `tx.rs`'s other five items — `deployment_capabilities`, `deployment_approval_capabilities`,
`has_deployment_capability`, `evaluation_capabilities`, `has_evaluation_capability`, plus the
`DEPLOYMENT_*`/`EVALUATION_*` capability-string constants — are not duplicates of anything in
`capability::queries.rs`. They are the deployment/evaluation-specific capability **composition**
logic (which roles grant which of the 8 deployment + 7 evaluation capability strings), unique to
these two domains and needed by no other module. These were **ported in place** (same file, same
public names, `conn: &mut PgConnection` → `db: &impl ConnectionTrait`, `sqlx::Error` → `DbErr`),
now composed entirely from `capability::queries::{has_platform_admin, has_active_organization_role,
has_active_project_role, project_organization, active_project}` instead of hand-duplicating the
locked/unlocked branching. `capability::locks::lock_project_role_authority` is called directly for
`evaluation_capabilities`'s upfront lock, matching `capability::queries::has_active_project_role`'s
own internal use of the same lock helper.

`tx.rs` shrank from 613 lines (with `sqlx`) to 305 lines (pure `ConnectionTrait` composition, zero
`sqlx` references) and is not going away — it is now `capability::tx.rs`'s final, permanent
"deployment/evaluation capability grants" module, matching `capability::queries.rs`'s and
`capability::locks.rs`'s already-generic status. The original design doc's Phase P7 "Deleted at
end" column listing `deployment/*`/`evaluation/*` sqlx forms (not `capability::tx.rs`) turns out to
be exactly right — `tx.rs` was never scheduled for deletion, just for the same generic-executor
conversion every other module needed.

## What else this step did

### `deployment` (module 10, 7 files + 2 top-level, ~7,100 lines)

- `writes.rs`, `rows.rs`, `queries.rs`, `mutations.rs`, `approval.rs`, `worker.rs`, `mod.rs`: every
  helper moved from `conn: &mut PgConnection`/`tx: &mut PgConnection` to `db: &impl ConnectionTrait`,
  using the established raw-SQL-via-`Statement` idiom; every SQL string preserved verbatim except
  for column aliases added purely to support name-based decode (see below). `cursors.rs` needed no
  changes (pure string/cursor logic, already engine-agnostic) beyond gaining a `sql_value`-style
  conversion at its one call site in `queries.rs`/`mutations.rs` (kept local to the consumer, not
  added to `cursors.rs` itself, to keep that module's own zero-dependency-on-a-DB-crate property).
- The five deployment mutations (`deploy`/`cancel`/`retry`/`promote`/`rollback`, the latter two
  sharing `recovery()`) each open their own `sea_orm::DatabaseTransaction` via `db.begin()`; the
  SQLSTATE-40001-at-commit retry loops (`cancel`, `promote`) and the unique-violation idempotency
  replay (`deploy`) converted to `crate::sql::{is_serialization_failure_db, is_unique_violation_db}`.
- The outbox worker (`worker.rs`)'s `deliver_next` keeps its documented rollback-then-fresh-
  transaction recovery path (a `DatabaseTransaction`'s `Drop` rolls back an uncommitted transaction
  the same way `sqlx::Transaction`'s does, so the existing reasoning in that function's doc comment
  carries over unchanged) and its own SQLSTATE-extraction helper, renamed `db_failure_code` (was
  `sql_failure_code`) since it now takes `&DbErr`.
- `worker_health.rs` (`deployment_worker_health`/`evaluation_worker_health`, shared by both modules
  since RTP-BOOTSTRAP) moved from `&PgPool` to `&DatabaseConnection`.
- `PgDeploymentRepository` drops its `PgPool` field entirely, holding only `db: DatabaseConnection`
  plus its own `next_approval_maintenance_at` rate-gate `AtomicI64`.

### `evaluation` (module 11, 5 files, ~4,000 lines)

- `rows.rs`, `queries.rs`, `mutations.rs`, `worker.rs`, `mod.rs`: same idiom. `cursors.rs` needed no
  changes (pure). The 8 mutations' shared `idempotent_mutation`/`receipt`/`draft_command` machinery
  and the outbox worker's `claim_next`/`commit`/`idle`/`delivered`/`failed`/`claim_failed` cycle all
  now run on `DatabaseTransaction`; `worker.rs`'s `append_evidence` (the cross-domain handoff into
  `deployment::{waiting_for_evaluation, touch_projection, automatic_approval_handoff}`) calls those
  already-`ConnectionTrait`-generic `deployment` functions directly over the same transaction — the
  two domains' interop needed no adapter now that both speak `ConnectionTrait`.
- `PgEvaluationRepository`/`PgEvaluationWorkStore` both drop `PgPool`, holding only `db:
  DatabaseConnection`.

### Column aliases added for name-based decode (`GSR-PERSISTENCE`'s stated goal)

Every join whose column list had no naming collision decodes by each column's own natural name
(unaliased `SELECT id, deployment_id, ...` already gives unambiguous names) — the large majority of
queries in both modules needed zero alias changes. Aliases were added only where two joined tables'
same-named column would otherwise collide, or where an original positional/tuple decode read from a
computed expression:

- `deployment`/`evaluation`'s `version_source`-shaped queries (`version.id`, `project.id`,
  `agent.id` all named `id`): aliased to `version_id`/`project_id`/`agent_id`.
- `evaluation::queries::run_row_full`/`run_rows` (the one query joining `evaluation_runs run` and
  `evaluation_target_snapshots snapshot`, both of which have their own
  `environment_definition_version_id`): the snapshot's copy aliased to
  `snapshot_environment_definition_version_id`.
- `evaluation`'s three `Target`-shaped queries (`resolve_target`'s two branches,
  `target_from_snapshot`): `logical_environment_class` aliased to `environment_class` to match the
  shared `Target` struct's field name — the one alias this port initially missed (see below).
- Several `EXISTS(...)`/`COUNT(...)`/boolean-expression columns gained an explicit alias
  (`AS still_pending`, `AS exists_flag`, `AS disposition`, `AS active`, `AS owned`, `AS remaining`,
  `AS pending`, `AS next_attempt_count`, `AS count`) since the original decoded them positionally
  from an unnamed computed column.

## Two real defects found and fixed during verification (not by inspection — by a genuine test failure each)

Both were caught by re-running the module's own integration checks after the initial full-workspace
build and clippy pass came back completely clean (zero errors, zero warnings on the first attempt
across ~11,200 ported lines) — a reminder that a clean compile proves the Rust is well-typed, not
that the SQL text is correct.

1. **Missing `environment_class` alias** (`evaluation::queries`): `resolve_target`'s two branches and
   `target_from_snapshot` selected `logical_environment_class` unaliased, but the shared
   `rows::target_from_row` mapper decodes column `"environment_class"` (matching the `Target`
   struct's field name, per the original's positional index 3). `check:integration:evaluation` and
   `check:integration:mvp-shared` both failed with `UNAVAILABLE` refusals; the actual cause —
   `Query Error: no column found for name: environment_class` — was only visible by setting
   `HIVE_FIXTURE_LOG`/`RUST_LOG=debug` and re-running, since `refuse_on_storage_failure` otherwise
   swallows the real `DbErr` into a generic refusal problem. Fixed by adding `AS environment_class`
   to all three call sites.
2. **Unbalanced parenthesis in `reconcile_project_archives`'s `still_pending` query**
   (`deployment::approval`): appending `AS still_pending` to the original's own
   `SELECT EXISTS (...)` text left one extra closing paren from a miscount while wrapping the alias
   around the existing expression (10 open, 11 close). This produced a genuine SQL syntax error at
   every 1-second maintenance tick, silently absorbed into `/health`'s upgrade-maintenance failure
   state (`reconcile_approval_upgrade` has no `tracing::warn!` on its own error path, matching the
   original) — invisible in the test's own assertions until `check:integration:approval`'s
   project-archive-replay scenario timed out waiting for a deployment to reach `CANCELED`. Found by
   the same `HIVE_FIXTURE_LOG` technique, confirmed with a Python parenthesis-balance check, then
   fixed by removing the extra `)`. A follow-up scripted paren-balance audit across every SQL string
   literal in all 11 ported files (`writes.rs` through `evaluation/worker.rs`) found no other
   instance of this mistake.

## Architecture: one dependency-direction fix

`check:architecture` flagged `crates/hive/Cargo.toml` for declaring `sea-orm` directly — only
`hive-persistence`/`hive-api` may depend on it. `hive/src/main.rs`'s `run_deployment_worker`/
`run_evaluation_worker` needed to name the connection type in their signatures (to pass
`connections.dynamic().clone()` through to `PgDeploymentRepository::new`/
`PgEvaluationWorkStore::new`); fixed by adding `pub use sea_orm::DatabaseConnection;` to
`hive-persistence`'s own `lib.rs` and having `hive`'s binary reference
`hive_persistence::DatabaseConnection` instead, removing the direct `sea-orm` dependency edge (and
the now-fully-unused `sqlx` one) from `hive/Cargo.toml`.

## Dead code removed

With `deployment`/`evaluation` off `sqlx`, two helpers that existed solely to serve them (and
`agent::draft`, already ported in Phase P6) lost their last callers:
`crate::sql::is_serialization_failure(&sqlx::Error)` and
`crate::audit::context::bind_audit_metadata` (the `sqlx::query::Query`-chaining audit-metadata
binder). Confirmed dead via `grep` across the whole workspace (both are `pub fn`s in a library
crate, so the compiler's own `dead_code` lint does not flag them even when truly unused) before
deleting; the `DbErr`-flavored twins (`is_serialization_failure_db`, `audit_metadata_values`) remain
and are now the only versions of either helper.

## Verification

- `cargo build --workspace --exclude hive-console --tests`: clean, zero warnings, on the first
  attempt after the full module conversion (before the two defects above were found by tests, not
  by the compiler).
- `cargo fmt --all`, `cargo clippy --workspace --exclude hive-console --all-targets -- -D warnings`:
  clean.
- `check:integration:deployment`, `check:integration:evaluation`, `check:integration:approval`,
  `check:integration:deployment-harness`, `check:integration:mvp-shared`: all pass (after the two
  fixes above).
- `check:e2e:deployment`, `check:e2e:approval`, `check:e2e:evaluation`: all pass.
- `check:rust:database`: 64/64 `http_integration`, 13/13 `capability_integration`, all other suites
  clean.
- `check:architecture`: 22 hive-api source files, 0 findings (after the `hive`/`sea-orm` dependency
  fix above).

Phase P7 is now fully complete. Next: Phase P8 (`audit`/`sql.rs` closure — the final phase).

## Full `validate:local` confirmation

Full `validate:local` against commit `d9c6347` (this phase's final commit), completing
`GSR-PHASE-P7`:

```
validate:local candidate=d9c6347be0474b4c852fd1b72ed4aec81d37da8d tree=cf203a2288da3151fc6840bb2157aa2b2bd19bbd state=clean checks=49 verified-after-checks
```

All 49 checks passed.
