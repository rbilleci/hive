# GSR-PHASE-P6 (partial): module 7 (administration) ported onto sea_orm

Status: module 7 (`administration`) done — the largest write-transaction module ported so far (four
files, ~2,645 lines, nine write commands). Follows `evidence/2026-09-20-seaography-phase6-configuration/`
(module 9).

## `tx_has_administration_capability` collapses into `capability::has_capability` directly

`administration::mutations`'s own `tx_has_administration_capability` was a hand-duplicated,
deliberately-narrowed twin of `hasCapability`'s `ADMINISTRATION_CAPABILITIES` fallback branch — the
module's own doc comment already documented the one place it differs from the real thing: it omits
the `ORGANIZATION.VIEW`-via-membership arm, since none of these nine write commands ever check that
specific capability string. Since `capability::has_capability` is already `ConnectionTrait`-generic
(`GSR-PHASE-P5`), and this port's write commands now open a `sea_orm::DatabaseTransaction`,
`tx_has_administration_capability` is now a two-line wrapper that converts an `AdministrationScope`
to `capability::Scope` and calls `capability::has_capability(&txn, ..., lock: true)` directly —
provably equivalent for every capability string these commands actually pass (confirmed by the
docstring's own claim, not just assumed). The four local helper functions that existed only to
support a hand-rolled branch-by-branch reimplementation (`tx_has_active_organization_role`,
`tx_has_active_project_role`, `tx_project_organization`, `tx_scope_exists`) became entirely dead
the moment this landed — confirmed via `cargo build`'s own `dead_code` warnings, then deleted.
`capability::tx::is_platform_administrator` (an unlocked helper `capability::tx.rs` itself had — a
different concern from this file's own locked `tx_is_platform_administrator`, which now calls
`capability::queries::has_platform_admin` directly with `lock: true`) also went dead and was
deleted the same way.

This is the same finding `configuration`'s port made for `capability::tx::configuration_write`,
now confirmed on a second, much larger module: once a module's write transaction moves onto
`sea_orm`, `capability::tx.rs`'s locked twins become redundant with the shared, already-generic
`capability` module — not because they were rewritten, but because they were never needed once the
generic-executor limitation that justified them (`sqlx`'s, not `sea_orm`'s) no longer applies.

## What else this step did

- `administration::rows.rs` — every function moved from `conn: &mut PgConnection` to
  `db: &impl ConnectionTrait`, the same raw-SQL-via-`Statement` idiom as every prior module.
  `is_unique_violation` delegates to `crate::sql::is_unique_violation_db`.
- `administration::queries.rs` — `organization`/`project` (previously split into a `pool`-based
  read path and a `db`-based capability check) collapsed onto one `db: &impl ConnectionTrait`
  parameter, since they now serve both `find_organization`/`find_project` (over a bare
  `&DatabaseConnection`) and `result_for` (over the write commands' own `&DatabaseTransaction`,
  reading the refreshed projection after `txn.commit()`) identically.
- `administration::mutations.rs` — all nine write commands (`createProject`, `addMembership`,
  `replaceMembership`, `endMembership`, `lifecycle`, `updateBudget`, `updateApprovalPolicy`,
  `updateProjectGeneral`, `saveProjectConnection`) and every helper they call (role replacement,
  the deployment/approval-domain scope-cache maintenance four-function family, the project-archive
  cascade including a `RETURNING` clause that gained an explicit column alias for name-based
  decoding, the `set_config` session-variable call, and the audit-event insert) ported onto
  `sea_orm::TransactionTrait::begin`/`DatabaseTransaction`. None of these nine commands use the
  SQLSTATE-40001-retry pattern `configuration` needed: every one locks its row with `FOR UPDATE`/
  `FOR KEY SHARE` and checks the expected revision *before* writing, so there is nothing to retry.
- `PgAdministrationRepository` drops its `PgPool` field entirely — `administration` no longer
  touches `sqlx` at all. The one construction site in `hive-api/src/schema/administration.rs`
  updated to match; the now-unused `use sqlx::PgPool;` import removed.

## Verification

- `cargo build --workspace --exclude hive-console --tests`, `cargo fmt --all`, `cargo clippy
  --workspace --exclude hive-console --all-targets -- -D warnings`: all clean (after deleting the
  five functions confirmed dead by the compiler's own warnings, not assumed).
- `check:integration:administration`, `check:e2e:administration-settings`, `check:e2e:organization-
  project-authoring`: all pass — exercising every one of the nine write commands' conflict/refusal
  branches as well as their success paths.
- `check:rust:database`: 64/64 `http_integration`, 13/13 `capability_integration`, all other suites
  clean.
- `check:architecture`: 22 hive-api source files, 0 findings, unchanged.
