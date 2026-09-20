# Rust Idiom Remediation Plan

## Purpose

Four parallel audit agents reviewed `hive-rust` on September 18, 2026, checking whether the code that implements [the transformation](./rust-transformation-design.md) reads as idiomatic Rust or carries over Java/Quarkus implementation patterns from the source project at `/home/richard/projects/hive`. The audit covered `hive-domain` and `hive-application` (business logic), `hive-persistence` (the sqlx repository layer), `hive-api` and the `hive` binary (the HTTP/GraphQL layer and CLI), and the workspace structure itself (crate boundaries, `Cargo.toml` organization, lint/format tooling). This plan records every finding as an immutable-identifier item, states the remediation decision for each, and tracks completion. Each item's own section states the exact command a reader can run to see its CURRENT count or state — this plan cites no transcribed occurrence counts, because every fix in progress changes those counts; the commands stay accurate for as long as the finding remains open.

This plan governs code-quality remediation only. It has no dependency relationship with the `RTP-*` phases in [the implementation plan](./rust-transformation-implementation-plan.md); those phases port and validate behavior, this plan improves the idiom of already-ported, already-passing code.

## Status legend

`Open` — not started. `In progress` — actively being worked. `Done` — fixed, built, tested, committed; the item's own citation command confirms the fix. `Deferred` — evaluated and intentionally not done in this pass, with the reason recorded in the item.

## RIDR-AUDIT-ACTION-ENUM

Status: Deferred (audit finding corrected on inspection, not acted on).

The audit flagged `hive-api/src/schema/audit.rs` for converting the application layer's `AuditAction` enum to a plain `String` at the GraphQL boundary (`AuditEvent.action`, `AuditEventFilter.action`), reading it as a discarded idiom. Direct comparison against Java's own committed schema (`/home/richard/projects/hive/service/src/main/resources/graphql/organization-directory.graphql:381,415`) shows Java declares both fields `String`/`String!` as well — there is no GraphQL enum for this concept in the contract this project ports. Converting Rust's wire type to a GraphQL enum would be a schema divergence from Java, not a fix: it would add a type Java's schema does not declare, contradicting the porting discipline this codebase already follows and the schema-parity work verified earlier in this project's history (`docs/rust-transformation-design.md`'s schema-fidelity checks). The `.as_str().to_string()` conversion at the boundary is correct, necessary serialization, not a Java-ism. No action taken; recorded here so the audit's original framing does not get re-proposed later without this context.

## RIDR-DEPLOYMENT-LIFECYCLE-ENUM

Status: Done.

`Deployment.lifecycle_status` and the deployment outbox worker's status checks were typed `String` end to end, and independently maintained literal string sets in `hive-persistence/src/deployment/mutations.rs`, `worker.rs`, and `approval.rs` each described the one state machine. Nothing stopped those sets drifting apart from each other or from the GraphQL enum's variant list.

`hive-domain::deployment::DeploymentLifecycleStatus` now owns the state machine: `as_str`/`FromStr` hold the one string list, and `is_terminal`, `is_cancellable`, `awaits_execution`, and `has_started_execution` are exhaustive `match` expressions, so a new variant fails compilation until each predicate classifies it. `hive-persistence/src/deployment/rows.rs`'s `lifecycle_status` parses the column once at the row boundary; `Deployment.lifecycle_status` and `DeploymentFilter.lifecycle_status` carry the enum. The GraphQL-layer enum lost its own `parse`/`value` string lists and converts through two `From` impls with exhaustive matches. The unit tests in `hive-domain/src/deployment.rs`'s `lifecycle_status_tests` pin each predicate to the literal set it replaced. String literals that remain under `hive-persistence/src/deployment/` are SQL text, audit action names, and attempt/stage values, none of which compare a lifecycle status in Rust. `schema-sdl` output stayed byte-identical against commit `ae80910`, and the workspace build, clippy, `cargo fmt --check`, `--lib` unit tests, and the `http_integration` `--ignored` suite passed on September 18, 2026.

