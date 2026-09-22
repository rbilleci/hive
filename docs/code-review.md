# Code review: idiomatic Rust and structure

September 21, 2026, against `ec4da13`. Two reviews, one for Rust idiom and one for structure
across the crates, each reading the source directly. Every finding below carries a location; the
ones marked *verified* were re-checked by hand against the source after the reviews reported them.

The codebase is in good shape. The layering holds, the generated read tier is the best-organised
part of it, and `cargo clippy -D warnings` is clean everywhere — so nothing here is a lint. What
follows is what a linter cannot see. Nothing in this document is a reason to reorganise for its
own sake: most of the structure is right, and the list of what should not be touched is at the end.

| Crate | Lines | Files |
| --- | --- | --- |
| `hive-persistence` | 28,004 | 124 |
| `hive-console` | 15,325 | 39 |
| `hive-application` | 7,447 | 38 |
| `hive-api` | 3,926 | 19 |
| `hive-domain` | 424 | 3 |
| `hive` | 263 | 1 |

## Must fix

These are correctness or safety problems, not preferences.

### 1. A panic reachable from the worker and the maintenance loop — *verified*

`deployment/rows.rs:603` takes a typed SeaORM enum, renders it to a `String`, parses it back, and
panics if the parse fails:

```rust
pub fn lifecycle_status(value: String) -> DeploymentLifecycleStatus {
    value.parse().unwrap_or_else(|error| panic!("{error}"))
}
```

The doc comment says this guards schema drift. It cannot: the input arrives as an `ActiveEnum`, so
a drifted column fails at row deserialization long before this line. The panic guards an
unreachable branch while sitting in a live path — `approval.rs:801` and `:1248` reach it from the
outbox worker's transaction and the one-second maintenance loop. Five of these exist
(`deployment/rows.rs:603` and `:610`, `evaluation/rows.rs:72`, `evaluation/computed.rs:220`,
`evaluation/worker.rs:582`).

Replace each with `impl From<EntityStatus> for DomainStatus` — an exhaustive match, infallible, no
allocation. The panics and the string round-trip both disappear.

### 2. `authority.rs` fails silently and has no tests — *verified*

944 lines, zero `#[cfg(test)]`. `read_condition` dispatches on 50 GraphQL type names and falls
through to `_ => return None` (`:166`), which `tenant_hooks.rs:40` turns into `deny_all`.

Registering an entity in `schema/mod.rs` without adding an access rule compiles, passes clippy,
passes every gate, and returns an empty result to every caller with no error and no log entry. The
failure is silent and shaped like a security bug, in the one file that decides who sees what.

`schema/mod.rs:428` already parses its own source with `include_str!` to count registrations, so
the mechanism for a test exists: assert every registered entity has a `read_condition` arm. Add
unit tests for the conditions themselves while you are there.

### 3. Every repository failure becomes HTTP 503 — *verified*

`RepositoryError` is one variant in six places (`deployment/repository.rs:18`,
`evaluation/repository.rs:12`, `configuration/repository.rs:10`, `administration/service.rs:10`,
`console/service.rs:6`, `agent/draft_service.rs:11`):

```rust
pub enum RepositoryError { #[error(transparent)] Other(#[from] anyhow::Error) }
```

`deployment/mod.rs:72` erases the `DbErr` into it, and `schema/deployment.rs:98` concedes the
consequence in a comment: every error becomes the marker that answers 503. A unique-violation or a
logic bug is reported to the client as "temporarily unavailable" — and a well-behaved client
retries it in a loop.

Give the type the variants the API already branches on (`Unavailable`, `Conflict`, `NotFound`) and
classify at the persistence edge using `retry.rs`'s existing `is_serialization_failure_db` and
`is_unique_violation_db`.

### 4. Two functions named `map_error` with opposite meanings — *verified*

`schema/deployment.rs:100` attaches `DependencyUnavailable`, which makes the handler answer 503.
`schema/evaluation.rs:47` — same name, same signature — does not. A further 24 sites use an inline
closure that also drops the source. So the same class of storage failure answers 503 in deployment
and 200-with-an-error in administration, configuration, agent and console.

One `schema::errors::storage_failure()` beside `DependencyUnavailable`; delete both `map_error`s
and the 24 closures.

### 5. The console re-implements server rules

`pages/deployment.rs:43`'s `is_terminal` duplicates `hive-domain/src/deployment.rs:43` variant for
variant (*verified*). `pages/evaluation.rs:31` duplicates the evaluation state machine;
`api/administration.rs:555`'s `NINE_CELLS` is `administration/rules.rs:23` verbatim, same order;
`pages/deployment.rs:674` re-derives the cancel, retry, promote and rollback preconditions.

