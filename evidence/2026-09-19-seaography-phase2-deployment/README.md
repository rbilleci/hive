# GSR-PHASE-2, seventh slice: `deployment.rs` — Phase 2 complete

Status: done. This is the last of the 13 static schema files; `GSR-PHASE-2` (the dynamic
Seaography GraphQL tier) is now feature-complete for every domain the frozen contract declares.
Continues after `evidence/2026-09-19-seaography-phase2-evaluation/`.

## What this step built

`crates/hive-api/src/graphql_schema/deployment.rs` (the largest file ported, 1798 static-tier
lines): the 7th and 8th interfaces this port builds — `DeploymentProblem` (7 implementors) and
`DeploymentApprovalProblem` (3 implementors) — 10 GraphQL enums (the most of any file), 5
deployment/approval-inbox queries, 5 deployment mutations, `decideDeploymentApproval`, and
`ApprovalRequirement.decisions` (a `GSR-NESTED-FIELDS` field). Also fixed a real, previously
undetected bug in `principal.rs` (see below) before `deployment.rs` could reuse it correctly.

## What this step found

**A latent bug in `principal.rs`, sitting undetected since phase 0.** `Principal.id` was a bare
`String` (printing as GraphQL `String!`) when the frozen contract's `Principal.id` is `ID!`. Silent
for 8 commits because (a) `check:schema:contract` still parses the *static* engine until phase 3,
so it can never see a dynamic-tier-only defect, and (b) every phase-2 slice's own SDL-parity check
only compares the types *that slice's file* introduces — none of them happened to re-list
`Principal`, since it belongs to `principal.rs`. Only surfaced because `deployment.rs`'s
`ApprovalRequirement.requester`/`satisfiedParticipants` were about to reuse `Principal`, prompting a
fresh look at its exact shape before reuse. Fixed (`id: scalars::Id`) and confirmed directly
against the dynamic SDL. Recorded in the design doc as a new risk-table row with a broader lesson:
**from this commit onward, verification runs a comprehensive whole-schema comparison (every shared
type between the frozen contract and the accumulated dynamic SDL, not just the current file's new
types) instead of only the incremental one.** That wider check is what `whole-schema-parity.log`
in this directory is — 215 shared types, 74 explicitly-implemented root fields, and all 7
interfaces' implementors, confirmed in one pass rather than accepting each slice's narrower claim
on faith.

**`Required<T>`'s generic bound couldn't actually admit its only real argument.** The struct
carried over from phase 2's first slice (`scalars.rs`, deferred pending a consumer) was generic:
`impl<T: CustomOutputType> CustomOutputType for Required<T>`. Every real usage — confirmed by
grepping the static tier, not assumed — is `Required<String>`, and plain `String` does not
implement `CustomOutputType` (the same `GqlScalarValueType` split `StringList`'s own doc comment
explains), so `Required<String>` failed to compile the moment `deployment.rs` tried to derive a
struct containing one. Simplified to a concrete, non-generic `Required(pub Option<String>)` —
simpler than the generic version and exactly matches every actual use, rather than fixing the bound
for a type parameter nothing ever needed.

**One deliberate, documented scope reduction: `ApprovalRequirement.decisions`'s in-memory-preview
optimization is not ported.** Java's resolver pre-fetches the first decisions page inside
`approvalInbox` (keyed off whether the query's selection set asks for
`edges/node/requirement/decisions`, via `ctx.look_ahead()` — confirmed to exist and work identically
on the dynamic engine's `Context`, since `ResolverContext` derefs to the same `async_graphql::
Context` the static macros use) so the nested `decisions` field can skip a second round trip on the
common path. This is a pure performance optimization with no wire-visible effect: the fallback path
(a direct `approval_decisions` query) always produces the identical result, and porting the
optimization would require hand-building the entire ~17-field `ApprovalRequirement` object field by
field (to carry a schema-invisible preview value the derive macro has no way to exclude — the same
class of problem as `administration.rs`'s `FixedApprovalPolicyMatrixInput`, but for an output object
with far more fields than that 9-field input). `decisions` always takes the direct-query path;
`approvalInbox` always passes `include_decision_preview: false`. This is a real, load-bearing
behavior difference (extra query count under one specific query shape), not a defect, and is
recorded here and in the file's own doc comment rather than silently dropped.

No other new construction problems surfaced — every other pattern in this file (10 enums via
`scalars::wire_enum!`, two interfaces, a `deployment_connection_type!`/
`from_app_deployment_connection!` macro pair for the nested-`pageInfo` connection shape distinct
from `evaluation.rs`'s flat one, `Field::deprecation` for `deploymentTimeline`) reused patterns
already proven by the prior six files and worked on the first or second attempt.

## Verification

Same core methodology as every prior slice, widened per the finding above: `hive schema-sdl
--engine dynamic`'s output (dangling `schema { ... subscription: Subscription }` block stripped)
compared against `schema/contract.graphql`, but this time over *every* type the two schemas share
(215, excluding `Query`/`Mutation` and scalars) plus every root field this entire port has
implemented so far (38 queries, 36 mutations — not just this file's new ones) and all 7
interfaces' implementors across the whole port. Everything matches exactly. Full run in
`whole-schema-parity.log`.

## Gate results

All pass on this commit: `cargo fmt --check`, `cargo clippy -p hive-api --all-targets -- -D
warnings`, `cargo test -p hive-api --lib` (23 tests), `check:architecture` (36 hive-api source
files, 0 findings).

## What's next

`GSR-PHASE-2` (porting every static schema file onto the dynamic tier) is complete. Remaining per
`docs/graphql-seaography-rewrite-plan.md`: `GSR-PHASE-3` (flip the default engine to dynamic,
regenerate `schema/hive.graphql` from it — this is also where the `Subscription`-root SDL-printing
defect and any other risk-table item marked "before phase 3" must be resolved first), `GSR-PHASE-4`
(delete the static schema tree, `HIVE_GRAPHQL_ENGINE`, and the `--engine` switch), then
`GSR-PHASE-P5` through `P8` (the persistence rewrite onto SeaORM, by far the largest remaining body
of work in the whole plan).
