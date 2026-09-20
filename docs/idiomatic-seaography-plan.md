# Idiomatic Seaography and SeaORM rewrite

Status: in progress (started September 20, 2026). Supersedes
[`graphql-seaography-rewrite-plan.md`](./graphql-seaography-rewrite-plan.md), which was marked
complete but did not deliver its goal: it kept 363 hand-written SQL statements behind
`Statement::from_sql_and_values`, hand-built the GraphQL tier on async-graphql's dynamic API, and
registered one generated entity that the console never calls.

## Goal

1. Every GraphQL read is a standard Seaography generated entity query (connection, `filters`,
   `orderBy`, `pagination`, relation fields, dataloaders) over SeaORM entities.
2. Every database access goes through SeaORM entities and sea-query builders. No SQL text.
3. The console contract is **not** frozen. The console, the harness scripts and the Rust HTTP tests
   are rewritten to the generated API.
4. Any deviation from standard SeaORM or Seaography needs an explicit exception from Richard,
   recorded in [`seaography-exceptions.md`](./seaography-exceptions.md). That register starts empty.
   Nobody but Richard adds to it. If something cannot be done the standard way, work on it stops
   and the exception is requested; it is never worked around or reclassified.

## Measured baseline (September 20, 2026)

| Fact | Value |
| --- | --- |
| Raw SQL execution sites in `hive-persistence` (outside `migrator/`) | 344 sites, 363 statements, 64 dynamic fragments |
| Statements by module | deployment 150, evaluation 64, administration 55, configuration 27, agent 25, capability 24, console 6, organization 6, worker_health 3, audit 2, project 1 |
| Entity modules / with a relation / with an ActiveEnum | 82 / 0 / 0 |
| Entities registered with Seaography | 1 (`organization_read`, no relations, unused by the console) |
| Hand-built resolvers in `hive-api/src/schema` | 41 `Field::new`, 21 `#[CustomFields]` blocks, 174 `CustomOutputType`, 7 hand-built interfaces |
| Console operations | 74 (42 queries, 32 mutations), ~3,400 lines of cynic code |
| Harness scripts sending their own GraphQL | ~20 scripts, ~330 document literals; `http_integration.rs` 3,074 lines |

Versions: `seaography 2.0.0-rc.9`, `sea-orm 2.0.3`, `sea-query 1.0.2` are the latest published.

## Architecture

**A1. Entities are the source of truth.** Every entity module gets real `Relation` variants
(`belongs_to` / `has_many` / `has_one`, declared by column because the schema has no foreign keys),
`Related` impls, and a `RelatedEntity` enum so Seaography emits relation fields and dataloaders.
Every status, kind and other closed-set text column becomes a string-backed `DeriveActiveEnum`,
which types it in the ORM. Found in phase 0: Seaography maps only native database enum columns to
GraphQL enums, and these columns are `TEXT` with a `CHECK` (Aurora DSQL has no enum types), so
the generated API exposes them as `String` with string filters. That is the standard output, not a
workaround. Codegen mistakes (for example `unique` on `projects.organization_id`)
are fixed. The duplicate `organization_read` module is deleted; the real entities are registered.

**A2. Reads are generated.** `hive-api` registers entities with `seaography::register_entity!` and
active enums with `register_enumeration`. The console reads through the generated connection
fields using Seaography's own `filters`, `orderBy` and `pagination` (cursor, page, offset) inputs
and its `pageInfo` / `paginationInfo` outputs. The five hand-built pagination styles, the
hand-built Connection/Edge types, the `*Read` naming override and the per-domain query resolvers
are deleted. The four projection views (`project_dashboard_projection`,
`agent_operational_view_projection`, `audit_event_projection`,
`effective_evaluation_capabilities`) are already entities and are exposed the same way.

**A3. Authorization uses Seaography lifecycle hooks.** The `/graphql` handler loads the
principal's authority once per request through the ORM (memberships, roles, platform role) into
request data. `LifecycleHooksInterface::entity_filter` turns it into a `Condition` per entity
(`organization_id IN (...)`, `project_id IN (...)`, or through a relation). Unknown entities are
**denied by default**; today's hook fails open. `entity_guard` blocks every generated write and
every entity the principal has no view capability for. `field_guard` covers sensitive fields.

**A4. Values that are derived from a row are computed fields on the entity**, using Seaography's
`#[CustomFields] impl Model { async fn ...(&self, ctx) }`: for example `canUpdate` / `canPublish`
on an agent draft, `latestPublishedVersion` on an agent. No separate wire structs and no `From`
mapping layers.

**A5. Writes are commands, registered with Seaography's `register_custom_mutation`
(`#[CustomFields]`)**, implemented only with SeaORM: `Entity::find` with `lock_exclusive` /
`lock_with_tables`, `ActiveModel` insert/update, `update_many().col_expr().filter()` for guarded
updates and counters, `on_conflict`, transactions via `TransactionTrait`. Generated CRUD mutations
stay off (`mutation: false`): every write in this system needs a capability check, optimistic
concurrency, and an audit row in the same transaction, and Seaography's hooks are synchronous and
cannot carry that. Command payloads return the entity `Model` itself (`register_custom_entity`)
plus a problem list, not hand-built objects. This is the standard Seaography mechanism for
non-CRUD operations, not a deviation; it is called out here so it can be challenged.