Verify current state: `grep -rn "lifecycle_status: \(Option<\)\?String" crates/hive-application/src/deployment/` returns no matches, and `grep -rn "lifecycle.*as_str(), \"" crates/hive-persistence/src/deployment/` returns no literal-set comparison.

## RIDR-EVALUATION-RUN-STATUS-ENUM

Status: Done.

`hive-application/src/evaluation/state_machine.rs` expressed an entire state machine as `&str` equality against `"QUEUED"`/`"RUNNING"`/`"COMPLETED"`/`"FAILED"`/`"CANCELED"`, and `hive-persistence/src/evaluation/worker.rs`, `mutations.rs`, and `queries.rs` each re-checked the same set independently instead of calling it.

`state_machine.rs` now defines `EvaluationRunStatus`, re-exported from `hive_application::evaluation`. The enum lives there rather than in `hive-domain` because `hive-domain` holds no evaluation module and the state machine it types already lives in `hive-application`, which `hive-persistence` and `hive-api` both depend on. `evaluation_runs.lifecycle_status` and `evaluation_case_runs.lifecycle_status` admit the same value set in `db/migration/V039__evaluation_definitions_runs_and_evidence.sql`, so the one enum types both. `as_str`/`FromStr` hold the one string list; `may_start_run`, `may_finalize`, `may_cancel`, `is_terminal`, `may_start_case`, and `may_complete_case` are exhaustive `match` expressions. `hive-persistence/src/evaluation/rows.rs`'s `run_status` parses the run column once at the row boundary. `RawRun.status`, `EvaluationRun.lifecycle_status`, `EvaluationWorkItem.current_lifecycle_status`, `EvaluationWorkDecision::Start`, `EvaluationExecutionDecision`, `EvaluationFinalizationDecision`, the `update_run`/`update_case` writers, and the `runs` status filter on `EvaluationRepository`/`EvaluationService` all carry the enum. The persistence checks call `may_cancel`/`is_terminal` instead of restating the sets, and the `runs` query dropped its own five-literal validity check because the filter's type now guarantees it. The GraphQL-layer enum converts through two exhaustive `From` impls. The unit tests in `state_machine.rs` pin each predicate to the literal set it replaced. String literals that remain under `hive-persistence/src/evaluation/` are SQL text, audit action names, outcome categories, and worker-heartbeat states. `schema-sdl` output stayed byte-identical against commit `ae80910`, and the workspace build, clippy, `cargo fmt --check`, `--lib` unit tests, and the `http_integration` `--ignored` suite passed on September 18, 2026.

Verify current state: `grep -n "current: &str\|run: &str" crates/hive-application/src/evaluation/state_machine.rs` and `grep -rn 'status.as_str(), "' crates/hive-persistence/src/evaluation/` both return no matches.

## RIDR-APPROVAL-STATUS-ENUM

Status: Done.

`ApprovalRequirement.status` / `ApprovalDecisionFacts.status` were typed `String` in `hive-domain/src/deployment.rs` and `hive-application/src/deployment/models.rs`, and `hive-application/src/deployment/policy.rs`, `hive-persistence/src/deployment/approval.rs`, and `hive-persistence/src/deployment/queries.rs` compared them against string literals.

`hive-domain::deployment::ApprovalRequirementStatus` now names the values the `deployment_approval_requirements.status` CHECK constraint admits (`PENDING`, `SATISFIED`, `REJECTED`, `EXPIRED`, `INVALIDATED`), with `as_str`/`FromStr` holding the one string list. `hive-persistence/src/deployment/rows.rs`'s `requirement_status` parses the column once at the row boundary. `RawRequirement.status`, `ApprovalDecisionFacts.status`, `ApprovalRequirement.status`, and `transition_requirement`'s target-status parameter carry the enum, so every Rust-side comparison names a variant. The GraphQL-layer enum lost its own `parse` string list and converts through an exhaustive `From` impl. This state field has no set-valued predicates — every check is a single-variant equality — so the enum adds no predicate methods. String literals that remain in those files are SQL text and audit action names. `schema-sdl` output stayed byte-identical against commit `ae80910`, and the workspace build, clippy, `cargo fmt --check`, `--lib` unit tests, and the `http_integration` `--ignored` suite passed on September 18, 2026.

