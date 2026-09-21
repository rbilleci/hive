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
  columns in the entity's column declaration order, and its hooks cannot touch ordering. Resolved
  with standard features (Richard's decision, September 20): every generated entity declares its
  primary key last, and the console always adds `id: ASC`, so the key is the final tie-break and
  rows that share a name keep one order across pages. A unit test in `hive-api` fails if a
  registered entity's key is not last. Priority among the other columns still follows
  declaration order; only an upstream change would fix that.
- **Timestamps** (phase 0). Seaography's default writes chrono's display format
  (`2026-09-01 00:00:00 +00:00`), which is not ISO 8601. The schema sets the standard
  `TypesMapConfig::timestamp_rfc3339`, so every generated timestamp is RFC 3339.
- **A malformed id is an error, not "no row"** (phase 0). `filters: { id: { eq: "not-a-uuid" } }`
  is a Seaography type-conversion error; the hand-built queries returned `null`. The console's
  route guard never sends one.
- **A generated root field collides with a same-named hand-built query** (`agentVersions`), so the
  hand-built query, its service and its SQL are deleted in the same slice that registers the entity.
- **Computed fields on an entity work** (phase 2, proves A4). `#[CustomFields] impl Model { async fn
  capabilities(&self, ctx) }` resolves with the row as `&self`, because the generated object's
  parent value is the `Model`. The impl must be in `hive-persistence` (orphan rule);
  `register_entity!` has no slot for extra fields, so `hive-api` folds `Model::to_fields(context)`
  onto the generated `Object` in `builder.outputs`.
- **A list of scalars needs Seaography's `with-postgres-array` feature** (phase 2). It is in
  Seaography's default features, which this workspace turns off. Without it `Vec<String>` has no
  GraphQL mapping and the schema build panics with "Vec<T> is not handled". Turning it on also
  adds five unused `*ArrayFilterInput` types to the SDL.
- **A command payload returns the entity itself** (phase 3, proves A5). With Seaography's
  `strict-custom-types` feature off (this workspace), `Model`, `Option<Model>` and `Vec<Model>`
  are output types as they are (`GqlModelType` / `GqlModelHolderType`): a
  `#[derive(CustomOutputType)]` payload with an `Option<agent_drafts::Model>` field exposes the
  generated `AgentDrafts` object, relations and computed fields included. No
  `register_custom_entity` call is needed for an entity `register_entity!` already registered.
- **A computed field cannot answer `null` for a list of scalars** (phase 3). `Option<Vec<String>>`
  is typed `[String!]`, but `None` resolves to "internal: expects an array". A nullable computed
  value is therefore a `CustomOutputType` struct (`Option<AgentVersionComparison>`).
- **A column can be left out of the generated API** (phase 3). `#[seaography(ignore)]` on a Model
  field drops the column from the generated object, its filter input and its order input.
  `project_tool_connections.stdio_arguments` and `remote_url` are ignored that way, because a read
  withholds stored values that look like secret material; the computed `arguments` and
  `remoteUrl` fields expose them instead.
- **`Vec<String>` is an input field type as it is** (phase 3), with `with-postgres-array` on. The
  configuration inputs no longer use the `StringList` wrapper.
- **A `Decimal` column needs `with-decimal`** (phase 6). Seaography types a `Decimal`/`Money`
  column as GraphQL `String` with or without the feature, but its value converter only has a
  `Value::Decimal` arm under `with-decimal`; without it the column resolves to `null` against a
  `String!` field. The feature is in Seaography's default set, like `with-postgres-array`, and is
  turned on for `evaluation_metric_results.value` / `threshold`.
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

## Progress

Gate counts are `npm run check:idiomatic` output at the named commit.

| When | G1 raw SQL tokens | G3 hand-built GraphQL | G4 unregistered entities | G5 missing relations | G7 console roots not generated |
| --- | --- | --- | --- | --- | --- |
| Baseline (`037d5a0`) | 746 | 67 | 78 | 51 | 32 |
| Phase 0 closed (`955cef9`) | 719 | 60 | 74 | 47 | 25 |
| Phase 1 closed | 718 | 60 | 74 | 0 | 25 |
| Capability evaluator on the ORM | 710 | 60 | 74 | 0 | 25 |
| Console context and agent operational view generated | 695 | 59 | 71 | 0 | 22 |
| Agent authoring on the ORM and the generated API | 657 | 55 | 70 | 0 | 19 |
| Configuration on the ORM and the generated API | 604 | 51 | 62 | 0 | 16 |
| Administration on the ORM and the generated API | 494 | 43 | 53 | 0 | 14 |
| Audit on the generated API | 490 | 38 | 52 | 0 | 12 |
| Evaluation reads on the generated API | 472 | 31 | 42 | 0 | 7 |
| Evaluation commands and the worker on SeaORM | 360 | 28 | 42 | 0 | 7 |
| Deployment reads on the generated API | 352 | 24 | 34 | 0 | 5 |
| Deployment commands on SeaORM | 262 | 21 | 34 | 0 | 5 |

Phase 0 is closed: `organization` and `project` persistence modules are at 0; organizations,
projects, agents, agent versions and the project dashboard are generated reads with relations,
tenant scoping and tests; seven console operations run on the generated API. One phase 0 item is
**not** proven yet: a computed field on an entity (`#[CustomFields] impl Model`, A4). Nothing in
phase 0 needed one; it is proven in the first slice that does (capability flags, phase 2 or 3).

Phase 1 is closed: 146 `belongs_to` relations with their reverses across the entity layer, 49
string-backed active enums over 67 `CHECK (... IN ...)` columns, value lists taken from the live
catalog. Four entities have no relation (the two migration tables and the two worker heartbeat
tables). `RelatedEntity` enums are untouched; each slice fills its own when it registers an
entity. The entity coverage test now verifies, against the migrated database and with no raw SQL,
every relation's tables, columns and column types, and every enum's values against its `CHECK`, in
both directions. Not fixed: seven composite unique keys that SeaORM cannot annotate because one of
their columns also belongs to a second key.

Phase 2, first half: the capability evaluator's 23 statements are built with SeaORM and take the
same row locks (`FOR UPDATE OF ...`, `FOR KEY SHARE`, confirmed in the Postgres statement log).
`capability::deployment_view_predicate` still returns SQL text, because its only consumers are
the deployment module's raw queries; it goes with them in phase 7. Known and older than this
work: the evaluator's membership lock and administration's project lock can deadlock under the
concurrent test suite (the Postgres log shows it on September 18 with the old SQL). It makes
`check:rust:database` fail about one run in five; the lock order is unchanged here and is to be
fixed when administration is ported (phase 4).

Phase 2, second half: `consoleContext`, `displayPreferences` and `agentOperationalView` are
deleted with their services and SQL; the `console` module is at 0. `principals`,
`principal_display_preferences` and `agent_operational_view_projection` are generated reads (a
principal reads only its own row and preferences until administration widens that). The
capability set is the computed `capabilities` field on `Organizations`, `Projects` and
`Principals`, answered by the evaluator for the requesting principal; the console builds its
context and its own access fingerprint from one generated `ConsoleShell` query.
`updateDisplayPreferences` stays a command, on SeaORM (`lock_exclusive`, `on_conflict`).

Phase 3, agent authoring half: `agentDraft`, `agentDraftReview` and `compareAgentVersions` are
deleted with the application read models behind them; the `agent` module is at 0. `agent_drafts`
is a generated read, visible with its agent. Computed fields: `Agents.draft` (the stored draft, or
the default draft of an agent that has none yet; reading stores nothing), `AgentDrafts.canUpdate`,
`canPublish` and `review`, and `AgentVersions.comparison(fromVersionId)`. The four commands keep
their names and inputs and run on SeaORM with the same row locks, revision checks, audit rows and
SQLSTATE 40001 handling. Their payload is `{ agentDraft: AgentDrafts, agentVersion:
AgentVersions, problems: [Problem!]! }`; `Problem` (`schema/problem.rs`) replaces the
`AgentDraftProblem` interface and is the type later slices reuse. A version's content digest is
still taken over the draft document as Postgres writes `jsonb` as text, read with sea-query's
`cast_as`, so digests of already published versions stay comparable.

Phase 3, configuration half: `catalogRelease`, `reusableResources`, `reusableResource`,
`projectMcpServers` and the hand-built `projectToolConnections` are deleted with the application
read models behind them, the `ConfigurationProblem` interface and its six types; the
`configuration` module is at 0. Generated reads: `catalog_projection_heads`, `catalog_releases`,
`catalog_definitions`, `catalog_environments`, `reusable_resources`, `reusable_resource_drafts`,
`reusable_resource_versions` and `project_tool_connections` (the MCP server rows). Project rows
follow the project like `agents`; drafts and versions follow their resource. The catalog has no
owner, so it is visible to a principal who holds `CATALOG.VIEW` anywhere (an active member of any
organization, or a platform administrator); the console reads the organization next to it and
treats an invisible organization as "unavailable". The current release is the plain filter
`catalogProjectionHeads(filters: { id: { eq: "local" } })` and its `catalogReleases` relation.
Computed fields: `ReusableResources.draft` (the draft at `currentDraftRevision`) and
`dependentResources`; `ProjectToolConnections.arguments`, `remoteUrl`, `status` and
`dependentResources`. JSON list columns are exposed as Seaography `Json`. The seven commands keep
their names and inputs and run on SeaORM with the same row locks, capability checks under lock,
audit rows and SQLSTATE 40001 handling; the revision is now also in the `WHERE` clause of every
guarded update. Their payload is `{ resource: ReusableResources, mcpServer:
ProjectToolConnections, tool: ProjectToolConnections, problems: [Problem!]! }`. Reverse
dependency and environment membership tests are `jsonb` containment through sea-query's
`PgExpr::contains`. Agent authoring now resolves references through the same configuration
helpers. Changed on the wire: `tool.rotationSummary` is the stored column, where the hand-built
type answered a fixed sentence.

Phase 4, administration: `organizationAdministration` and `projectAdministration` are deleted
with the application read models behind them, the `AdministrationProblem` interface and its six
types, and the hand-built `FixedApprovalPolicyMatrixInput`; the `administration` module is at 0.
Generated reads: `organization_memberships`, `organization_membership_roles`,
`project_memberships`, `project_membership_roles`, `project_budget_policies`,
`project_budget_policy_versions`, `project_approval_policies`,
`project_approval_policy_versions` and `project_settings_connections`. Tenant rules follow the
evaluator's view capabilities, not plain membership: organization memberships and roles need
`ORGANIZATION_MEMBERSHIP.VIEW`; project memberships, roles and the budget policy need
`PROJECT_MEMBERSHIP.VIEW` / `PROJECT_BUDGET.VIEW`; the approval policy needs
`PROJECT_APPROVAL_POLICY.VIEW`; settings connections follow the project. A principal row is
visible to itself and to whoever may view a membership it holds. This is narrower than the deleted
queries, which answered any holder of `ORGANIZATION.VIEW` / `PROJECT.VIEW` with the member list.
Computed fields: `Organizations` / `Projects` `assignableRoles` and `availablePrincipals` (empty
without the membership view capability), memberships' `roleCodes`,
`OrganizationMemberships.projectAccessSummary`, `ProjectBudgetPolicies.currentVersion` and
`status`, `ProjectApprovalPolicies.currentVersion`, `ProjectApprovalPolicyVersions.rules`. The ten
commands keep their names and run on SeaORM, one typed path per scope instead of interpolated
table names, with the revision in the `WHERE` clause of every guarded update. Their payload is
`{ organization: Organizations, project: Projects, problems: [Problem!]! }`. The approval policy
matrix is the list input `[ApprovalPolicyRuleInput!]!` (`cell`, `requiredEvidence`,
`requiredApprovers`); a cell listed twice is `INVALID_INPUT`. The approval scope caches are still
rebuilt by one `INSERT ... SELECT MIN(GREATEST(...)) ... GROUP BY`, now built with sea-query
(`Func::min`, `Func::greatest`, `InsertStatement::select_from`); `DELETE ... USING` is a
`delete_many` with `in_subquery`; the budget status computes the UTC month start with chrono and
its rules are pure functions in `hive-application`. `set_config('hive.m14_approval_role_assignment_actor')`
is deleted: the triggers that read it were removed for Aurora DSQL and nothing in the migrated
schema or the code calls `current_setting`. Changed on the wire: `connections.agentCount` (always
0) is gone; ended memberships are listed by start, not by end; the budget status period is decided
on the service clock.

Phase 5, audit: `auditEvents` and `auditEvent` are deleted with the application audit read
service, its filter and cursor types, and the hand-built `AuditResourceReference`; the `audit`
module is at 0 (`audit/context.rs`, the request metadata every command's audit row binds, stays).
The `audit_event_projection` view is a generated read whose tenant rule grants a row to a holder
of `AUDIT.VIEW` at that row's organization or project, and to a platform administrator. Redaction
is enforced by the schema, not by a resolver: `source_ip` and `user_agent` carry
`#[seaography(ignore)]`, so they are absent from the filter and order inputs and cannot be
selected as columns, and they are re-exposed as computed fields answering `null` unless the
requesting principal holds `AUDIT_SENSITIVE.VIEW` at that row's scope, alongside the computed
`sensitiveFieldsRedacted` flag; the capability is evaluated once per request per scope through
`SensitiveAuditAccess` in the request data. `check:integration:audit-plan` still asserts the
index-backed plan, now against the SQL Seaography generates. The `/graphql` handler's 503 rule is
no longer tied to one hand-written message: a resolver error whose source is a connection or
statement `DbErr` answers 503, and that error's text is replaced before it reaches the client,
because a `DbErr` names tables and columns.

The gate's G7 regex was corrected in this phase. It terminated a console query struct at the
first `\n}`, which for a struct inside a `mod` block ran past the struct and counted the next
struct's fields as query roots. It now matches the closing brace at the struct's own
indentation. The count on the previous commit is unchanged at 14, and the correction removes
three false positives from this slice. This makes the measurement stricter, never more lenient.

Phase 6, read half: the ten evaluation queries (`evaluationDefinitions`, `evaluationDefinition`,
`evaluationDefinitionVersion(s)`, `evaluationDefinitionVersionComparison`,
`evaluationDefinitionVersionUsage`, `evaluationRuns`, `evaluationRun` with its four nested
connections, `evaluationTargets`) are deleted with their eight connection types, their wire
projections, the application read service/repository/models behind them, `evaluation/cursors.rs`
and fifteen of the module's read statements. Generated reads: `evaluation_definitions`,
`evaluation_definition_drafts`, `evaluation_definition_versions`, `evaluation_runs`,
`evaluation_case_runs`, `evaluation_metric_results`, `evaluation_artifact_metadata`,
`evaluation_audit_events`, `evaluation_target_snapshots` and `evaluation_target_projections`.
Tenant rules reproduce `capability::evaluation_capabilities`: a definition needs
`EVALUATION_DEFINITION.VIEW`, a run `EVALUATION_RUN.VIEW` (which an active project's `OPERATOR`
holds without the definition one), a candidate target row `EVALUATION_RUN.RUN` (never on an
inactive project, a platform administrator included), and drafts, versions, cases, metrics,
artifacts, audit events and target snapshots follow their definition or run.
`effective_evaluation_capabilities` is **not** the source of those sets and stays unqueried: it
reads only `project_memberships`, so it ignores the platform role, the organization roles, the
project's lifecycle status and whether the owning organization membership is still active.
Computed fields: `EvaluationDefinitions.canAuthor` / `canPublish` / `draft` / `latestVersion`,
`EvaluationDefinitionVersions.comparison(rightVersionId)`, `EvaluationRuns.durationMillis` /
`failureSummary` / `deploymentEvidenceDisposition` / `target`, `EvaluationAuditEvents.summary`,
and `Projects.compatibleEvaluationTargets(definitionVersionId)` — the candidate targets, which
are not one row's value and not a plain filter (the compatible kinds and environment classes are
parsed out of the published document), so they are a computed list on the project the deleted
query was scoped to. Redaction is enforced by the schema as it is for audit: the draft's and the
version's `canonical_document`, the draft's `diagnostics` and an evaluation audit event's `facts`,
`source_ip` and `user_agent` carry `#[seaography(ignore)]`, and `canonicalDocument` / `diagnostics`
/ `summary` come back as computed fields that answer empty without
`EVALUATION_DEFINITION.AUTHOR`. Changed on the wire: the lists page by page number, not by cursor;
the candidate targets are answered in one bounded list instead of a cursor page; a metric's `value`
and `threshold` are the `numeric` column's exact digits as text, where the deleted type cast them
to `float8`; `canonicalDocument` is the stored `jsonb` re-serialized, not Postgres's `::text`
rendering of it; and an unauthorized list is an empty connection, so the console decides
"unavailable" from the project's `capabilities` or from the parent row's absence. The eight
mutations and the outbox worker keep their raw SQL and their names; they are ported in their own
slice, so `--module evaluation` reports 112, not 0.

Phase 6 is closed, write half: the eight commands and the outbox worker run on SeaORM only and the
`evaluation` module is at 0. The commands keep their names and their input fields, take the same
row locks (the definition and its draft `FOR UPDATE`, the run `FOR UPDATE`, the project
`FOR UPDATE` for the lifecycle test, the evaluator's own scope-first locks), keep the revision and
the generation in the `WHERE` clause of every guarded write, keep their command receipts and their
SQLSTATE 40001/23505 handling, and write their audit rows through the
`evaluation_audit_events` entity with the request metadata. Their payload is `{ definition:
EvaluationDefinitions, version: EvaluationDefinitionVersions, run: EvaluationRuns, problems:
[Problem!]! }`; the `EvaluationProblem` interface and its eight concrete types are deleted, and the
eight `code` values are unchanged. The worker's heartbeat keeps its anti-flap upsert as an
`on_conflict().values()` with two `CASE` expressions over `EXCLUDED` and the boolean recovery bind.
`EvaluationRunStatus` and `EvaluationOutcomeCategory` stay registered enums with no field of their
own: they are the wire vocabulary the console's `cynic::Enum`s are checked against, so a value the
server adds or removes fails the console build. Restructured, each time inside the one transaction
that already held the locks: the definition and its draft are two locked reads instead of one
locked join; the `EXISTS (...)`/`COALESCE(MAX(...))` scalars are an entity read plus Rust; the
heartbeat's bounded aggregate is the same bounded read with the count and the oldest taken in Rust
(it was never in a transaction). Intervals that sea-query has no `Value` for are a bound text
value cast to `interval` (`CAST('30 seconds' AS interval)`). Changed on the wire: a published
version's number is the generated `versionNumber`, where the deleted payload type called it
`number`. Not in this slice and still raw SQL: `deployment::{waiting_for_evaluation,
touch_projection, automatic_approval_handoff}`, which `append_evidence` calls across the domain
boundary, and `worker_health`, both in phase 7.

Phase 7, deployment read half: `deployments`, `deploymentProjection`, the deprecated
`deploymentTimeline` and `deploymentEnvironmentDefinitionVersions` are deleted with their five
connection/edge/page types, the `Deployment` wire type and its nine nested wire structures, the
`DeploymentFilter` input, the application read models and the list/timeline/environment cursors.
Generated reads: `deployments`, `deployment_attempts`, `deployment_plan_versions`,
`deployment_plan_review_facts`, `deployment_policy_snapshots`, `deployment_runtime_health`,
`deployment_evidence_snapshots` and `environment_definition_versions`. The tenant rule reproduces
`capability::deployment_view_predicate` exactly — a deployment is visible to a platform
administrator, to an active `ORGANIZATION_ADMIN`/`AUDITOR` of the owning organization, and to any
of the five project roles while the owning organization membership is active — and everything under
a deployment follows it. `capability::deployment_view_predicate` is **not** deleted yet: the
approval inbox's keyset query and the not-yet-ported deployment mutations still use it, so it goes
with them in the write half. Environment definition versions are catalog rows with no owner, so
they follow the catalog rule (visible to an active member of any organization), which is wider than
the deleted query's `DEPLOYMENT.VIEW`-at-the-version's-project gate. The nested structures are
relations (`agents`, `agentVersions`, `environmentDefinitionVersions`,
`deploymentPolicySnapshots`, `deploymentRuntimeHealth`, `deploymentEvidenceSnapshots`) and computed
fields (`Deployments.plan` / `currentAttempt` / `rollbackTarget` / `timeline`,
`DeploymentPlanVersions.review`, `DeploymentEvidenceSnapshots.state`). `rollbackTarget` answers the
prior active deployment row itself, keeping the deleted lateral's three inner joins, so its version
number, plan digest and runtime health are that row's own relations. Every payload that used to
carry a `Deployment` — the five commands, the approval inbox item and the approval decision —
returns the generated `Deployments` object, re-read by key after the command, so one console
fragment covers the reads and the writes. Restructured in Rust: the two `LEFT JOIN LATERAL ...
LIMIT 1` subqueries are one ordered `LIMIT 1` entity query each; the correlated `jsonb_agg(...
ORDER BY ...)` with its six-way `CASE` is the evidence relation plus a computed `state` (with SQL's
own `NULL`-is-never-equal semantics kept explicitly); the `UNION ALL` timeline with its three
`CASE` vocabularies is one bounded list merged and ordered in Rust. None of them is a guard on a
write: every one is a read outside a transaction, where the deleted statement held no lock either.
Changed on the wire: lists page by page number, not by cursor; the timeline is one bounded list
(`timeline(first:)`, at most 200) instead of a cursor page, as the evaluation candidate targets
are; an unauthorized list is an empty connection, so the console decides "unavailable" from the
project's `capabilities`; a plan with no retained review facts still answers the placeholder
`changeSummary`, but every text enum (`lifecycleStatus`, `strategy`, `risk`, attempt and health
`status`, evidence `kind` and `state`) is a `String`, `requiredEvidence` is the stored `Json`, and
the approval item's `deployment.plan.canonicalPlan` is no longer withheld — the same principal
already read it through the deleted `deploymentProjection`. The approval surface, the five
mutations, the outbox worker and `worker_health` keep their SQL for the next slice, so
`--module deployment` reports 292, not 0; `rows::deployments` (the seven-table join) stays with
them because it builds the application `Deployment` those paths still use.

Phase 7, deployment command half: `deployAgentVersion`, `cancelDeployment`, `retryDeployment`,
`promoteDeployment` and `rollbackDeployment` run on SeaORM only — `mutations.rs` and `writes.rs`
are at 0 raw SQL, and so is `rows::deployments`. The commands keep their names, their input fields
and their five problem codes; the `DeploymentProblem` interface and its seven concrete types are
replaced by the shared `Problem` (`schema/problem.rs`), so a payload is `{ deployment: Deployments,
problems: [Problem!]! }`. Guarded updates carry the expected revision in the `WHERE` clause and
check `rows_affected`; counters are `col_expr(col, Expr::col(col).add(1))`; the timeline-sequence
allocator is an `on_conflict().value()` upsert read back with `exec_with_returning`; the quota
claim and the evaluation target projection are `on_conflict` upserts; the plan review facts and
the approval requirement are `on_conflict(...).do_nothing().try_insert()`; the attempt
terminalization is an `update_many(...).exec_with_returning()`; every audit row goes through the
`deployment_audit_events` `ActiveModel` with `audit::context`'s request metadata. Row locks keep
their tables and modes (`FOR UPDATE`, `FOR KEY SHARE OF version, agent, project`, `FOR SHARE OF
policy, version`, `FOR SHARE OF deployment`) through `lock_with_tables`, and the scope-row-first
order is untouched. Restructured, each time inside the transaction that already held the locks:
the policy snapshot's and the approval requirement's `INSERT ... SELECT FROM deployments` are the
same read followed by an insert by key (that transaction created the deployment row itself), and
the requirement's `requested_at + INTERVAL '24 hours'` is chrono arithmetic on the row's own
instant. `rows::deployments` — the seven-table join with two laterals and a correlated
`jsonb_agg(... CASE ...)` — is batched entity reads assembled in Rust, with the inner joins kept as
"skip the row", the rollback target as one ordered `LIMIT 1` query and the evidence state as the
same six-way decision in Rust; it is a read that held no lock before and holds none now.
`set_config('hive.m14_compiler_review')` is deleted: V025 records that the trigger reading it was
removed for Aurora DSQL, and nothing in the schema or the code calls `current_setting`. Found while
porting: `sqlx` caches a prepared statement per connection by SQL text, so two call sites that
build the *same* statement with a different literal width (`.add(1)` vs `.add(1_i64)`) make the
second one fail with "insufficient data left in message"; the deployment counters now use the
repository's existing `.add(1)` spelling. Changed on the wire: nothing but the problem type — the
codes (`FORBIDDEN`, `REVISION_CONFLICT`, `IDEMPOTENCY_CONFLICT`, `RATE_LIMITED`, `LIFECYCLE_CONFLICT`,
`NOT_FOUND`, `INVALID_INPUT`, `REASON_REQUIRED`, `CONFIRMATION_REQUIRED`, `CONFIRMATION_MISMATCH`)
and their messages are unchanged. Still raw SQL and left for the final slice: `approval.rs` (114),
`worker.rs` (50), the approval half of `queries.rs` (32), the `raw_requirement*` loaders in
`rows.rs` (6) and `worker_health`, so `--module deployment` reports 202, not 0.

### Known flaky checks (older than this work; confirmed on the base commit `standalone-repo`)

- `check:rust:database`: **fixed with the administration port (phase 4).** About 1 run in 5
  failed with Postgres "deadlock detected": an administration command evaluated its capability
  first (scope row `FOR KEY SHARE`, then the actor's memberships `FOR UPDATE`) and only then took
  the scope row `FOR UPDATE`, so two transactions could each hold a key share of the row while
  one held the membership locks the other waited for. Every administration command now locks its
  scope row `FOR UPDATE` first, then runs the evaluator, then locks the row it changes; the
  evaluator's two deployment paths take the project row `FOR KEY SHARE` before the memberships
  too, so every locked evaluation is scope row first. Ten consecutive runs passed with no
  deadlock in the Postgres log. A first series of ten had two failures of a different kind, a
  test race older than this work: `audit_events_bind_request_metadata_...` makes Ada a platform
  administrator for a moment, and the approval round trip could see it. That test now holds the
  project lock the round trips hold while the grant exists.
- `check:integration:approval`: `approval-access.mjs:1928` expects a requirement to still be
  `PENDING` while it holds an advisory lock the server stopped taking (removed for Aurora DSQL), so
  the one-second maintenance tick can expire it first. 1 of 5 runs failed on the base commit with
  the same values. To fix with the deployment port (phase 7).
- `check:e2e:deployment`: `deployment.e2e.mjs:463` occasionally gets `REVISION_CONFLICT` from
  `retryDeployment`; seen once, passed on three reruns. Deployment module untouched so far.

## Rules of execution

- No exception is self-granted. A blocked item stops and is reported with the exact construct,
  the file and what was tried.
- A phase is not reported done without the gate's real output and the real check output.
- No redefinition of G1 to G8. If a gate turns out to be wrong, that is reported, not edited.
- Commits carry no Claude attribution. Nothing is pushed without being asked.