These drift independently of the server and are a correctness risk, not untidiness. The correct
pattern is already in the codebase: `decisionAvailable` is computed server-side
(`deployment/computed.rs:631`) and simply read by `pages/approval.rs:267`. Do the same for the
action preconditions and the policy cells.

### 6. A mutex held across a database round-trip, inside a per-row resolver

`audit/mod.rs:61` takes a `tokio::sync::Mutex` guard, then awaits a capability query while holding
it. Three computed fields call it per audit row, so a 200-row page serialises 600 resolutions
behind one lock. Drop the guard before the await and re-take it to insert — a duplicate evaluation
is harmless — or key an `Arc<OnceCell<bool>>` per scope.

### 7. The maintenance loop cannot be shut down

`hive-api/src/lib.rs:99` spawns `maintenance::run` and drops the `JoinHandle`;
`maintenance.rs:16` loops forever with no shutdown arm, while only axum receives the shutdown
future. On SIGTERM the task is dropped by the runtime, possibly mid-transaction. Select the loop
against a cancellation token and await the handle after `axum::serve` returns.

### 8. Validate here, unwrap there

`deployment/service.rs:34`, `:241`, `:243`; `deployment/mutations.rs:1140`, `:1145`;
`administration/service.rs:411`:

```rust
comment: (!blank(comment)).then(|| comment.unwrap().trim().to_string()),
```

Safe today, but the invariant lives in another function. Widening `blank` introduces a panic in a
mutation handler with no local evidence. Have the validator return the value:
`fn present(v: Option<&str>) -> Option<&str>`.

## Should fix

### 9. The commit-retry is written seven times

Aurora DSQL's serialization-failure-at-commit handling appears at `evaluation/mutations.rs:562`,
`:759`, `:897`, `:1229`, `deployment/mutations.rs:501`, `:640` and `agent/draft.rs:799`. Identical
control flow, different re-read. `configuration/mutations.rs:80` already has the right shape as
`committed(db, txn, …)`. Lift it to `crate::retry::committed` generic over the re-read closure.
This is the largest mechanical duplication in the crate.

### 10. The revision guard is written about 39 times

Twenty `WHERE`-clause revision guards plus nineteen read-then-compare sites across the five write
domains (`agent/draft.rs:231`, `administration/rows.rs:129`, `configuration/mutations.rs:288`,
`evaluation/mutations.rs:275`, `deployment/mutations.rs:431` and so on). Mechanically identical
apart from the entity and the payload columns. `administration/mutations.rs:181` already extracted
`lost_update`; nothing else adopted it. Add `crate::guard::bump(entity, key, expected, cols)`.

### 11. Forty-six suppressed `too_many_arguments` lints

`ConfigurationRepository::create_mcp_server` (`configuration/repository.rs:60`) takes 15 positional
parameters, four of them adjacent `String`/`Option<String>` — swapping two compiles cleanly and
fails at runtime. Replace with a named struct plus `Default`. Same for
`AdministrationRepository::save_project_connection` and `EvaluationRepository::run_evaluation`.

### 12. Closed value sets still typed as `String`

`deployment/models.rs:63` has `status`, `state`, `risk` and `strategy` as `String` in the same
struct where `lifecycle_status` is a proper domain enum. The wire layer already enumerates every
one of them. Lift the remaining six into `hive-domain`. Related: evidence codes are allocated
strings compared by string (`approval.rs:717`, `:758`, `:849`) in the hottest decision path.

### 13. N+1 in computed fields

`deployment/computed.rs:170`, `:180`, `:196` each issue one query per parent row.
`console/mod.rs:43`'s `held()` is worse: it checks capabilities sequentially, and
`ADMINISTRATION_CAPABILITIES` has 24 codes, so a `capabilities` field costs 24 serial queries per
row. Use `async_graphql::dataloader::DataLoader` keyed on the parent id, and have `held()` fetch
the held set in one query.

### 14. Port traits that do not earn their keep

`DeploymentRepository`, `ConfigurationRepository`, `EvaluationRepository` and
`DeploymentOutboxDelivery` each have one implementation, are consumed generically rather than as
`dyn`, and have no test double. `EvaluationRepository`'s three associated types resolve to SeaORM
models, so the separation they imply is nominal. By contrast `AdministrationRepository`,
`ConsoleRepository` and `AgentDraftRepository` have in-crate fakes and do earn it. Keep the three
that are tested; collapse the others into inherent methods.