Verify current state: `grep -rn 'status [!=]= "\(PENDING\|SATISFIED\|REJECTED\|EXPIRED\|INVALIDATED\)"' crates/hive-application/src/deployment/ crates/hive-persistence/src/deployment/` returns no matches.

## RIDR-REMAINING-STATE-FIELDS

Status: Deferred.

The audit found 30+ additional `String`-typed closed-set fields beyond the three items above, spanning `kind`/`state`/`outcome`/`strategy`/`event_type` across the configuration, agent, organization, console, and administration domains (for example `TypedReference.kind`, `DeploymentEvidence.kind`, `Deployment.strategy`, `BudgetStatus.state`). Converting all of them in this pass would touch the large majority of files in `hive-application` and `hive-persistence` with no incremental build/test checkpoint between most of them, which is a correctness risk disproportionate to a style fix. Deferred as a tracked backlog rather than silently dropped: the four items above establish the pattern (domain enum, parsed once at the persistence boundary, transport layer derives from it) that a future pass applies to the rest, domain by domain.

Verify current state: `grep -rln "pub \(lifecycle_status\|status\|action\|kind\|state\|outcome\|strategy\|event_type\): \(String\|&str\|Option<String>\)" crates/hive-domain/src crates/hive-application/src` lists every file with at least one remaining occurrence.

## RIDR-ADMINISTRATION-REPO-SPLIT

Status: Done.

`hive-persistence/src/administration/mod.rs` was one file holding both an inherent `impl PgAdministrationRepository` block and the trait `impl`, mixing the write commands with unrelated private helpers in one undifferentiated block. Five sibling domains (`deployment`, `evaluation`, `capability`, `agent`, `organization`) already split into `queries.rs`/`mutations.rs`/`rows.rs` with a thin dispatcher `impl`; this file did not follow the pattern already established in its own crate.

Split along the same lines as `deployment/`: `queries.rs` holds `find_organization`/`find_project`, `mutations.rs` holds the write commands and the transaction-scoped capability re-check they share, `rows.rs` holds the scope-to-table-name encodings, the approval policy matrix's JSON/digest encoding, and the locked single-row fetchers both other modules use. `mod.rs` keeps only the struct, its constructor, and the `AdministrationRepository` trait `impl`, which dispatches one call per method into `queries` or `mutations`. The move changed no SQL and no function body. `cargo run -p hive -- schema-sdl` emitted byte-identical SDL from the pre-split commit `ae80910` and from the split tree on September 18, 2026, and the workspace build, clippy, `cargo fmt --check`, the `--lib` unit tests, and the `http_integration` `--ignored` suite all passed on the split tree the same day.

Verify current state: `grep -c "sqlx::query" crates/hive-persistence/src/administration/mod.rs` returns 0, because every query lives in a submodule.

## RIDR-CONFIGURATION-REPO-SPLIT

Status: Done.

Same finding as `RIDR-ADMINISTRATION-REPO-SPLIT`, for `hive-persistence/src/configuration/mod.rs`. Split the same way: `queries.rs` holds `catalog`/`resources`/`resource`/`mcp_servers`, `mutations.rs` holds the write commands with their inline serialization-failure retries, `rows.rs` holds the pure content diagnostics, reference resolution, read assembly, locked single-row fetchers, and the draft/audit writes and digests. `mod.rs` keeps the struct, its constructor, and the one-call-per-method trait `impl`. The move changed no SQL and no function body; the module doc comment's reference to a `transaction_with_conflict` helper that never existed now describes the inline retry the code uses. `cargo run -p hive -- schema-sdl` emitted byte-identical SDL against commit `ae80910` on September 18, 2026, and the workspace build, clippy, `cargo fmt --check`, the `--lib` unit tests, and the `http_integration` `--ignored` suite all passed the same day.

Verify current state: `grep -c "sqlx::query" crates/hive-persistence/src/configuration/mod.rs` returns 0.

## RIDR-HEALTH-SERIALIZE-STRUCT

Status: Done.