**A6. No custom queries.** The 42 hand-built query resolvers are deleted. Where a value is not
stored and not derivable from one row (the `consoleContext` capability set, `deploymentPreview`,
`agentDraftReview`, version comparison, evaluation targets), the order of preference is: generated
reads composed in the console; a computed field on an entity; and only then a request for an
exception. None is assumed.

**A7. Persistence uses the ORM only.** Read repositories and application read services that
existed to serve GraphQL queries are deleted with their SQL. What commands and workers still need
is rewritten onto entities. Constructs sea-query cannot express (`LIMIT (subquery)`,
`jsonb_array_elements_text` laterals, `get_byte(uuid_send())` jitter, `set_config`) are
restructured: the rows are loaded under lock and the logic runs in Rust. `sql.rs` is deleted.

**A8. Migrations move to `sea-orm-migration`** as the last phase. The 5,550 lines of Flyway-style
SQL collapse into a baseline built with `SchemaManager`. View DDL has no sea-query builder, so
that one item will need an exception or a different design; it is raised when the phase starts.

## Found during execution

Facts about the standard tooling that shaped the work. None is a workaround.

- **Text enums stay strings in GraphQL** (phase 0): see A1.
- **`orderBy` ignores the order it is written in** (phase 0). Seaography applies the requested
  columns in the entity's column declaration order, so `{ displayName: ASC, id: ASC }` sorts by
  `id` first. The directories therefore order by `displayName` alone, and rows that share a name
  have no guaranteed order across pages. A deterministic tie-break needs either an upstream change
  or an exception; none has been requested.
- **A generated root field collides with a same-named hand-built query** (`agentVersions`), so the
  hand-built query, its service and its SQL are deleted in the same slice that registers the entity.
- **Hooks are synchronous.** The handler loads the principal's `Authority` once per request and the
  hook turns it into a row condition. If that load fails, generated reads are refused by
  `entity_guard` and commands still run, because they report an unavailable dependency themselves.

## Definition of done (fixed; this section is not edited to make a phase pass)

`npm run check:idiomatic` (`scripts/idiomatic-gate.mjs`) fails unless all of these hold, outside
entries Richard has added to `seaography-exceptions.md`:

| Gate | Rule |
| --- | --- |
| G1 | 0 occurrences in `crates/*/src` and `crates/*/tests` of `from_sql_and_values`, `Statement::from_string`, `execute_unprepared`, `query_one_raw`, `query_all_raw`, `execute_raw`, `Expr::cust`, `cust_with_values`, `cust_with_exprs`, `SimpleExpr::Custom`, `raw_sql!`, `from_raw_sql`, `sqlx::query` |
| G2 | 0 occurrences of `Func::cust` not listed in the register |
| G3 | 0 occurrences in `crates/hive-api/src` of `register_custom_query`, `Field::new(`, `Object::new(`, `InputObject::new(`, `Interface::new(` |
| G4 | Every entity module is either registered with Seaography or listed in the gate's internal-only list with a reason (ledger, locks, outbox, receipts) |
| G5 | No entity with a `*_id` column pointing at another entity has an empty `Relation` enum |
| G6 | `crates/hive-persistence/src/sql.rs` does not exist; `sqlx` appears in no `Cargo.toml` |
| G7 | Every console query root selects a Seaography-generated field |
| G8 | `npm run validate:local` passes on a clean committed tree |

During the work the gate runs in report mode and prints the counts per module; each phase's exit
requires its own modules to be at zero. G1 to G8 all enforce at the end.

## Phases

Each phase is a vertical slice: entities, generated reads and hooks, commands on the ORM, console
code, harness scripts, tests. A phase ends when its checks pass on a committed tree and the gate
reports zero for that phase's modules. Counts are reported from the gate's output, not from
memory.

| # | Slice | Persistence modules (statements) | Console operations |
| --- | --- | --- | --- |
| 0 | Foundations: gate script, exceptions register, authority snapshot, default-deny hooks, one proven end-to-end path (organizations → projects → agents with relations, enum, computed field, page pagination) | organization (6), project (1) | 5 directory and overview queries |
| 1 | Entity model: relations and ActiveEnums for all 82 modules; entity tests extended to verify each relation's columns | none | none |
| 2 | Capability evaluator and console context on the ORM | capability (24), console (6) | ConsoleShell, UpdateDisplayPreferences, ProjectDashboard, AgentOperationalView |
| 3 | Agent authoring and configuration | agent (25), configuration (27) | 19 |
| 4 | Administration | administration (55) | 11 |
| 5 | Audit | audit (2) | 2 |
| 6 | Evaluation and its worker | evaluation (64) | 21 |
| 7 | Deployment, approval and their workers | deployment (150), worker_health (3) | 12 |
| 8 | Migrations onto `sea-orm-migration`; delete `sql.rs` and the superseded plan; gate enforces G1 to G8; full `validate:local` | migrator | none |

`schema/hive.graphql` stays as the generated SDL the console's cynic build reads, and
`check:schema:contract` is now "the committed SDL equals `hive schema-sdl` and parses". The frozen
`schema/contract.graphql`, the React-era `schema/console-operations/` documents and
`check:console:operations` were removed in phase 0, when the first generated field replaced a
hand-built one: the contract is no longer frozen, and the console is held to the real schema by
cynic compiling against it.

## Rules of execution

- No exception is self-granted. A blocked item stops and is reported with the exact construct,
  the file and what was tried.
- A phase is not reported done without the gate's real output and the real check output.
- No redefinition of G1 to G8. If a gate turns out to be wrong, that is reported, not edited.
- Commits carry no Claude attribution. Nothing is pushed without being asked.
