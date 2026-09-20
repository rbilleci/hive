# GSR-PHASE-2, first slice: `organization.rs` and `project.rs`

Status: done for this slice. `GSR-PHASE-2` continues with the remaining 11 static schema files;
this covers the first 2 of 13 (`organization.rs`, `project.rs`) plus the shared `scalars.rs`
infrastructure (`Id`/`Long`/`JSON`) built alongside them.

## What this step built

- `crates/hive-api/src/graphql_schema/scalars.rs`: hand-written `Id`/`Long`/`Json` newtypes
  implementing `CustomOutputType`/`CustomInputType` directly (Seaography's own scalar machinery
  names the JSON scalar `"Json"`, not this contract's `"JSON"`, and has no impl for
  `async_graphql::ID` or a signed 64-bit `Long`). 4 tests execute a real throwaway `dynamic::Schema`
  and assert on actual `schema.execute(...)` responses.
- `crates/hive-api/src/graphql_schema/organization.rs`: `Organization` (complex,
  `GSR-NESTED-FIELDS`: `projects` is a hand-built nested connection field folded onto the derived
  object), `OrganizationConnection`/`OrganizationEdge`, `OrganizationProjectFilter`
  (`#[derive(CustomInputType)]` with an explicit `input_type_name` override — the derive macro
  always appends "Input" to the default name otherwise), `AccessibleOrganizationsFilter` and
  `accessibleOrganizations` (both hand-built: `GSR-DEFAULTS`, `includeArchived`/`first` carry
  defaults `#[derive(CustomInputType)]`/`#[CustomFields]` cannot express), `organization(id)`.
- `crates/hive-api/src/graphql_schema/project.rs`: same pattern for `Project`
  (`agents` nested field), `Agent`, `AgentDirectoryFilter`, `CostSummary`, `ProjectDashboard`,
  `project(id)`, `projectDashboard(id)`.
- `crates/hive-api/src/graphql_schema/mod.rs`: wires both modules into `build()`; the phase-0
  spike's throwaway `problems.rs` interface is deleted now that real domain modules exist to prove
  the same construction paths.

## What this step found

**`#[derive(CustomInputType)]` always appends `"Input"` to the default type name.** Reading
`seaography-macros-2.0.0-rc.9/src/custom_input_type.rs` directly: `format!("{ident}Input")` runs
unconditionally when no `#[seaography(input_type_name = ...)]` is given, regardless of whether the
struct's own name already contains "Input". `OrganizationProjectFilter`/`AgentDirectoryFilter` both
needed the explicit override — `Builder::finish()` failed with
`SchemaError("Type \"OrganizationProjectFilter\" not found")` before it was added. This corrects an
earlier assumption (only "*Input"-suffixed names would need it) recorded during phase 0.

**Dead code from redundant manual filter parsing.** The first version of `projects_field`/
`agents_field` manually re-implemented `ObjectAccessor` parsing for `OrganizationProjectFilter`/
`AgentDirectoryFilter` field-by-field instead of using the derived type's own
`CustomInputType::parse_value`, leaving the derived structs' fields genuinely unread — caught by
`clippy -D warnings` (`fields ... are never read`), not a style nit. Fixed by parsing through
`OrganizationProjectFilter::parse_value(context(), Some(filter))?` (guarded the same way as before:
only called when the `filter` argument is present, so an omitted argument still resolves to "no
filter" rather than erroring) and reading `.lifecycleStatus`/`.search` off the result.

**`Required<T>` (`GSR-REQUIRED`) had no consumer yet.** It was written ahead of need in
`scalars.rs`; its only consumer in the static tier is `deployment.rs`, the last file in the port
order. Removed for now rather than suppressing `clippy`'s `dead_code` lint or leaving unused
production code in place — it will be re-added when `deployment.rs` is ported.

**Seaography's `Builder::new` hard-codes a dangling `Subscription` schema root.** Recorded as a new
risk-table row in `docs/graphql-seaography-rewrite-plan.md` rather than fixed now, since it doesn't
block phase 2 (the automated gate still parses the *static* engine's SDL until phase 3 flips the
default). `seaography-2.0.0-rc.9/src/builder.rs:82-87` sets the dynamic `SchemaBuilder`'s
`subscription_type` to `Some("Subscription")` unconditionally, but only registers the actual `type
Subscription { ... }` when at least one subscription field exists (`builder.rs:570`). Since this
schema never registers a subscription, `hive schema-sdl --engine dynamic`'s printed SDL declares
`schema { ... subscription: Subscription }` with no matching type definition —
`graphql-js`'s `buildSchema` (which `check:schema:contract` uses) rejects this as invalid SDL. Must
be fixed (strip the line, or register a placeholder type) before `GSR-PHASE-3`.

## Verification

`npm run check:schema:contract` still only parses the *static* engine's SDL (phase 3 hasn't flipped
the default), so it cannot yet exercise the dynamic port directly. To get an authoritative,
non-textual verdict on the ported types ahead of that flip, `hive schema-sdl --engine dynamic`'s
output (with the dangling `schema { ... }` block above stripped) was compared against
`schema/contract.graphql` using the exact same `describe()`/`assert.deepEqual()` methodology
`scripts/schema-contract.mjs` itself uses (order- and whitespace-insensitive structural comparison,
not raw text diff). All 14 ported types (`Organization`, `OrganizationConnection`,
`OrganizationEdge`, `Project`, `ProjectConnection`, `ProjectEdge`, `Agent`, `AgentConnection`,
`AgentEdge`, `CostSummary`, `ProjectDashboard`, `OrganizationProjectFilter`,
`AgentDirectoryFilter`, `AccessibleOrganizationsFilter`) and all 4 root query fields
(`organization`, `accessibleOrganizations`, `project`, `projectDashboard`) match the frozen
contract exactly. `dynamic-sdl-parity.log` in this directory is the full run.

## Gate results

All pass on this commit: `cargo fmt --check`, `cargo clippy -p hive-api --all-targets -- -D
warnings`, `cargo test -p hive-api --lib` (14 tests), `check:rust` (full workspace including
ignored database tests via the wrapper script), `check:architecture`, `check:schema:contract`
(static engine, 220 console-reachable types), and the full `npm run validate:local` on the
committed tree (see `validate-local.log`).
