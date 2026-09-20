# GSR-PHASE-2, second slice: `console.rs` and `audit.rs`

Status: done for this slice. Continues `GSR-PHASE-2` after
`evidence/2026-09-19-seaography-phase2-organization-project/`; covers files 3 and 4 of 13.

## What this step built

- `crates/hive-api/src/graphql_schema/console.rs`: `ConsolePrincipal`, `EffectiveCapability`,
  `ConsoleProject`, `ConsoleOrganization`, `ConsoleContext`, `DisplayPreferences`,
  `UpdateDisplayPreferencesInput`, `consoleContext`/`displayPreferences`/
  `updateDisplayPreferences`. First module needing a mutation (`register_custom_mutation`, which
  mirrors `register_custom_query` exactly) and the first real interface this port builds:
  `DisplayPreferencesProblem` (2 implementors), following the `GSR-INTERFACES` pattern the retired
  phase-0 spike proved — a hand-built `dynamic::Interface`, `Object::implement(name)` on each
  concrete type, and a container enum deriving `CustomOutputType` whose generated `CustomUnion`
  impl is left unregistered.
- `crates/hive-api/src/graphql_schema/audit.rs`: `AuditEvent`, `AuditEventConnection`/`Edge`,
  `AuditPageInfo` (Java's non-Relay 2-field page info, distinct from `seaography::PageInfo`),
  `AuditEventFilter`, `AuditResourceReference`, `auditEvents`/`auditEvent`.

## What this step found

**`#[derive(CustomOutputType)]` on a container enum resolves each variant by the *enum variant's
own identifier*, not the inner type's name.** Reading `custom_output_type.rs`'s
`derive_custom_output_type_enum_containers` directly: `variant_value = stringify!(#variant_ident)`
feeds `FieldValue::owned_any(inner).with_type(variant_value)`, the exact mechanism the engine uses
to resolve an interface-typed field to its concrete implementor. Each variant here is therefore
named identically to its inner payload type
(`DisplayPreferencesProblem::DisplayPreferencesNotFoundProblem(DisplayPreferencesNotFoundProblem)`,
not a shortened `NotFound(...)`) so the resolved name matches the concrete object actually
registered on the schema. Proven correct with 3 new runtime tests in `console.rs` executing a real
throwaway schema (interface resolution to each implementor, plus a type-specific inline fragment) —
not just an SDL-shape comparison, since that alone cannot prove
`FieldValue::owned_any(...).with_type(...)` resolves correctly at query time.

**A struct field whose GraphQL name is a Rust keyword cannot go through
`#[derive(CustomOutputType)]`/`#[derive(CustomInputType)]` at all, even as a raw identifier.**
`AuditResourceReference { type, id }`: the derive macro names every field with
`stringify!(#field_ident)`, and `stringify!(r#type)` produces the *literal string* `"r#type"`, not
`"type"` — confirmed directly with a one-line `rustc` program before writing any workaround, not
assumed. Fixed by hand-building the whole output type (`Object::new(name).field(...)` per property,
replicating exactly what `custom_output_type.rs`'s own generated code does for a plain struct)
instead of deriving it.

**A bare `Vec<String>` field panics the schema builder at boot, not at compile time.** Found
porting `AuditEvent.changedFields: Vec<String>`: `hive schema-sdl --engine dynamic` crashed with
`internal error: entered unreachable code: Vec<T> is not handled` inside
`seaography::custom::types::GqlScalarValueType::gql_type_ref`. Root cause: Seaography's blanket
`impl<T: ValueType + Into<sea_orm::Value>> GqlScalarValueType for T` (SeaORM's query-building
`ValueType`) also covers `Vec<String>` itself (SeaORM supports it as a real Postgres array column
type), and that blanket wins the field's type-ref resolution over `CustomOutputType`'s own
`impl<T: CustomOutputType> CustomOutputType for Vec<T>` — which was never actually a candidate,
since plain `String` implements `GqlScalarValueType`, not `CustomOutputType`, so the blanket's own
bound is unsatisfied for `Vec<String>` too, just not detected until `TypesMapHelper` fails to map
the resulting `ColumnType::Array` and hits `unreachable!()`. Reproduced in isolation first (a
throwaway `#[derive(CustomOutputType)] struct { items: Vec<String> }`, same panic, same message)
before writing the fix, per the project's verify-before-claiming-done discipline. Fixed with a new
`graphql_schema/scalars.rs::StringList` newtype (not a SeaORM `ValueType`, so it only ever resolves
through its own hand-written `CustomOutputType` impl), used by `AuditEvent.changedFields`, with a
new round-trip test (`string_list_prints_as_a_non_null_list_of_non_null_strings_and_resolves`).
This risk (and the keyword-field-name one above) is recorded in
`docs/graphql-seaography-rewrite-plan.md`'s risk table since either could recur in any of the
remaining 9 files — several have string-list-shaped fields (tags, scopes) and none have been
audited yet for keyword-named fields.

**`check:architecture`'s text-scan false-positived on a doc comment.** Explaining the `Vec<String>`
finding above by spelling out `sea_orm::sea_query::ValueType` tripped the architecture gate's raw
regex scan for `sea_query::` (a real rule: hive-api must not construct sea-query statements
directly), even though the match was prose, not code. Fixed by rewording the comment to avoid the
literal substring rather than weakening the gate.

## Verification

Same methodology as the previous slice (`check:schema:contract` still parses the *static* engine's
SDL until phase 3 flips the default, so it can't exercise the dynamic port directly yet): `hive
schema-sdl --engine dynamic`'s output, with the known dangling `schema { ... subscription:
Subscription }` block stripped, compared against `schema/contract.graphql` using the same
`describe()`/`assert.deepEqual()` structural comparison `scripts/schema-contract.mjs` itself uses.
All 30 ported types (14 from the first slice plus 11 console/audit types, `DisplayPreferencesProblem`
counted once) and 9 root fields (5 from the first slice plus `consoleContext`, `displayPreferences`,
`auditEvents`, `auditEvent`, `updateDisplayPreferences`) match the frozen contract exactly, and both
`DisplayPreferencesProblem` implementors are confirmed to declare the interface. Full run in
`dynamic-sdl-parity.log`.

## Gate results

All pass on this commit: `cargo fmt --check`, `cargo clippy -p hive-api --all-targets -- -D
warnings`, `cargo test -p hive-api --lib` (18 tests, including 3 new interface-resolution tests and
1 new `StringList` test), `check:architecture` (31 hive-api source files, 0 findings).
`validate:local` was also run against this commit directly: every check passed until a final
tree-state guard tripped on new untracked files created (deliberately, for the next file) during
the run — not a code defect. That same run's `check:rust:database` step hit one apparently-real
`http_integration.rs` failure (`project_administration_is_null_for_a_project_the_principal_cannot_see`,
a static-engine authorization test this slice never touches), re-run in isolation twice afterward
and passed both times (65/65), confirming a pre-existing flake in this suite's known
concurrency-sensitive behavior (documented in project memory), not a regression from this commit.