### 15. `hive-domain` implies a rule the code does not follow

424 lines: one domain's status enums plus `timestamp.rs`, an HTTP formatting helper whose only
caller is `hive-api/src/health.rs`. Evaluation's structurally identical state machine lives in
`hive-application` instead. Either move the other four domains' status types down and push
`timestamp.rs` up into `hive-api`, or fold the crate into `hive-application`. Leaving it as is
teaches a boundary that does not exist.

### 16. `rows.rs` means something different in each domain

Read-side mappers in `deployment/`, reads *and* writes *and* the audit writer in `administration/`
and `configuration/`, types and parsers in `evaluation/`. `agent/` has neither `mutations.rs` nor
`rows.rs` — `draft.rs` is 815 lines doing all of it. `computed.rs` is the one filename that means
the same thing in all five domains. Make the rest match.

### 17. Files that should split, and files that should not

Split: `deployment/approval.rs` (1,834, three concerns), `deployment/queries.rs` (1,320 — it holds
four decision-recording commands despite the name), `evaluation/mutations.rs` (1,540),
`deployment/mutations.rs` (1,151), `api/deployment.rs` (996, two domains), and the console's
`pages/evaluation.rs` (1,066, nine unrelated components) and `api/evaluation.rs` (1,126).

Do **not** split, despite the size: `authority.rs` (944) is a dispatch table, not a bag;
`administration/mutations.rs` (1,146) is ten commands under one documented locking protocol; both
workers are single engines; `deployment/computed.rs` (894) is uniform.

### 18. Console fetch and state

`pages/refresh.rs` (310 lines of jitter, backoff, visibility gating and generation-guarded
responses) is good, and two of sixteen pages use it. `pages/administration.rs:97` is a third,
parallel abstraction. Elsewhere: fifteen hand-rolled Loading/Ready/Unavailable/Error enums, the
literal `"Your session has expired."` eighteen times, sixteen `spawn_local`s in one page. Four
incompatible pagination shapes across `api/`. The `wire_enum!` layer is schema-checked and good,
and only two of nine modules use it. This is the biggest single cleanup in the console.

### 19. No lint configuration anywhere

No `[lints]` in the workspace or any crate, no `clippy.toml`, no `#![deny]`. And `hive-console` is
outside `default-members`, so a bare `cargo clippy` never sees the second-largest crate. Add
`[workspace.lints]` with `lints.workspace = true` in all six, and lint the wasm target explicitly.

### 20. Tests sit in the wrong place to support any of this

`hive-persistence` is 21,354 hand-written lines with five `#[cfg(test)]` modules, none in the five
write domains. `deployment/approval.rs` has 1,834 lines and no tests, while containing exactly the
pure, database-free logic that unit tests suit: `policy_matches`, `evidence_issue_of`,
`EvidenceFacts::valid`, `archive_boundary_condition`. Behaviour rides almost entirely on one
3,850-line `http_integration.rs` needing live Postgres. Split that file per domain and add unit
tests to the pure logic **before** attempting findings 9, 10 or 17.

## Do not change

Stated explicitly, because the tree has just been through a large rewrite and does not need churn:

- **The layering holds in spirit, not just in the manifest check.** Grepping `hive-application` for
  `sea_orm`, `DbErr` or `hive_persistence` returns two doc comments and nothing else.
- **`hive-api` reaching into `hive-persistence` is correct**: entity models *are* the GraphQL
  output types, and the orphan rule requires computed-field impls in the owning crate.
- Module file style is uniform (20 `mod.rs`, no `dir.rs` + `dir/` mixing).
- The five `ProblemKind` enums should stay distinct: they carry different refusal vocabularies that
  map to different wire codes. Unifying them would be a false abstraction.
- The generated entity tier with its coverage test and primary-key-last test; the shared `Problem`
  type; explicit `begin`/`commit` everywhere; the console's `api/` ↔ `pages/` boundary, which has
  zero cynic types outside `api/` and zero Leptos inside it.

## Suggested order

1. Findings 1, 2, 3, 4 — the latent panic, the silent deny, and the two error-mapping defects.
2. Finding 20 — split the integration test and unit-test the pure logic, so the rest is safe.
3. Findings 9, 10, 11 — the three duplications, now covered by tests.
4. Findings 5, 13 — correctness drift and the N+1s.
5. The rest as convenient. 17 and 18 are the largest and the least urgent.
