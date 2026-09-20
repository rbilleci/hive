# GSR-PHASE-2, third slice: `agent.rs`

Status: done. Continues `GSR-PHASE-2` after
`evidence/2026-09-19-seaography-phase2-console-audit/`; covers file 5 of 13.

## What this step built

`crates/hive-api/src/graphql_schema/agent.rs`: `AgentDraft`, `AgentVersion`,
`AgentVersionComparison`, `AgentDraftReview`, `AgentDraftDiagnostic`, `AgentOperationalView` and
its 6 summary types, the third interface this port builds (`AgentDraftProblem`, 4 implementors —
`AgentDraftNotFoundProblem`/`AgentDraftAuthorizationProblem`/`AgentDraftValidationProblem`/
`AgentDraftRevisionConflict`), 4 input types, 5 read root fields, and 4 mutations. No field or
argument in this file carries a default, so every root operation is a plain `#[CustomFields]`
method — the first ported file needing no hand-built `Field`s at all.

## What this step found

**A production schema-composition gap, not a bug in this file's own content.** `hive schema-sdl
--engine dynamic` panicked with `SchemaError("Type \"JSON\" not found")` the moment `agent.rs`
became the *first production module* to actually use `scalars::Json` (`organization.rs`/
`project.rs`/`console.rs`/`audit.rs` never needed it). Root cause: a `CustomOutputType`/
`CustomInputType` impl only supplies a scalar's *type ref* — the scalar's own `Type` still needs
registering on the `SchemaBuilder` once, the same way an `Object`/`Interface` does. `scalars.rs`'s
own unit tests already do this (`.register(Scalar::new("JSON"))`), but only for their own
throwaway schema; the production `graphql_schema::build()` never did. Fixed by registering
`Scalar::new("JSON")` in `mod.rs::build()` alongside the interface registrations, with a comment
explaining why it was missed and that `Long` will need the identical treatment the first time some
future file actually uses it (not registered speculatively now, per the same "verify by building
into a real failure" discipline the rest of this port follows).

**`clippy::enum_variant_names` fires on `AgentDraftProblem`, correctly in general but not here.**
4 variants (`AgentDraftNotFoundProblem`, `AgentDraftAuthorizationProblem`,
`AgentDraftValidationProblem`, `AgentDraftRevisionConflict`) all share the `AgentDraft` prefix —
clippy's heuristic requires 3+ variants sharing a prefix before it fires, which is why
`console.rs`'s 2-variant `DisplayPreferencesProblem` never tripped it. Here the prefix is not a
discretionary naming choice: each variant's identifier is the *exact* GraphQL type name
`#[derive(CustomOutputType)]`'s generated code resolves it to at query time
(`with_type(stringify!(variant))`), so shortening a variant to `NotFound`/`Authorization`/etc.
would silently break interface resolution rather than just look different. Suppressed with
`#[allow(clippy::enum_variant_names)]` and a comment explaining why, following the same precedent
this exact file's static predecessor already set for `#[allow(clippy::duplicated_attributes)]` on
the same interface derive.

## Verification

Same methodology as the prior two slices: `hive schema-sdl --engine dynamic`'s output (dangling
`schema { ... subscription: Subscription }` block stripped) compared against
`schema/contract.graphql` using `describe()`/`assert.deepEqual()` (order- and whitespace-
insensitive structural comparison, matching `scripts/schema-contract.mjs`'s own methodology). All
50 ported types (30 from the prior two slices plus 20 agent types) and 19 root fields (9 from the
prior slices plus 10 agent queries/mutations) match the frozen contract exactly, and all 6
interface implementors across both interfaces built so far are confirmed to declare them. Full run
in `dynamic-sdl-parity.log`.

## Gate results

All pass on this commit: `cargo fmt --check`, `cargo clippy -p hive-api --all-targets -- -D
warnings`, `cargo test -p hive-api --lib` (18 tests — no new tests added this slice; the interface
mechanism was already proven correct at runtime by `console.rs`'s 3 tests, so this slice relies on
the SDL-parity check plus that already-established proof rather than duplicating a runtime test for
a second interface using the identical mechanism), `check:architecture` (32 hive-api source files,
0 findings).
