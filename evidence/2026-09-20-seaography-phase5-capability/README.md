# GSR-PHASE-P5 (partial): capability moves onto sea_orm::ConnectionTrait

Status: module 4 (`capability`) done, with one scope correction recorded below. Modules 5-6
(`console`, the organization/project/agent read directories) not started — only the minimal,
compile-forced `db` threading needed to keep their current callers of `capability` building.
Follows `evidence/2026-09-19-seaography-phase4-static-removal/` (Phase 4 completion).

## What this step did

- `capability/{mod,queries,locks}.rs` rewritten so every function takes `db: &impl ConnectionTrait`
  instead of `pool: &PgPool`, and returns `Result<_, sea_orm::DbErr>` instead of
  `Result<_, sqlx::Error>`. Every hand-written SQL string is preserved *verbatim* — this is not an
  Entity/`Select`-builder rewrite; each query executes via `sea_orm::Statement::from_sql_and_values`
  + `ConnectionTrait::query_one_raw`/`query_all_raw`, the same raw-SQL-through-`ConnectionTrait`
  idiom `migrator/mod.rs` already established in `GSR-PHASE-1`. Chosen deliberately over a full
  builder rewrite: capability's ~15 queries are hand-tuned multi-join `EXISTS`/`FOR UPDATE`
  statements, and re-deriving each one in `Select`/`Condition` form would risk subtle behavioral
  drift a byte-for-byte-preserved raw statement cannot have.
- The one text change: `authority_assignments`'s SQL gained three column aliases (`project_id`,
  `approval_view`, `approval_decide`) on its first `UNION ALL` branch. The original used positional
  `sqlx::query_as::<(Uuid, Option<Uuid>, bool, bool)>` tuple decoding, which never needed column
  names; the port decodes by name via `QueryResult::try_get_by::<T, _>("column_name")`
  (`GSR-PERSISTENCE`'s stated goal), and PostgreSQL derives a `UNION`'s output column names from its
  *first* branch only — every other branch's matching alias was already implicit from position and
  needed no change.
- The `Vec<Uuid>` `= ANY($1)` array-bind case (`locks::lock_deployment_approval_authority_page`)
  needed a manually constructed `sea_orm::Value::Array(ArrayType::Uuid, Some(Box::new(...)))`: sea-
  query has no blanket `Vec<T> -> Value` conversion for arbitrary bindable types, only the specific
  array-type variants its own value.rs enumerates (confirmed `ArrayType::Uuid` exists, gated behind
  the `with-uuid` feature the workspace already enables).
- `capability::locks::lock_deployment_approval_authority_page`'s four `sqlx::query(...).fetch_all()`
  calls (previously *not* routed through the shared `lock()` helper `lock_project_role_authority`
  uses) now share that same helper — a small simplification once both were transcribed to the same
  `ConnectionTrait` shape, not a behavior change.

## Scope correction: `capability::tx.rs` is deliberately untouched

The design doc's module-4 text assumed `queries.rs`/`tx.rs`'s duplication would collapse into one
`ConnectionTrait`-generic implementation in this same phase. Grepping `tx::` consumers before
touching it found five modules depend on it, not the one the phase ordering implied:
`agent::draft` and `configuration::mutations` (phase P6), `administration::mutations` (phase P6),
and `deployment::{queries,mutations}` and `evaluation::{queries,mutations}` (phase P7) — none of
which are ported yet, and all of which still open their write transactions via raw
`sqlx::Pool::begin()` / `&mut PgConnection`.

Rewriting `tx.rs` now would force a bad choice: either pull all five modules' transaction
management forward out of dependency order in this one commit, or run `tx.rs`'s locked capability
check on a *different* physical connection than the write transaction it exists to protect —
silently breaking the "hold this row lock until the caller's own commit" guarantee, a correctness
regression `capability_integration`'s own tests cannot catch (they compare a locked answer against
an unlocked one on a bare, transaction-less connection; they never assert a lock actually blocks a
concurrent writer). `tx.rs` stays on `sqlx`/`&mut PgConnection`, unchanged, and shrinks module-by-
module as `agent::draft`/`configuration`/`administration` (P6) and `deployment`/`evaluation` (P7)
each port their own transaction management in their own phase; it is deleted once its last caller
is gone, matching the design doc's own item 13 framing for `sql.rs`.

