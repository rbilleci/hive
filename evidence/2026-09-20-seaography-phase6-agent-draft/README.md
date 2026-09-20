# GSR-PHASE-P6: module 8 (agent::draft) ported onto sea_orm — Phase P6 complete

Status: module 8 (`agent::draft`) done, the last module of Phase P6. Follows
`evidence/2026-09-20-seaography-phase6-configuration/` (module 9) and
`evidence/2026-09-20-seaography-phase6-administration/` (module 7). With this module landed,
Phase P6 (persistence authoring) is fully complete.

## Third and final confirmation: `capability::tx.rs`'s locked twins are redundant once a module's writes move onto `sea_orm`

`agent::draft`'s own `tx_agent_draft_create_or_publish` (the shared `AGENT_DRAFT.CREATE`/
`AGENT_DRAFT.PUBLISH` check) and the several `tx::project_visible`/`tx::legacy_or_developer`/
`tx::active_project` call sites in `command()`/`create_draft`/`publish_draft` collapsed into direct
calls to `capability::queries::{project_visible, legacy_or_developer, active_project}` with
`lock: true` over the module's own `sea_orm::DatabaseTransaction` — the same finding
`configuration` and `administration` each made in their own ports, now confirmed a third time on
the last of the three Phase P6 modules.

With this port landed, `capability::tx::{project_visible, legacy_or_developer,
active_organization_for_project}` lost their last remaining external callers. Confirmed dead via
`cargo build`'s own `dead_code` warnings (all three fired immediately after the rewrite compiled),
then `grep -rn "tx::" crates/hive-persistence/src | grep -v capability/tx.rs` to verify zero
remaining callers anywhere in the workspace before deleting. `active_organization_for_project` was
only ever called by `tx::project_visible` itself (both local to `tx.rs`), so it went dead in the
same pass.

`capability::tx::active_project` is explicitly **not** deleted: `tx.rs`'s own
`has_deployment_capability` (serving the not-yet-ported `deployment`/`evaluation` modules, Phase
P7) still calls it internally. `tx.rs` now shrinks to exactly the functions those two remaining
modules need — `active_project`, `has_deployment_capability`, `has_evaluation_capability`,
`has_platform_admin`, `has_active_project_role`/`_read`, `deployment_approval_capabilities`,
`has_platform_admin_read`, and their supporting private helpers — to be deleted in full once Phase
P7 lands.

## What else this step did

- Every sqlx-based helper (`visible_target`, `ensure_draft`, `latest_version_number_or_null`,
  `load_draft`, `update_document`, `catalog_reference`, `resource_reference`,
  `dependencies_resolved`, `diagnostics`, `validate_document`, `catalog_release`,
  `next_version_number`, `latest_version_document`, `version_row`, `version_for_digest`,
  `organization_of`, `legacy_audit`, `authoring_audit`, `project_agent_version_target`) moved from
  `conn: &mut PgConnection` to `db: &impl ConnectionTrait`, using the same raw-SQL-via-`Statement`
  idiom as every prior module — every SQL string preserved verbatim, including the
  `evaluation_target_projections` upsert `publishDraft` performs in place of the
  DSQL-incompatible trigger the original schema declared.
- `organization_of`'s missing-row case moved from `.ok_or(sqlx::Error::RowNotFound)` to
  `DbErr::RecordNotFound(format!("no project with id {project}"))` — the `sea_orm`-native
  equivalent, confirmed to exist by reading `sea-orm`'s own `error.rs`.
- `legacy_audit`/`authoring_audit` moved from `crate::audit::bind_audit_metadata` (which chains
  onto a live `sqlx::query::Query`) to `crate::audit::context::audit_metadata_values()` (returns a
  `Vec<sea_orm::Value>` appended to the hand-built bind list), the same helper `configuration`
  and `administration` already established.
- `command()` (the shared body behind `updateDraft`/`validateDraft`) and its SQLSTATE-40001-at-
  commit retry loop converted to `crate::sql::is_serialization_failure_db(&DbErr)` and
  `self.db.begin()`/`DatabaseTransaction`, matching `configuration`'s own retry pattern exactly.
- `PgAgentDraftRepository` drops its `pool: PgPool` field entirely — `agent::draft` no longer
  touches `sqlx` at all. The one construction site in `hive-api/src/schema/agent.rs` updated to
  pass only the `sea_orm::DatabaseConnection`; the now-unused `use sqlx::PgPool;` import removed
  from that file (nothing else in it still needed it).

## Verification

- `cargo build --workspace --exclude hive-console --tests`: clean (after deleting the three
  functions confirmed dead by the compiler's own warnings, not assumed).
- `cargo fmt --all`, `cargo clippy --workspace --exclude hive-console --all-targets -- -D
  warnings`: clean.
- `check:integration:agent-draft-editor`, `check:integration:agent-authoring`,
  `check:integration:agent-operational-view`: all pass.
- `check:e2e:agent-draft-editor`, `check:e2e:agent-authoring`, `check:e2e:agent-operational-view`:
  all pass.
- `check:rust:database`: 64/64 `http_integration`, 13/13 `capability_integration`, all other
  suites clean.
- `check:architecture`: 22 hive-api source files, 0 findings, unchanged.

Phase P6 (persistence authoring — modules 7, 8, 9) is now fully complete. Next: Phase P7
(`deployment`/`evaluation`).

## Full `validate:local` confirmation

Full `validate:local` against commit `55532e2` (this phase's final commit), completing
`GSR-PHASE-P6`:

```
validate:local candidate=55532e2bf6ec03b5dfd45f00146d7e0377242543 tree=267c0a5e9d9d0886565ebe73ac89015b5e91aecf state=clean checks=49 verified-after-checks
```

All 49 checks passed.