`hive-api/src/health.rs`'s three handlers built their JSON response bodies by hand with `serde_json::Map::new()` plus sequential `.insert(...)` calls, including conditional inserts to omit a field rather than serialize `null` — a direct port of Java's `Map<String,Object>` health-response construction. Replaced with `#[derive(Serialize)]` response structs using `#[serde(skip_serializing_if = "Option::is_none")]` for the conditional fields.

Verify current state: `grep -n "serde_json::Map::new()" crates/hive-api/src/health.rs` returns no matches once fixed.

## RIDR-GRAPHQL-ERROR-ENVELOPE

Status: Done (with a correction to the audit's proposed fix).

`hive-api/src/graphql.rs` hand-built the same `{"errors": [{"message": ...}]}` JSON envelope at three separate call sites. The audit proposed reusing `async_graphql::Response`'s own shape instead; checked against that type's `Serialize` impl (`data` has no `skip_serializing_if`, so it always emits `"data":null`) and against `DirectoryServer.java`'s three equivalent call sites (lines 76, 95, 97, `json(status, Map.of("errors", ...))`, no `data` key) — Java never emits a `data` key for these three transport-level, pre-execution checks, so switching to `async_graphql::Response` would have added one Java does not have, a regression rather than a fix. Instead added a local `transport_error(status, message)` helper that builds the exact same no-`data`-key shape the three call sites already produced, collapsing the duplication without changing the wire format.

Verify current state: `grep -c "json!({\"errors\"" crates/hive-api/src/graphql.rs` drops from 3 to 1 (the one remaining occurrence is inside the `transport_error` helper itself).

## RIDR-PAGEINFO-DEDUP

Status: Done (audit's proposed merge rejected; the underlying doc comment was the only real defect).

The audit proposed pointing `deployment.rs`/`audit.rs`/`evaluation.rs` at the shared `connection::PageInfo` instead of each redefining an identical-looking two-field struct. Checked against Java's committed schema first, given the last two items' lesson: Java declares three genuinely distinct types here, not one — `PageInfo` (four-field Relay shape, used only by `organization`/`project` connections), `DeploymentPageInfo` (two-field, its own named type), and `AuditPageInfo` (two-field, its own named type) — and `evaluation`'s connections inline `hasNextPage`/`endCursor` as flat fields with no nested page-info object at all. Merging any of these into `connection::PageInfo` would collapse distinct GraphQL type identities into one, a schema regression, not a cleanup — the three "duplicate" structs are required precisely because Java names them differently. The one genuine defect was `connection::PageInfo`'s own doc comment, which claimed "every connection reuses" it; corrected to name exactly which two domains do and why the other three must not.

Verify current state: `crates/hive-api/src/schema/connection.rs`'s doc comment now names `DeploymentPageInfo`/`AuditPageInfo`/evaluation's flat fields explicitly instead of claiming universal reuse.

## RIDR-DEPLOYMENT-CONNECTION-MACRO

Status: Done.

`hive-api/src/schema/evaluation.rs` solves repeated `XxxConnection { edges, page_info }` / `XxxEdge { cursor, node }` boilerplate with two `macro_rules!` (`connection_type!`, `from_app_connection!`). `hive-api/src/schema/deployment.rs` had the same repeated shape five times over and hand-wrote every one. Unlike `RIDR-PAGEINFO-DEDUP`, this carried no schema risk to check first: the macro generates five separately-named structs (`DeploymentConnection`, `DeploymentTimelineConnection`, `DeploymentEnvironmentDefinitionVersionConnection`, `ApprovalDecisionConnection`, `ApprovalInboxConnection`), each keeping its own distinct GraphQL type name — it only removes repetition in how those five are *authored*, not how many distinct types the schema declares. Added `deployment_connection_type!`/`from_app_deployment_connection!` (adapted from evaluation.rs's pair for this domain's `nodes`/`cursors`-parallel-vector application-layer connection shape, versus evaluation's single `edges` list) and replaced all five hand-written blocks with macro invocations. Confirmed zero wire impact by diffing `hive schema-sdl`'s full output before and after: byte-identical.

Plan: move the two macros to a shared location (`schema/connection.rs` is the natural home, alongside `PageInfo`) and apply them in `deployment.rs`.

Verify current state: `grep -c "^pub struct.*Connection {" crates/hive-api/src/schema/deployment.rs` drops toward 0 as hand-written connection structs are replaced by macro invocations.

## RIDR-CONSOLE-FROM-IMPL

Status: Done.

`hive-api/src/schema/console.rs`'s `console_context` resolver inlined a 3-level nested struct conversion (Organization → ConsoleOrganization → ConsoleProject) directly in the query method via chained closures. Every sibling domain file in `schema/` factors this into `impl From<AppX> for X`; console.rs was the one outlier. Extracted `From<AppConsoleProject>`, `From<AppConsoleOrganization>`, `From<AppConsoleCapability>`, and `From<AppConsoleContext>` impls, matching the sibling-file convention exactly (same aliased-import style already used for `DisplayPreferencesProblem` in the same file). The resolver body is now `Ok(context.map(ConsoleContext::from))`. Confirmed zero wire impact via a full `hive schema-sdl` diff (byte-identical) and the `console_context_*` integration tests plus the full suite.

Verify current state: `crates/hive-api/src/schema/console.rs`'s `console_context` resolver body is a single `.map(ConsoleContext::from)` call, no inline closures.

Verify current state: `grep -c "impl From<" crates/hive-api/src/schema/console.rs` is 0 before the fix.

## RIDR-REDUNDANT-CLONE

Status: Done.

Clippy's `nursery` lint group (not part of this project's normal gate) flagged two genuine `redundant_clone` hits: `hive-persistence/src/evaluation/queries.rs:142,148`, both `if can_author { x.clone() } else { redacted(&x) }` where `x` is never used again afterward. Removed both `.clone()` calls — the borrow checker accepts moving in the `if` branch and borrowing in the mutually-exclusive `else` branch without any restructuring needed.

Verify current state: `cargo clippy -p hive-persistence --all-targets -- -W clippy::redundant_clone 2>&1 | grep -c redundant_clone` is 0.

## RIDR-DEPLOYMENT-SERVICE-CLONE

Status: Done.

`hive-application/src/deployment/service.rs:92` passed `&request` (a local, single-use `CompiledRequest` never touched again afterward) into `preview()`, which individually cloned 19 of its fields. Changed `preview()` to take `CompiledRequest` by value and destructure it, moving every field instead of cloning. Two fields are read twice in the original code (`environment.stable_definition_id`, `rule.evidence`); reordered so the first read is a non-consuming borrow (`format!`'s `Display` argument, `.iter().any(...)`) before the second read moves the field — no clone needed even for those two.

Verify current state: `grep -c "\.clone()" crates/hive-application/src/deployment/service.rs` around the `preview` function drops to 0 for that function's body.

## RIDR-BARE-STRING-ERRORS

Status: Done.

Four functions returned `Result<_, String>` instead of this codebase's own established `thiserror` enum convention: `hive-application/src/audit/filter.rs` (`AuditFilter::new`), `audit/resource_reference.rs` (`AuditResourceReference::new`), `audit/action.rs` (`AuditAction::parse`), `evaluation/decider.rs` (`decide`).

Checked every caller first (the plan's own stated condition): all three audit constructors' errors are discarded by every caller (`hive-api/src/schema/audit.rs`'s `build_filter`/`action` helpers map to `()`; `hive-persistence/src/audit/mod.rs` either matches away the content or `.expect()`s), so the message text carries no behavior — added one shared `ValidationError(String)` (`#[derive(thiserror::Error)]`, `#[error("{0}")]`) in `audit/mod.rs` for these three rather than `AuditError` (the existing repository-level enum), since that type also carries `Unavailable`/`Dependency` variants a pure, I/O-free constructor could never produce. `evaluation::decider::decide`'s error *is* read by its one caller (wrapped into `anyhow::anyhow!(message)`, preserving the text) and has two genuinely distinct semantic kinds, so it got a proper two-variant `DecideError` enum instead of a message wrapper.

Verify current state: `grep -rn "Result<.*, String>" crates/hive-application/src/audit crates/hive-application/src/evaluation/decider.rs` returns no matches.

Verify current state: `grep -rn "Result<.*, String>" crates/hive-application/src/audit crates/hive-application/src/evaluation/decider.rs`.

## RIDR-CRATE-BOUNDARIES

Status: Deferred.

The 5-crate split (`hive-domain`/`hive-application`/`hive-persistence`/`hive-api`/`hive`) mirrors Java's `domain`/`application`/`persistence`/`api` package layering; the design document states this directly (`RTD-CRATE-DIRECTION` in [the design](./rust-transformation-design.md)) as replicating a Java conformance script's rule in Cargo's dependency graph, not as isolating a heavy dependency, serving an external consumer, or breaking a compile-time bottleneck (`hive-persistence` and `hive-api` both depend on `sqlx` directly, so no isolation is actually achieved by the split).

Deferred, not fixed: collapsing or restructuring the workspace's crate boundaries is the single highest-blast-radius change this audit surfaced — it touches every file's `use` paths, every crate's `Cargo.toml`, and the design document's own stated rationale for the current shape, for a benefit that is organizational clarity rather than a correctness, performance, or maintainability defect. `hive-domain` in particular is independently defensible on Rust-native terms regardless of the Java-conformance framing (small, genuinely dependency-free, consumed by multiple sibling crates). Recommendation: leave the workspace topology as-is. Revisit only on explicit request, since undoing it is far more expensive than the status quo's cost.

## RIDR-SEA-ORM-DEAD-DEPENDENCY

Status: Deferred.

`sea-orm` is declared in the workspace `[workspace.dependencies]` table but consumed by zero files under `crates/`. [The design document](./rust-transformation-design.md) (lines 137-216) specifies it as the intended object-relational mapping layer for the Seaography-generated read tier `RTD-OPEN-SEAOGRAPHY-RC` covers — a forward declaration for planned, not-yet-built work, not an accidental leftover.

Deferred, not removed: removing it now would contradict the design document's own stated plan for a feature this audit did not evaluate for necessity. Recommendation: leave declared; revisit only alongside a decision on `RTD-OPEN-SEAOGRAPHY-RC` itself.

Verify current state: `grep -rl "sea_orm" crates/` returns no matches (confirms the dependency is still unconsumed, matching this entry's premise).

## RIDR-FROM-ROW

Status: Deferred.

`sqlx::FromRow` is used nowhere in `hive-persistence` (`grep -rn "FromRow" crates/hive-persistence/src/` returns no matches); every row mapper hand-writes positional `row.get(0)`, `row.get(1)`, ... decoding, which a `SELECT` column-list reorder could silently mis-map with no compile error. The audit found no live bug from this — it is a missing safety net, not a confirmed defect — but retrofitting is a comparable scale of change to `RIDR-REMAINING-STATE-FIELDS` (roughly 40 files).

Deferred, not silently dropped: recommend adopting `#[derive(FromRow)]` for any row-mapping function touched by future work (including the domain-enum conversions above, which already touch these mappers), rather than a dedicated retrofit pass across untouched code.

Verify current state: `grep -rn "FromRow" crates/hive-persistence/src/` (0 matches confirms the deferred state persists; any positive count means a future pass has started adopting it).

## Clippy and formatting policy

Status: Deferred.

No `clippy.toml`, `rustfmt.toml`, or `[lints]` table exists anywhere in the workspace; every gate this project runs uses clippy's and rustfmt's un-customized defaults (`cargo clippy --workspace -D warnings`, no pedantic/nursery groups). A pedantic+nursery sweep for this audit surfaced a large volume of stylistic lints (missing `# Errors` doc sections, missing `#[must_use]`, numeric-cast lints typical of moving between Postgres integer columns and Rust counters) that are expected noise for a codebase this size on a stricter lint tier the project has never opted into. Recommendation: no action — adopting pedantic/nursery workspace-wide is a policy decision with a large one-time cleanup cost, out of scope for this remediation pass, which targets confirmed idiom gaps rather than raising the lint bar itself.
