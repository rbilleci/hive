# GSR-PHASE-2, fourth slice: `configuration.rs`

Status: done. Continues `GSR-PHASE-2` after `evidence/2026-09-19-seaography-phase2-agent/`; covers
file 6 of 13.

## What this step built

`crates/hive-api/src/graphql_schema/configuration.rs`: `CatalogDefinition`/`CatalogRelease`,
`ReusableResource`/`ReusableResourceVersion`, `McpServerConfiguration`, `ProjectToolConnection`
(reshaped from `McpServerConfiguration` via the `legacyTool()` mapping, ported verbatim), the
fourth interface this port builds (`ConfigurationProblem`, 6 implementors — the largest yet), 6
input types, 5 read root fields, and 7 mutations. No field or argument carries a default, so every
root operation is a plain `#[CustomFields]` method, like `agent.rs`.

## What this step found

**`scalars::StringList` needed a `CustomInputType` impl, not just `CustomOutputType`.** This file
is the first to put `Vec<String>` on the *input* side (`CreateProjectMcpServerInput.arguments`,
`.redactedBindings`, `.tools`, `.resources`, `.prompts`, and the same on
`UpdateProjectMcpServerInput`, plus `dependencies` on two more input types) — 6 input types across
7 fields in total. Checked whether the exact `audit.rs`/`Vec<String>`-output panic recurs on inputs
before writing any code: `GqlScalarValueType::gql_input_type_ref` shares `GqlScalarValueType::
gql_type_ref`'s default body, so yes — the same ambiguous blanket resolution and the same
`unreachable!()` would strike a derived `Vec<String>` input field identically. Added
`impl CustomInputType for StringList` to `scalars.rs` (mirroring `custom/input.rs`'s own
`impl<T: CustomInputType> CustomInputType for Vec<T>` pattern by hand, since `StringList` itself
must stay outside that blanket to dodge the SeaORM ambiguity), with 2 new round-trip tests
(`string_list_prints_as_a_non_null_list_of_non_null_strings_and_round_trips`,
`string_list_rejects_a_non_list_value`) — the existing output-only test was extended into a real
argument-echo, proving both directions through one real schema execution rather than assuming
symmetry with the output impl.

No other new defects surfaced — `configuration.rs` compiled, linted, and matched the frozen
contract on the first attempt once `StringList`'s input side existed, reusing every construction
pattern proven by the prior four files (interface, hand-rolled input-type-name overrides,
`#[CustomFields]`-only root operations).

## Verification

Same methodology as the prior slices: `hive schema-sdl --engine dynamic`'s output (dangling
`schema { ... subscription: Subscription }` block stripped) compared against
`schema/contract.graphql` using `describe()`/`assert.deepEqual()`. All 20 new types and 12 new root
fields (5 queries, 7 mutations) match the frozen contract exactly, and all 6
`ConfigurationProblem` implementors are confirmed to declare the interface. Full run in
`dynamic-sdl-parity.log`.

## Gate results

All pass on this commit: `cargo fmt --check`, `cargo clippy -p hive-api --all-targets -- -D
warnings`, `cargo test -p hive-api --lib` (19 tests, including the 2 new `StringList` input tests),
`check:architecture` (33 hive-api source files, 0 findings).
