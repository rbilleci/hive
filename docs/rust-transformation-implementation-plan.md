# Hive Rust Transformation Implementation Plan

## Status: plan superseded (September 19, 2026)

The feature phases through `RTP-AUDIT` landed as planned. `RTP-FULL-GATE` and `RTP-CUTOVER` were not
executed as written: there was no differential replay against a Java server and no mirror period.
Instead the console, harness, schema contract, infrastructure, and `Dockerfile` were brought into this
repository directly, the harness was made to pass against the Rust service, and
`npm run validate:local` became the gate. The Java tree was neither edited nor archived by this work.
[The design's status section](./rust-transformation-design.md#status-standalone-repository-september-19-2026)
lists what changed and what remains open. The text below is the historical plan.

## Purpose

This plan sequences the work that [the design](./rust-transformation-design.md) specifies. Each phase carries an immutable identifier `RTP-<SLUG>`, names its entry condition, lists its deliverables, and states the gate command whose exit status decides completion. A phase is complete only when its gate passes on a clean tree and the gate output is retained under `hive-rust/evidence/<YYYY-MM-DD>-<phase-slug>/`. A later phase may start only after every phase it depends on is complete.

## Working rules

The Java tree at `/home/richard/projects/hive` is read-only for the duration of every phase except [Cutover](#cutover-phase). Every Rust artifact lives under `/home/richard/projects/hive-rust`. Every migration, seed, harness script, and the frozen Java schema copy is a byte-identical mirror validated by `scripts/check-mirrors.mjs` (`RTD-MIGRATION-MIRROR`, `RTD-HARNESS-MIRROR`); an engineer who needs a harness change makes it in `hive/scripts/` first and re-mirrors, so the two trees never diverge on test intent.

Gate evidence is a directory of plain-text command outputs, one file per gate command, plus a `manifest.txt` recording the date, the `Cargo.lock` hash, and the SHA-256 of `hive-original.zip` or, once `hive-rust` is a Git repository, the commit hash. The Java tree is not a Git repository on September 17, 2026; the bootstrap phase initializes `hive-rust` as one so later phases can record commits.

Every phase's Rust code follows the porting discipline the design fixes: application services keep their Java names and method sets; repository SQL is copied verbatim with `$n` placeholders; resolvers contain transport adaptation only; and no feature adds a field, argument, or type absent from `schema/java-composed.graphql`.

The host constraints matter for scheduling. The Java side of the differential replay and the baseline capture need a JDK 25 host; the survey host carries OpenJDK 21.0.11 and cannot build the Java tree. Every other gate runs on any host with Rust stable (`cargo 1.97.1` observed on September 17, 2026), Node 22, Docker Compose, and the Playwright Chromium the harness installs.

## Phase sequence

| Phase | Identifier | Depends on | Gate summary |
| --- | --- | --- | --- |
| Baseline capture | `RTP-BASELINE` | none | Java gate output and schema copy retained |
| Workspace bootstrap | `RTP-BOOTSTRAP` | `RTP-BASELINE` | Empty schema serves, migrator ledger-compatible, packaging probes pass |
| Contract skeleton | `RTP-CONTRACT-SKELETON` | `RTP-BOOTSTRAP` | Custom-tier schema diff empty with stub resolvers |
| Capability and console core | `RTP-CORE` | `RTP-CONTRACT-SKELETON` | Organization, project, console, dashboard checks pass |
| Administration | `RTP-ADMINISTRATION` | `RTP-CORE` | Administration checks pass |
| Agent authoring | `RTP-AGENT` | `RTP-ADMINISTRATION` | Agent overview, draft editor, authoring checks pass |
| Configuration | `RTP-CONFIGURATION` | `RTP-AGENT` | Configuration checks pass |
| Deployment and worker | `RTP-DEPLOYMENT` | `RTP-CONFIGURATION` | Deployment checks and deployment-worker harness pass |
| Approval and maintenance | `RTP-APPROVAL` | `RTP-DEPLOYMENT` | Approval checks and `/health` maintenance fields pass |
| Evaluation and worker | `RTP-EVALUATION` | `RTP-APPROVAL` | Evaluation checks, migration and rollback compatibility pass |
| Audit | `RTP-AUDIT` | `RTP-EVALUATION` | Audit checks, transaction, migration, rollback, plan checks pass |
| Full gate and replay | `RTP-FULL-GATE` | `RTP-AUDIT` | `validate:local`, `check:packaging`, `contract-replay` pass |
| Cutover | `RTP-CUTOVER` | `RTP-FULL-GATE` | Console moved, image built, Java tree archived |

The order follows the dependency graph of the Java application layer rather than the milestone order in `hive/docs/PLAN.md`. The capability evaluator (`PostgresEffectiveCapabilityEvaluator`) gates every read and mutation, so it lands first with the console core. Administration owns memberships and lifecycle revisions that agent authoring reads. Configuration owns the catalog release and reusable resources that deployment binds. Deployment owns the outbox and approval requirements; approval reconciliation reads them. Evaluation binds deployment evidence. Audit projects every earlier aggregate's events.

## Baseline capture phase

`RTP-BASELINE` establishes the artifacts every later gate compares against. On a JDK 25 host with the Compose PostgreSQL running, an engineer runs `npm run validate:local` in `hive/` once and retains its full output, then runs `npm run generate:schema:graphql-runtime` and confirms the emitted file equals the committed `hive/service/src/main/resources/graphql/organization-directory.graphql`. The gate output resolves `RTD-OPEN-LOCAL-DATABASE`: if the Java migrator fails on `postgres:16-alpine` because of `CREATE INDEX ASYNC`, the engineer records that fact, and the plan's every later Java-side step (the differential replay) runs against the database the baseline passes on.

Deliverables are `hive-rust/evidence/<date>-baseline/validate-local.txt`, `hive-rust/schema/java-composed.graphql` (the byte copy), `hive-rust/db/migration/` and `hive-rust/db/seed/` (the byte copies), and `hive-rust/scripts/` populated with the mirrored specs and access scripts (all `*.e2e.mjs`, `*-access.mjs`, `*-compatibility.mjs`, `mvp-shared-fixture.mjs`, `graphql-schema.mjs`, `runtime-graphql-schema.mjs`, `check-graphql-generated.mjs`, `entity-column-coverage.mjs`, `entity-relation-coverage.mjs`, `dsql-conformance.mjs`, `quinoa-packaging.mjs` as the source for `check:packaging`, and `local-boundary.mjs`).

The gate is `node scripts/check-mirrors.mjs` exiting 0 after the copies land, plus the retained Java gate output.

## Workspace bootstrap phase

`RTP-BOOTSTRAP` produces a running Rust server that serves the console, verifies sessions, migrates the database with the compatible ledger, and answers health, with an empty custom tier. It resolves `RTD-OPEN-SEAOGRAPHY-RC` and `RTD-CRATE-PINS`.

The work creates the Cargo workspace with the five crates the design lays out, `rust-toolchain.toml`, and `Cargo.lock`; pins `seaography 2.0.0-rc.9` and lets it select `sea-orm` and `async-graphql`, recording the resolved versions in the evidence manifest; implements `hive-persistence::connection` with `RTD-JDBC-URL-COMPAT`; implements `hive-persistence::migrator` with `RTD-LEDGER-COMPAT` and `RTD-MIGRATOR-PARITY`, including the dialect probe; generates the SeaORM entity modules from a migrated database and hand-annotates relations; implements `hive-api::auth` with `RTD-SESSION-COOKIE`; implements the axum router with `RTD-HTTP-GRAPHQL`, `RTD-HEALTH`, `RTD-LOCAL-DEV-LOGIN`, `RTD-SPA-SERVING`, and `RTD-BIND`; composes a schema that registers the three Seaography read entities with the `TenantHooks` filter and a `CoreQueries` custom type containing only `currentPrincipal` so the readiness probe has a `Query` type to hit; and rewrites `scripts/local-service.mjs` to spawn the Rust binary.

The migrator gate deserves a precise statement because it underwrites every later phase. The test creates two isolated databases; migrates one with the Java service (on the JDK 25 host, or from the baseline evidence if the host cannot run Java) and the other with `hive migrate`; and asserts that `SELECT version FROM hive_schema_migrations ORDER BY version` returns identical rows and that `information_schema.columns` and `pg_indexes` match for every table. A second test migrates one database with `hive migrate`, starts the Java service against it, and confirms the Java service accepts traffic without applying anything new, proving the reverse direction.

Gate commands: `cargo fmt --check`, `cargo clippy --workspace -D warnings`, `cargo test --workspace`, `node scripts/check-mirrors.mjs`, `npm run check:architecture`, `npm run check:schema:entity-coverage`, `npm run check:schema:entity-relations`, `npm run check:migrator-compat`, and `npm run check:packaging` restricted to the SPA, session, `/health`, and `/graphql` probes. The `check:packaging` script is `quinoa-packaging.mjs` with the JAR assertions replaced by an assertion that `target/release/hive` exists and boots from a temporary directory with `HIVE_WEB_DIST` pointing at `../hive/web/dist`.

## Contract skeleton phase

`RTP-CONTRACT-SKELETON` makes the Rust schema identical to the Java custom tier before any resolver contains logic. The work declares every custom type, input, enum, union, payload, problem, and root field from `schema/java-composed.graphql` as `#[CustomFields]`, `#[derive(CustomOutputType)]`, `#[derive(CustomInputType)]`, and `#[derive(CustomEnum)]` items in `hive-api::graphql`, grouped by the six feature bundles, with every resolver returning the `NOT_IMPLEMENTED` error. Descriptions are copied verbatim because introspection carries them.

Deliverables are the type declarations, the `schema-snapshot` subcommand, `schema/hive.graphql`, and the `check:schema:custom-tier` script: it introspects the running Rust server, normalizes per `RTD-SDL-NORMALIZATION`, restricts both schemas to the types reachable from the custom root fields by walking the type graph with graphql-js, and diffs the two prints. The script also runs the contract regular expressions from `hive/scripts/graphql-schema.mjs` against the Rust print.

Gate commands: `npm run check:schema:custom-tier` with an empty diff, `npm run check:schema:graphql-runtime`, and `npm run check:generated:graphql`, which regenerates the console client from `schema/hive.graphql` into a temporary directory and byte-compares it to `hive/web/src/generated/graphql`. An empty diff here proves `RTD-FRONTEND-FROZEN` before a single line of business logic exists, and it also verifies `RTD-GENERATED-READ-TIER` by construction, since the regenerated client ignores the Seaography tier. `RTD-OPEN-COMPLEXITY-PARITY` closes in this phase by executing `ProjectAdministrationQuery` against both servers and recording the result.

## Capability and console core phase

`RTP-CORE` ports the foundation every feature reads: `application/organization`, `application/project`, `application/console`, and the capability evaluator. Persistence work ports `PostgresEffectiveCapabilityEvaluator` (567 lines, 16 `FOR UPDATE` sites per the September 17, 2026 survey) verbatim, the four `Jpa*` read repositories as SeaORM queries with the explicit tenant condition, `PostgresConsoleRepository`, `PostgresProjectDashboardRepository`, `PostgresAgentOperationalViewRepository`, `PostgresOrganizationOverviewRepository`, `PostgresOrganizationProjectDirectoryRepository`, `PostgresProjectAgentDirectoryRepository`, `PostgresAccessibleOrganizationRepository`, and `JpaConsoleRepository.updatePreferences` as a SeaORM transaction. Resolvers `AccessibleOrganizationResolver`, `OrganizationOverviewResolver`, `OrganizationProjectDirectoryResolver`, `ProjectAgentDirectoryResolver`, `ProjectDashboardResolver`, `ConsoleContextResolver`, and `AgentOperationalViewResolver` replace their stubs. The Java unit tests under `application/organization`, `application/project`, and `application/console` port to Rust.

Gate commands: `cargo test --workspace`; `npm run check:integration:organization-access`, `check:integration:organization-overview`, `check:integration:organization-project-list`, `check:integration:project-dashboard`, `check:integration:project-agent-list`, `check:integration:console-access`, `check:integration:agent-overview`, `check:integration:ux-redesign`, `check:integration:console-usability-corrections`; and the e2e specs `organization-selector`, `organization-overview`, `organization-project-list`, `project-dashboard`, `project-agent-list`, `agent-operational-view`, `console-context`, `console-navigation`, `ux-redesign`, and `console-usability-corrections`.

## Administration phase

`RTP-ADMINISTRATION` ports `application/administration` and `PostgresAdministrationRepository` (1,260 lines, 5 `FOR UPDATE` sites), the `AdministrationResolver`, and the ten administration mutations plus `organizationAdministration` and `projectAdministration`. The archive-event insert at `PostgresAdministrationRepository.java` line 830 is part of this phase because approval reconciliation in a later phase consumes it. The `administration-migration-compatibility.mjs` script pre-seeds `hive_schema_migrations` to simulate a partially migrated database, which exercises `RTD-LEDGER-COMPAT` under the Rust migrator.

Gate commands: `cargo test -p hive-application administration`; `npm run check:integration:administration-access`, `check:integration:administration-migration`; e2e `administration-settings` and `organization-project-authoring`.

## Agent authoring phase

`RTP-AGENT` ports `application/agent` (20 files), `PostgresAgentDraftRepository` (611 lines), `AgentDraftResolver`, and the agent version queries and mutations, including the typed problem codes `INVALID_DOCUMENT`, `INVALID_DRAFT`, and `WARNING_ACKNOWLEDGEMENT_REQUIRED` that `AgentDraftResolver.java` lines 188 through 226 map. The `LocalPromptCaseFixtureAdapter` under `integrations/evaluation` ports later with evaluation.

Gate commands: `cargo test -p hive-application agent`; `npm run check:integration:agent-draft-editor`, `check:integration:agent-authoring`; e2e `agent-draft-editor` and `agent-authoring`; and `npm run check:static:agent-authoring` re-targeted at the Rust sources.

## Configuration phase

`RTP-CONFIGURATION` ports `application/configuration`, `PostgresConfigurationRepository` (797 lines, including the `transaction(Command, Command onConflict)` helper at line 701 whose SQLSTATE `40001` handling becomes `RTD-SQLSTATE-MAPPING`), `ConfigurationGraphql`, and the seven configuration mutations plus five queries.

Gate commands: `cargo test -p hive-application configuration`; `npm run check:integration:configuration`; e2e `configuration`.

## Deployment and worker phase

`RTP-DEPLOYMENT` is the largest phase: `application/deployment` (31 files), `PostgresDeploymentRepository` (4,328 lines, 130 prepared statements), `DeploymentGraphql`, the five deployment mutations and five queries, `LocalDeploymentOutboxWorker`, `DeploymentOutboxDelivery`, and the `deployment-worker` subcommand with `RTD-WORKER-PARITY`. The repository port proceeds statement by statement in the file's own order with the Java line number recorded in a comment only where the Rust code must deviate from the verbatim SQL, so a reviewer can audit the port against the source. The `/health/deployment-worker` handler lands here.

Gate commands: `cargo test -p hive-application deployment`; `npm run check:integration:deployment`, `check:integration:deployment-harness`; e2e `deployment`; `npm run check:static:deployment` re-targeted.

## Approval and maintenance phase

`RTP-APPROVAL` ports the approval inbox and decision surface (`approvalInbox`, `approvalRequirement`, `decideDeploymentApproval`), `PostgresDeploymentApprovalEvidenceIssue`, the reconciliation methods `reconcileApprovalExpiry` and `reconcileApprovalUpgrade` with their health records, and the maintenance task with `RTD-MAINTENANCE-PARITY`. The `/health` maintenance fields become live in this phase.

Gate commands: `cargo test -p hive-application approval`; `npm run check:integration:approval`; e2e `approval`; `npm run check:static:approval` re-targeted; and the `/health` assertions of `check:packaging`.

## Evaluation and worker phase

`RTP-EVALUATION` ports `application/evaluation` (34 files), `PostgresEvaluationRepository` (1,808 lines), `PostgresEvaluationWorkStore`, `PostgresEvaluationTargetProjection`, the `LocalPromptCaseFixtureAdapter`, `EvaluationGraphql` with eight mutations and nine queries, `LocalEvaluationWorker`, and the `evaluation-worker` subcommand. The `/health/evaluation-worker` handler lands here. The `evaluation-migration-compatibility.mjs` and `evaluation-rollback` scripts exercise the migrator's `V039` repair replay and ledger behavior.

Gate commands: `cargo test -p hive-application evaluation`; `npm run check:unit:evaluation`; `npm run check:integration:evaluation`, `check:integration:evaluation-query-shape`, `check:integration:evaluation-migration`, `check:integration:evaluation-rollback`; e2e `evaluation`; `npm run check:static:evaluation` re-targeted.

## Audit phase

`RTP-AUDIT` ports `application/audit`, `domain/audit`, `PostgresAuditRepository`, `PostgresAuditRequestContext` as the tokio task-local, `AuditGraphql` with `auditEvents` and `auditEvent`, and the sensitive-field redaction that depends on `AUDIT_SENSITIVE.VIEW`. The two custom GraphQL errors `AuditGraphql.java` lines 88 and 142 build, and the `503` classification for `AuditDependencyUnavailableException`, land here.

Gate commands: `cargo test -p hive-application audit`; `npm run check:integration:audit`, `check:audit-transaction`, `check:integration:audit-migration`, `check:integration:audit-rollback`, `check:integration:audit-plan`; e2e `audit`; `npm run check:static:audit` re-targeted; `npm run check:mvp-accessibility`.

## Full gate and replay phase

`RTP-FULL-GATE` runs the complete `validate:local` set from `hive-rust/package.json`, which mirrors the check order in `hive/scripts/validate-local.mjs` with the substitutions the design's gate mapping table records, followed by `check:packaging`, `check:mvp-acceptance`, and the differential replay `RTD-DIFFERENTIAL-REPLAY`. The replay runs on the JDK 25 host: it starts the Java server and the Rust server against one isolated database migrated by `hive migrate`, executes every query document under `hive/web/src/graphql/` with the fixture variables from the access scripts as principals `…0001` and `…0002`, and diffs the JSON bodies with `X-Request-Id` removed. Any nonzero diff is a defect in the Rust tree, never a reason to change the console or the Java tree.

Gate commands: `npm run validate:local`, `npm run check:packaging`, `npm run check:mvp-acceptance`, `node scripts/contract-replay.mjs`, all retained in `evidence/<date>-full-gate/`.

## Cutover phase

`RTP-CUTOVER` is the only phase that touches the Java tree, and it does so by moving, not editing. The work moves `hive/web/` to `hive-rust/web/` unchanged, then changes the single schema path in `hive-rust/web/codegen.ts` from the Java resource to `../schema/hive.graphql` and re-runs `check:generated:graphql` to confirm a byte-identical client; sets the `HIVE_WEB_DIST` default to `web/dist`; adds the `Dockerfile` the design describes; updates the image reference in `hive/infra/aws/fargate-app` and moves `hive/infra/` to `hive-rust/infra/` unchanged otherwise; rewrites `README.md` for `hive-rust` from the Java README with the `mvnw` steps replaced by `cargo build --release` and `hive serve`; and archives the Java tree as `hive-java-<date>.zip` beside `hive-original.zip`. The mirror guard changes its source for the harness from `hive/scripts/` to the archived copy, and the migration mirror check is retired by recording the SHA-256 of every migration file in `evidence/<date>-cutover/migration-digests.txt`, which becomes the frozen reference for `RTD-SCHEMA-FROZEN`.

Gate commands: `npm run validate:local` and `npm run check:packaging` from `hive-rust/` with the moved console, plus `docker build .` producing an image whose `hive serve` answers the `check:packaging` probes.

## Effort signals

The plan publishes no duration estimate because no retained artifact supports one. The sizing figures a planner can use are the September 17, 2026 survey counts in the design's [Baseline facts](./rust-transformation-design.md#baseline-facts): 11,301 repository lines and 337 prepared statements to port verbatim, 44 root queries and 36 root mutations to declare, 78 tables to map as entities, 18 browser specs and the integration scripts to pass unchanged, and 24 Java test files to port. The deployment phase alone carries 4,328 of the repository lines; the plan places it after configuration so that the smaller phases establish the porting conventions before the largest port begins.

## Definition of done

The transformation is done when `RTP-CUTOVER` passes its gate, every `RTD-OPEN-*` decision in the design records a resolution with a dated evidence path, and a reader following `hive-rust/README.md` on a host with Rust stable, Node 22, and Docker Compose reaches `http://127.0.0.1:8080/organizations` as principal `00000000-0000-0000-0000-000000000001` without any Java installation.