## The compile-forced ripple into five not-yet-ported modules

`capability`'s public functions are called from `console`, `configuration::{queries,mutations}`,
`administration::{queries,mutations}`, `audit`, and `agent::draft` — none of which are otherwise in
scope for this phase, but all of which had to be *touched* the moment `capability`'s parameter type
changed, since a Rust crate cannot partially compile. Each of the affected `Pg*Repository` structs
gained a `db: sea_orm::DatabaseConnection` field (alongside their existing `pool: PgPool`, unchanged)
threaded from a new second constructor argument; every construction site in `hive-api/src/schema/
{administration,configuration,audit,agent,console}.rs` now passes
`ctx.data::<sea_orm::DatabaseConnection>()?.clone()` alongside the existing `ctx.data::<PgPool>()?
.clone()`. None of these five modules' *own* SQL was touched — only the one parameter each needed to
keep reaching `capability` — mirroring `GSR-PHASE-1`'s `ConnectionFactory::pool()`-not-removed
correction (a shared resource's signature change forces minimal collateral edits everywhere it's
used, independent of which phase "owns" porting that caller's own internals).

Two mapping helpers were added where a file's existing `other(error: sqlx::Error) -> RepositoryError`
couldn't accept `capability`'s new `sea_orm::DbErr`: `configuration::rows::other_db` and
`agent::draft::other_db`, both `RepositoryError::Other(error.into())` — identical in shape to `other`,
since `anyhow::Error`'s blanket `From<E: std::error::Error + Send + Sync>` accepts `DbErr` exactly
as it already accepted `sqlx::Error`. `administration::queries.rs` and `audit::mod.rs` needed no new
helper: the former already built `RepositoryError::Other(error.into())` inline at each capability
call site, and the latter's `AuditError` already discards the underlying error via
`.map_err(|_| AuditError::Dependency)`.

## Verification

- `cargo build --workspace --exclude hive-console --tests`: clean after each mechanical step
  (capability rewrite, each of the five rippled-into files, the `capability_integration` test file).
- `cargo fmt --all` + `cargo clippy --workspace --exclude hive-console --all-targets -- -D warnings`:
  clean (four administration mutation functions gained
  `#[allow(clippy::too_many_arguments)]`, matching the file's own existing precedent on its other
  five write commands, once the new `db` parameter pushed them from 7 to 8 arguments).
- `cargo test -p hive-persistence --test capability_integration -- --ignored`: 13/13 pass, including
  `locked_has_capability_matches_the_unlocked_answer`/`locked_evaluation_capabilities_matches_the_
  unlocked_answer`/`locked_deployment_approval_capabilities_many_matches_the_unlocked_answer` —
  proving the transcribed locking SQL (including the hand-built `Value::Array` bind) behaves
  identically to the original.
- `check:integration:console`: passes (console's own capability wiring, unchanged in behavior).
- `check:integration:administration`, `agent-draft-editor`, `agent-authoring`, `configuration`,
  `audit`, `audit-transaction`, `audit-plan`: all pass — confirming the compile-forced `db` threading
  into these five not-yet-ported modules changed no observable behavior.
- `check:rust:database`: 64/64 `http_integration` tests, 13/13 `capability_integration`, both other
  suites clean.
- `check:architecture`: 22 hive-api source files, 0 findings (unchanged from phase 4).
- `check:rust`: clean (fmt + clippy + full workspace test run).
- Full `validate:local` against commit `68668af` (this phase's implementation commit):

  ```
  validate:local candidate=68668aff92696e35c9d08a86357c87cae429c61e tree=d68d92614676b090bff892c4b817fe381d130cc7 state=clean checks=49 verified-after-checks
  ```

  All 49 checks passed.
