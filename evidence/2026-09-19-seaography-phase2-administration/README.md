# GSR-PHASE-2, fifth slice: `administration.rs`

Status: done. Continues `GSR-PHASE-2` after
`evidence/2026-09-19-seaography-phase2-configuration/`; covers file 7 of 13.

## What this step built

`crates/hive-api/src/graphql_schema/administration.rs`: `AdministrationPrincipal`/
`AdministrationMembership`, `ProjectBudgetPolicy`/`ProjectBudgetStatus`, `ApprovalPolicyRule` and
its version/policy wrappers, `ProjectSettingsConnection`, `OrganizationAdministration`/
`ProjectAdministration`, the fifth interface this port builds (`AdministrationProblem`, 6
implementors — tied with `configuration.rs` for the most so far), 10 input types, 2 read root
fields, and 10 mutations.

## What this step found

**A GraphQL input type whose wire field names cannot be expressed as Rust identifiers at all.**
`FixedApprovalPolicyMatrixInput`'s 9 fields are named `DEVELOPMENT_LOW`, `DEVELOPMENT_MEDIUM`, ...,
`PRODUCTION_HIGH` — fixed SCREAMING_SNAKE_CASE risk-tier keys the console's admin UI sends
literally (confirmed against the frozen contract, not assumed: `schema/contract.graphql`'s own
`FixedApprovalPolicyMatrixInput` declaration lists exactly these 9 keys). `#[derive(
CustomInputType)]` has no field-level rename hook (only the struct-level `input_type_name`
override exists — confirmed by re-reading `custom_input_type.rs`'s codegen), and no valid Rust
field identifier stringifies to `DEVELOPMENT_LOW` while still being ordinary Rust, so this type
cannot be derived at all, unlike every previous keyword-collision or list-shaped-field problem this
port has hit. Hand-built instead: a plain Rust struct (snake_case fields, since nothing about it is
wire-facing) with hand-written `CustomInputType`/`CustomInputObject` impls that replicate exactly
what `derive_custom_input_type_struct` generates for a normal struct, substituting literal string
keys for `stringify!(field_ident)` — the same general technique `audit.rs`'s
`AuditResourceReference` used on the output side for its `type` keyword field, applied here to the
input side for a different reason (keys with no valid Rust-identifier form as opposed to a single
Rust keyword collision).

Because this hand-built parser is meaningfully more complex than any other hand-built type so far
(a 9-key nested object accessor, not a 1-2 field passthrough), the SDL-shape comparison against the
frozen contract wasn't treated as sufficient proof by itself — it confirms the *declared* shape
matches, not that `parse_value` actually reads each of the 9 keys into the correct field. Added 2
new tests executing a real throwaway schema: one supplies all 9 distinct integer values and asserts
`DEVELOPMENT_LOW`'s value specifically reached `development_low.requiredApprovers` (ruling out a
copy-paste key/field mismatch among the 9), the other confirms an incomplete matrix is rejected
rather than silently defaulting a missing cell.

No other new defects surfaced — every other construction in this file (interface, `StringList` on
both `AdministrationMembership.roleCodes`-style fields and `ApprovalPolicyCellInput.
requiredEvidence`, plain `#[CustomFields]` root operations) reused patterns already proven by the
prior four files and worked on the first attempt.

## Verification

Same methodology as the prior slices: `hive schema-sdl --engine dynamic`'s output (dangling
`schema { ... subscription: Subscription }` block stripped) compared against
`schema/contract.graphql` using `describe()`/`assert.deepEqual()`. All 29 new types and 12 new root
fields (2 queries, 10 mutations) match the frozen contract exactly — including
`FixedApprovalPolicyMatrixInput`'s 9-key shape — and all 6 `AdministrationProblem` implementors are
confirmed to declare the interface. Full run in `dynamic-sdl-parity.log`.

## Gate results

All pass on this commit: `cargo fmt --check`, `cargo clippy -p hive-api --all-targets -- -D
warnings`, `cargo test -p hive-api --lib` (21 tests, including the 2 new matrix-parsing tests),
`check:architecture` (34 hive-api source files, 0 findings). `validate:local` was also run against
this commit directly: every check passed until the same benign tree-state guard trip described in
the previous slice's evidence (new files created for the next slice while it ran). That run's
`check:rust:database` step also hit a `migrate the test database: LockTimeout(30s)` failure on an
unrelated static-engine test; re-running the same check twice more reproduced the identical
`LockTimeout(30s)` message on two different, unrelated tests, then passed cleanly on a third
attempt — confirming the documented pre-existing migration-lock race under 65 concurrent tests
(project memory), not a regression from this commit.
