# Hive Rust Transformation Design

## Status: standalone repository (September 19, 2026)

This repository no longer depends on the Java checkout. The console (`crates/hive-console`, a Leptos
port that replaced the React `web/` on the same day; see `docs/leptos-frontend-plan.md`), the validation
harness (`scripts/`), the schema snapshot and the frozen contract (`schema/`), the infrastructure
definitions (`infra/`), and a `Dockerfile` now live here; `HIVE_WEB_DIST` defaults to
`crates/hive-console/dist`; and
`npm run check:standalone` fails if any file reaches outside the tree. The sections below are kept as
the design record of the port. Where they disagree with this section, this section holds:

- `RTD-FRONTEND-FROZEN`, `RTD-HARNESS-MIRROR`, `RTD-MIGRATION-MIRROR`, and `RTD-DIFFERENTIAL-REPLAY`
  described a side-by-side period in which the Java tree stayed authoritative. That period is over.
  There are no mirror guards and no replay against a Java server. The console, the harness, the
  migrations, and the seeds are owned and edited here. `RTD-SDL-CUSTOM-TIER` survives as
  `npm run check:schema:contract`, which compares the runtime schema with `schema/contract.graphql`.
- `RTD-GENERATED-READ-TIER` was never built **as of this section's original writing**. It has since
  been built: [`docs/graphql-seaography-rewrite-plan.md`](./graphql-seaography-rewrite-plan.md)
  (`GSR-PHASE-0` through `GSR-PHASE-P8`, now complete) replaced the static async-graphql tier
  described below with the Seaography-composed dynamic schema (`organizationRead`/`projectRead`/
  `agentRead`), and ported every aggregate repository in `hive-persistence` from verbatim SQL
  through `sqlx` onto `sea_orm::ConnectionTrait`/`TransactionTrait` (SQL text still preserved
  verbatim, via `Statement::from_sql_and_values`, for exactly the locking/conflict-behavior reason
  the "Repositories" section below gives for keeping it verbatim — only the execution engine
  changed, not the query text or its semantics). `RTD-TENANT-HOOKS` and `RTD-ENTITY-COVERAGE` were
  also built, as that plan describes. Everywhere below that says a repository is "verbatim SQL
  through `sqlx`", that a pool is a `sqlx::PgPool`, or that `DatabaseConnection::
  get_postgres_connection_pool()` bridges the two, is superseded by the Seaography plan; `sqlx`
  left the workspace's production dependencies entirely once that plan's final phase ported the
  last repository (`audit`) — it remains only as two crates' `[dev-dependencies]`, for integration
  test fixture helpers unrelated to the repository layer.
- The harness inherited from the Java tree had never passed against the Java service (the baseline run
  stopped at the Java migrator; see [Open decisions](#open-decisions)). Bringing it up against the Rust
  service separated two kinds of failure. Stale expectations were corrected in the scripts: fixture
  names from before a seed rename, storage assertions for the triggers, rules, foreign keys, arrays, and
  pgcrypto digests that the Aurora DSQL alignment had removed, and UI locators for controls the console
  no longer has. Port defects were corrected in the Rust code: the missing `Long` scalar (every Java
  `Long` had been narrowed to `i32`), six nullability differences, porting notes leaking into the
  schema as descriptions, evaluation cursors that broke on their own delimiter and omitted Java's
  `kind`, the unscoped approval-inbox cursor, directory `totalCount` shrinking after a cursor, a
  panic in `/health/deployment-worker`, the missing approval-replay receipt, errors where Java returns
  an empty decision connection, the `UNAVAILABLE` refusal for evaluation storage failures, the `503`
  classification for deployment storage failures, the `event=deployment_recovery` log line, logging
  that defaulted to `ERROR`, and the absence of `SIGTERM` handling.
- One seed file changed: `organization-directory.sql` now sets `active_marker` on seeded active
  memberships, without which `organization_memberships_one_active` never constrained them.
- The Java-only checks (static assertions over Java sources, JPA entity coverage, Quarkus packaging,
  and compatibility runs against archived Java builds) were removed. `check:packaging` replaces the
  Quinoa probe, and the Vite dev server now proxies the service paths that Quinoa used to bridge.
- Open: Aurora DSQL IAM token authentication is not implemented, so the Fargate stack cannot start
  this image yet. `README.md` records the detail.

## Purpose

This document defines how the Hive service moves from Java 25 on Quarkus to Rust while the PostgreSQL schema, the React console, and the local validation harness stay unchanged. The Rust implementation lives in the peer directory `/home/richard/projects/hive-rust`; the Java implementation in `/home/richard/projects/hive` stays untouched until the cutover step in [the implementation plan](./rust-transformation-implementation-plan.md#cutover-phase). The scope covers the Rust runtime, the Seaography GraphQL layer, and the validation gate that proves parity. Serverless hosting, Aurora DSQL change data capture, and any schema change stay out of scope; [Scope and non-goals](#scope-and-non-goals) lists the boundary.

Every requirement in this document carries an immutable identifier of the form `RTD-<SLUG>`. The identifier stays fixed after publication; the title may change.

## Scope and non-goals

The transformation replaces every Java artifact under `hive/service/` with Rust: the GraphQL schema composition, the HTTP transport, the application services, the persistence repositories, the migrator, the two worker processes, and the in-process maintenance loop. The transformation keeps the migration and seed files, the composed GraphQL contract that the console consumes, the console workspace `hive/web/`, the Node harness under `hive/scripts/`, the Docker Compose PostgreSQL service, and the process topology (one HTTP server, one deployment worker, one evaluation worker).

The transformation does not change the deployment model. The Rust binary runs wherever the Quarkus JAR runs today; `hive/infra/aws/fargate-app` receives a new container image and nothing else. AWS Lambda packaging, Aurora DSQL change data capture, EventBridge scheduling, and Kinesis consumers stay out of scope; a later design may add them on top of the Rust codebase.

## Baseline facts

The figures in this table describe the Java tree at `/home/richard/projects/hive` as surveyed on September 17, 2026. That tree is not a Git repository (`git rev-parse` fails at the filesystem boundary); the archive `/home/richard/projects/hive-original.zip` (1,474,003 bytes, dated September 14, 2026) is the retained source. Each row names the command that recomputes the figure so a reader can detect drift.

| Figure (September 17, 2026) | Value | Recompute with |
| --- | --- | --- |
| Root `Query` fields in the composed SDL | 44, of which 6 carry the `jpa` prefix | Parse `type Query` in `hive/service/src/main/resources/graphql/organization-directory.graphql` with graphql-js `getQueryType().getFields()` |
| Root `Mutation` fields | 36 | Parse `type Mutation` in the same file |
| Console operation documents | 75 | `cat hive/web/src/graphql/*.graphql \| grep -c '^query\|^mutation'` |
| Console sources referencing the `jpa*` reads | 0 files | `grep -rli 'jpa' hive/web/src --include=*.ts --include=*.tsx --include=*.graphql \| grep -v /generated/ \| wc -l` |
| `jpa` or `UUID` references in the generated client types | 0 | `grep -c 'UUID\|jpa' hive/web/src/generated/graphql/graphql.ts` |
| Migration files | 42 (`V000` through `V040`, plus `V016_1`) | `ls hive/service/src/main/resources/db/migration/*.sql \| wc -l` |
| Seed files | 3 | `ls hive/service/src/main/resources/db/seed/*.sql \| wc -l` |
| Hibernate `@Entity` classes | 78 | `grep -l '@Entity' hive/service/src/main/java/dev/hive/persistence/entity/*.java \| wc -l` |
| Lines in `Postgres*` and `Jpa*` repositories | 11,301 | `cat hive/service/src/main/java/dev/hive/persistence/query/{Postgres,Jpa}*.java \| wc -l` |
| `prepareStatement(` call sites | 337 | `grep -o 'prepareStatement(' hive/service/src/main/java/dev/hive/persistence/query/*.java \| wc -l` |
| Browser end-to-end specs | 18 | `ls hive/scripts/*.e2e.mjs \| wc -l` |
| Java test files | 24 | `find hive/service/src/test/java -name '*.java' \| wc -l` |

Three facts from the survey shape every later decision. First, the console never calls the generated `jpa*` reads: zero console sources and zero generated client types reference them, because `hive/web/codegen.ts` uses the `client` preset, which emits types only for the operation documents under `hive/web/src/graphql/`. Second, the persistence layer is dominated by hand-written SQL: the `Postgres*` repositories hold the 337 prepared statements, while Hibernate serves only the three read entities that `GraphqlJpaSchemaComposer.java` registers, four `Jpa*` read repositories, and one write path (`JpaConsoleRepository.updatePreferences`). Third, the migrator `DatabaseMigrator.java` is a hand-coded ordered call list with a name-keyed ledger, not a directory scan, so a Rust migrator can reproduce its ledger exactly.

## Frozen contracts

### Database schema

`RTD-SCHEMA-FROZEN`: the Rust tree carries byte-identical copies of every file under `hive/service/src/main/resources/db/migration/` and `hive/service/src/main/resources/db/seed/` at `hive-rust/db/migration/` and `hive-rust/db/seed/`. The transformation adds no migration and edits no migration. The mirror guard in [Mirror guards](#mirror-guards) fails the gate when any copy diverges from its source.

`RTD-LEDGER-COMPAT`: the Rust migrator writes the same ledger rows to `hive_schema_migrations (version TEXT PRIMARY KEY, applied_at TIMESTAMPTZ)` using the same literal version strings that `DatabaseMigrator.java` lines 128 through 187 supply, including the seed markers `S001__organization_directory`, `S009__organization_project_administration`, and `S011__local_catalog_configuration` and the out-of-lexical-order `V016_1__deployment_projection_revision_preflight`. It claims the same application-level lock through `hive_schema_migration_lock (id INTEGER PRIMARY KEY, locked_at TIMESTAMPTZ)` with the same 60-second stale threshold, 200-millisecond poll, and 30-second wait timeout. A database migrated by the Java service is therefore usable by the Rust service without repair, and the reverse holds, which lets the [differential replay gate](#differential-replay) run both implementations against one database.

`RTD-MIGRATOR-PARITY`: the Rust migrator reproduces the execution order and gating of `DatabaseMigrator.migrateLocked()`: `V000` through `V014` run unconditionally on every start through the unversioned path; `V015` and later run once per ledger row; the three seeds run through the versioned path at the positions the Java list fixes (before `V015`); `repairAppliedEvaluationSchema()` re-executes `V039` unconditionally between `V039` and `V040`. It splits statements on `;` while honoring `''` escapes and `--` comments (`splitStatements`, lines 536 through 581), commits after every individual statement (lines 286 through 289), and inserts the ledger marker in its own commit (line 290). It ports the Aurora DSQL statement rewrites in `runStatement` (lines 380 through 430: `CREATE INDEX` gains `ASYNC` and loses `ASC`/`DESC`, `ADD CONSTRAINT ... CHECK` becomes `DROP CONSTRAINT IF EXISTS` plus `NOT VALID` plus `ALTER TABLE ASYNC ... VALIDATE CONSTRAINT`, and every returned `job_id` is awaited through `sys.jobs` with the 180-second timeout and 500-millisecond poll). The Java code applies these rewrites unconditionally; the Rust migrator applies them only when a dialect probe (`SELECT 1 FROM sys.jobs LIMIT 0`) succeeds, and executes the original statement text on a database where the probe fails. [Open decisions](#open-decisions) records why this is the one deliberate behavioral difference.

`RTD-SEED-PARITY`: the seeded fixture identities the harness hard-codes (principal `00000000-0000-0000-0000-000000000001`, organization `10000000-0000-0000-0000-000000000001`, project `50000000-0000-0000-0000-000000000001`, and the rest of `organization-directory.sql`) come from the mirrored seed files, so no Rust code embeds a fixture value.

### Frontend and GraphQL contract

`RTD-FRONTEND-FROZEN`: no file under `hive/web/` changes during the transformation. The Refine data provider `hive/web/src/providers/graphqlDataProvider.ts`, the operation documents under `hive/web/src/graphql/`, the generated client under `hive/web/src/generated/graphql/`, and `hive/web/codegen.ts` stay byte-identical. The Rust server serves the existing Vite build output.

`RTD-SDL-CUSTOM-TIER`: every type, field, argument, default value, enum value, input, union, interface, description, and scalar that a console operation document references is a frozen contract. The Rust schema reproduces them so that a normalized print of the Rust runtime schema, restricted to the types reachable from the 38 custom root `Query` fields and the 36 root `Mutation` fields, equals the same restriction of `hive/service/src/main/resources/graphql/organization-directory.graphql`. The gate script `check:schema:custom-tier` in [Validation architecture](#validation-architecture) computes both restrictions with graphql-js and diffs them.

`RTD-GENERATED-READ-TIER`: the six `jpa*` root fields, the `JpaOrganization`, `JpaProject`, and `JpaAgent` wrapper types, their `where`/`page`/`select` argument shapes, and the `UUID` scalar are artifacts of the graphql-jpa-query library. The Rust schema replaces them with Seaography-generated reads for the same three read entities. The generated tier is the one part of the composed schema that changes. This is safe because zero console sources and zero generated client types reference the generated tier (see [Baseline facts](#baseline-facts)); the codegen output therefore stays byte-identical even after the schema file it reads changes. The Seaography root fields are named `organizationRead`, `projectRead`, and `agentRead`, in the singular form Seaography emits for an entity module, and they must not collide with any custom root field; the composition step fails on a collision exactly as `GraphqlJpaSchemaComposer.compose` does.

`RTD-SDL-NORMALIZATION`: schema comparison never relies on a server-specific printer. Both sides are introspected over HTTP and printed with graphql-js `printSchema(lexicographicSortSchema(buildClientSchema(introspection)))`, the same normalization `hive/scripts/runtime-graphql-schema.mjs` uses today. Directive declarations such as the `@specifiedBy` re-declaration in `GraphqlSchemaFactory.java` line 752 do not survive introspection and therefore do not participate in the comparison.

`RTD-SCALAR-SERIALIZATION`: the custom scalars keep their names and wire forms. `JSON` accepts and returns any JSON value unchanged. `Long` serializes as a JSON number, never a string, because `hive/web/codegen.ts` maps `Long` to `number`. `ID` values are lowercase hyphenated UUID text.

`RTD-TIMESTAMP-FORMAT`: every timestamp field is `String!` or `String` and carries the text `java.time.OffsetDateTime.toString()` produces for a UTC value: `YYYY-MM-DDTHH:MM`, then `:SS` only when seconds or fractional seconds are nonzero, then a fractional part of exactly 3, 6, or 9 digits chosen by trailing-zero truncation only when nanoseconds are nonzero, then `Z`. The Rust crate `hive-domain` owns one formatter `java_offset_date_time_string` with fixture tests for each branch, and the differential replay confirms it against live Java output.

### HTTP transport

`RTD-HTTP-GRAPHQL`: `POST /graphql` accepts `application/json`. The body must be a JSON object with a string `query`; an optional `variables` object; and an optional `operationName` that must name an operation in the parsed document, otherwise the response is `400` with body `{"errors":[{"message":"..."}]}`. The response is `application/json` in GraphQL specification form (`data`, `errors`, `extensions`) with status `200`, or `503` when any error originates from a dependency-unavailable failure (`DeploymentUnavailableException`, `AuditDependencyUnavailableException`, or the message `Audit history is temporarily unavailable.` in `GraphqlExecutor.java` lines 152 through 163). Every response carries `X-Request-Id`. The server supports no batching, no `GET`, and no persisted queries. Depth is limited to 20 and complexity to 500, matching `GraphqlExecutor.java` lines 83 through 86.

`RTD-SESSION-COOKIE`: the server resolves the principal from cookie `sf_session` whose value is `<claim>.<signature>`, where `claim` is the principal UUID text and `signature` is lowercase hexadecimal HMAC-SHA-256 over the UTF-8 claim bytes keyed by the UTF-8 bytes of `HIVE_IDENTITY_SIGNING_KEY` exactly as `SignedCookiePrincipalVerifier.java` computes it. The split point is the last `.`; comparison is constant-time; a missing cookie, bad signature, or non-UUID claim yields `401`. HMAC stands for Hash-based Message Authentication Code.

`RTD-HEALTH`: `GET /health` returns the JSON object `DirectoryServer.java` lines 115 through 127 build (`status` in `ok`/`degraded`, `approvalMaintenance`, `approvalMaintenanceAttempted`, `approvalMaintenanceReconciled`, `approvalMaintenanceFailed`, `approvalUpgradeMaintenance`, `graphqlRequests`, `graphqlFailures`, `graphqlUnavailable`, `graphqlLastDurationNanos`, `graphqlMaxDurationNanos`, and the two conditional failure-code fields) with HTTP `200` when `status` is `ok` and `503` otherwise. `GET /health/deployment-worker` and `GET /health/evaluation-worker` return the objects lines 138 through 158 build, with `status` in `READY`/`STALE`/`DEGRADED`/`UNAVAILABLE` and HTTP `200` only for `READY`. The deployment-worker staleness threshold reads `HIVE_DEPLOYMENT_WORKER_STALE_MILLIS` with default 15000.

`RTD-LOCAL-DEV-LOGIN`: `GET /local-dev/login?principal=<uuid>&redirect=<path>` mints an `sf_session` cookie with `Path=/`, `HttpOnly`, and `SameSite=Lax`, then answers `302` to `redirect` (default `/`). It answers `404` unless `HIVE_LOCAL_AUTOLOGIN_ENABLED` is `true` and the peer address is loopback, matching `LocalDevAutoLoginResource.java` lines 56 through 87.

`RTD-SPA-SERVING`: the server serves the single-page application (SPA) build from the directory `HIVE_WEB_DIST` names. `GET /` returns `index.html`. Any path that does not begin with `/graphql`, `/health`, `/assets`, or `/local-dev` and does not resolve to a file returns `index.html` with status `200`. A path under `/assets/` that resolves to no file returns `404`. `GET /graphql` never returns `index.html`. These rules reproduce the Quinoa settings in `application.properties` lines 27 and 28 and the probes in `hive/scripts/quinoa-packaging.mjs`.

`RTD-BIND`: the server binds `127.0.0.1` on `HIVE_PORT` (default 8080) unless `HIVE_BIND_ADDRESS` overrides the host, which the container image sets to `0.0.0.0` to reproduce the `%prod.quarkus.http.host` override.

### Configuration

`RTD-ENVIRONMENT`: the Rust binaries read the same environment variables with the same defaults the Java tree reads, so `hive/scripts/local-service.mjs` and the Terraform task definition need no new variables.

| Variable | Default | Reader in the Java tree |
| --- | --- | --- |
| `HIVE_DATABASE_URL` | `jdbc:postgresql://127.0.0.1:5432/hive` | `application.properties`, both worker `main` methods |
| `HIVE_DATABASE_USER` | `hive` | same |
| `HIVE_DATABASE_PASSWORD` | `hive` | same |
| `HIVE_IDENTITY_SIGNING_KEY` | none; required | `ApplicationBeans.java` line 80 |
| `HIVE_LOCAL_AUTOLOGIN_ENABLED` | `false` | `LocalDevAutoLoginResource.java` line 50 |
| `HIVE_PORT` | `8080` | `application.properties` line 39 |
| `HIVE_DEPLOYMENT_WORKER_ID` | `local-deployment-worker` | `LocalDeploymentWorkerServer.java` |
| `HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS` | `100` | same |
| `HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS` | equals the interval | same |
| `HIVE_DEPLOYMENT_WORKER_STALE_MILLIS` | `15000` | `DirectoryServer.java` line 136 |
| `HIVE_EVALUATION_WORKER_ID` | `local-evaluation-worker` | `LocalEvaluationWorkerServer.java` |
| `HIVE_EVALUATION_WORKER_INTERVAL_MILLIS` | `100` | same |

`RTD-JDBC-URL-COMPAT`: the connection factory accepts `HIVE_DATABASE_URL` in the JDBC form `jdbc:postgresql://<host>:<port>/<database>` that the harness passes, and also in `postgres://` form. It parses host, port, and database from the JDBC form and combines them with `HIVE_DATABASE_USER` and `HIVE_DATABASE_PASSWORD`.

### Background processes

`RTD-WORKER-PARITY`: the deployment worker subcommand reproduces `LocalDeploymentWorkerServer.java`: it runs the migrator first, derives its worker identifier by appending `-<random UUID>` and truncating to 120 characters, records a heartbeat before and after each batch of at most 50 deliveries, and adapts its delay (0 after a full batch, the base interval after a partial batch, `min(base * 4, 5000)` when idle, `min(max(base, current) * 2, 30000)` after a failure). It prints `component=local-deployment-worker event=started workerId=<id>` on standard output because `hive/scripts/local-service.mjs` line 342 through 360 waits for that line. The evaluation worker subcommand reproduces `LocalEvaluationWorkerServer.java`: migrator first, batches of at most 50, failure backoff `min(max(interval, current) * 2, 5000)`, and a `READY` heartbeat in `evaluation_worker_heartbeats` because the harness waits for a row with `status='READY'` observed within 30 seconds.

`RTD-MAINTENANCE-PARITY`: the server runs one asynchronous task every second that calls the approval-expiry reconciliation and then the archive reconciliation, skips a tick while the previous tick is still running, and honors the in-repository rate gate `APPROVAL_MAINTENANCE_INTERVAL_MILLIS` (1000) that `PostgresDeploymentRepository.java` lines 336 through 339 enforce. Its health counters feed `GET /health`.

### Error mapping

`RTD-ERROR-MAPPING`: domain failures are typed payload problems, not GraphQL errors. Each mutation payload keeps its `problems: [XProblem!]!` list with `code` and `message`, and each code the Java resolvers emit (`NOT_FOUND`, `FORBIDDEN`, `REVISION_CONFLICT`, `INVALID_DOCUMENT`, `INVALID_DRAFT`, `WARNING_ACKNOWLEDGEMENT_REQUIRED`, and the others the SDL enumerates) keeps its spelling. SQLSTATE `40001` and `23505` conditions map to the same typed conflicts the Java repositories produce, and a conditional update that affects zero rows raises the same conflict the synthetic `40001` in `PostgresDeploymentRepository.java` lines 1531, 1548, and 1645 raises.

## Target architecture

### Repository layout

```text
hive-rust/
├── Cargo.toml                      workspace manifest
├── rust-toolchain.toml             pins the stable channel the plan records
├── crates/
│   ├── hive-domain/                identifiers, lifecycle enums, digests, timestamp formatter, typed errors
│   ├── hive-application/           application services and ports, one module per feature
│   ├── hive-persistence/           SeaORM entities, repositories, migrator, connection factory
│   ├── hive-api/                   Seaography schema, resolvers, axum router, auth, health, SPA
│   └── hive/                       binary: serve, deployment-worker, evaluation-worker, migrate, schema-snapshot
├── db/
│   ├── migration/                  byte-identical mirror of hive/service/src/main/resources/db/migration
│   └── seed/                       byte-identical mirror of hive/service/src/main/resources/db/seed
├── schema/
│   ├── java-composed.graphql       frozen copy of hive/service/src/main/resources/graphql/organization-directory.graphql
│   └── hive.graphql                normalized snapshot of the Rust runtime schema
├── scripts/                        Node harness: rewritten launcher plus mirrored access and e2e specs
├── package.json                    check:* scripts re-targeted at the Rust binary
├── evidence/                       dated gate outputs retained as artifacts
└── docs/
```

The console workspace is not copied during the transformation. The server reads `HIVE_WEB_DIST`, whose default is `../hive/web/dist`, so the existing `npm run --workspace web build` in `hive/` produces the assets the Rust server serves. The cutover phase moves `hive/web/` into `hive-rust/web/` unchanged except for the one-line schema path in `codegen.ts`.

### Crate dependency direction

`RTD-CRATE-DIRECTION`: Cargo enforces the inward dependency rule that `hive/scripts/architecture-conformance.mjs` enforces by regular expression today. `hive-domain` depends on no database, GraphQL, or HTTP crate. `hive-application` depends on `hive-domain` and declares every persistence and evaluator port as a trait. `hive-persistence` depends on `hive-domain` and `hive-application` (to implement the ports) and is the only crate that depends on `sea-orm` and `sqlx`. `hive-api` depends on all three and is the only crate that depends on `seaography`, `async-graphql`, and `axum`. The binary crate wires them. A gate script reads `cargo metadata` and fails when any crate gains a dependency outside this matrix.

### Technology selection

`RTD-CRATE-PINS`: the workspace pins the versions the following table records. `cargo search` produced these figures on September 17, 2026; the plan's bootstrap phase re-resolves them once and records the resolved lock file as evidence.

| Concern | Crate | Version observed on September 17, 2026 | Note |
| --- | --- | --- | --- |
| GraphQL framework | `seaography` | `2.0.0-rc.9` | Supplies `Builder`, `BuilderContext`, `LifecycleHooks`, `#[CustomFields]`, generated reads |
| Object-relational mapping (ORM) | `sea-orm` | `2.0.3` | Must satisfy the version range `seaography` declares; the SeaQL project structure page states the two versions must match |
| GraphQL engine | `async-graphql` | version `seaography 2.0.0-rc.9` depends on | Never chosen independently; `8.0.0-rc.5` on crates.io is not used unless Seaography requires it |
| HTTP | `axum` | `0.8.9` | With `tower-http` for static files and request identifiers |
| SQL for aggregate repositories | `sqlx` | the version `sea-orm 2.0.3` re-exports | Shares SeaORM's connection pool; never a second pool |
| Async runtime | `tokio` | `1.53.1` | |
| Session signature | `hmac`, `sha2` | `0.13.0`, `0.11.0` | |

The rationale for keeping `sqlx` beside SeaORM follows from the baseline: 337 prepared statements with manual transaction control, revision predicates, `FOR UPDATE` anchors, and SQLSTATE-driven conflict mapping port most faithfully as verbatim SQL text executed through `sqlx::Transaction`. Rewriting them as SeaORM query builders would change the SQL PostgreSQL executes and therefore change the locking and conflict behavior the schema governance document requires tests to prove. SeaORM owns the entity model, the four normalized read repositories, the display-preference write, and the Seaography reads, which is exactly the surface Hibernate owns in the Java tree per `hive/docs/decisions/postgresql-repository-boundaries.md`.

## GraphQL layer

### Composition

The runtime schema stays code-first with no SDL input, satisfying the architecture specification's prohibition on runtime SDL (`hive/docs/architecture-specification-v1.1.md`, "GraphQL service"). `hive-api::graphql::schema` builds it in one function:

```rust
static CONTEXT: LazyLock<BuilderContext> = LazyLock::new(|| BuilderContext {
    hooks: LifecycleHooks::new(TenantHooks),
    ..Default::default()
});

pub fn schema(db: DatabaseConnection, services: Arc<AppServices>) -> Schema {
    let mut builder = Builder::new(&CONTEXT, db.clone());
    builder.register_entity::<organization_read::Entity>(vec![], &organization_read::RELATED);
    builder.register_entity::<project_read::Entity>(vec![], &project_read::RELATED);
    builder.register_entity::<agent_read::Entity>(vec![], &agent_read::RELATED);
    builder.register_custom_query::<CoreQueries>();
    builder.register_custom_query::<AdministrationQueries>();
    // ... one registration per feature bundle, mirroring *Graphql.java
    builder.register_custom_mutation::<AgentMutations>();
    // ...
    builder
        .set_depth_limit(Some(20))
        .set_complexity_limit(Some(500))
        .schema_builder()
        .data(db)
        .data(services)
        .finish()
        .expect("schema composition")
}
```

`register_entity` registers reads only; the code never calls `register_entity_mutations`, which reproduces the Java rule that generated mutations do not exist (`hive/docs/schema-governance-v1.1.md`, "Default deny"). `register_custom_query` and `register_custom_mutation` hold the 38 custom queries and 36 mutations as `#[CustomFields]` implementations grouped by the same feature bundles the Java factory delegates to (`AdministrationGraphql`, `ConfigurationGraphql`, `DeploymentGraphql`, `EvaluationGraphql`, `AuditGraphql`, plus the core types in `GraphqlSchemaFactory`). Payload and problem types are `#[derive(CustomOutputType)]` structs; inputs are `#[derive(CustomInputType)]`; enums are `#[derive(CustomEnum)]` with the exact value spellings of the SDL.

### Tenant filtering and guards

`RTD-TENANT-HOOKS`: the Java tree enables the Hibernate filter `tenantScope` with parameter `principalId` on every request-scoped session (`DirectoryServer.java` lines 208 through 214, `persistence/entity/package-info.java`), and the four `Jpa*` repositories rely on it. The Rust tree implements `LifecycleHooksInterface` for a `TenantHooks` type whose `entity_filter` returns a SeaORM `Condition` that restricts each read entity to rows the request principal may see, using an `in_subquery` predicate over active organization memberships with the principal identifier taken from `ResolverContext` data. The predicate reproduces the filter condition each `@Filter` annotation on `OrganizationReadEntity`, `ProjectReadEntity`, and `AgentReadEntity` declares. `entity_guard` denies `Create`, `Update`, and `Delete` for every entity as defense in depth, and `field_guard` protects the fields the SDL marks sensitive. The hooks are defense in depth only; the capability evaluator port remains the authorization authority for every custom operation, matching the architecture specification's rule that filters never replace capability authorization.

### Scalars and problems

`Long` is a custom scalar over `i64` that serializes as a JSON number. `JSON` wraps `serde_json::Value` under the schema name `JSON`. Timestamps are plain `String` fields produced by the `hive-domain` formatter. Problem lists are non-null lists of non-null problem objects with `code: String!` and `message: String!`, the shape `GraphqlSchemaFactory.java` lines 481 through 486 define.

### Snapshot

The `schema-snapshot` subcommand builds the schema with throwing stub services, the way `GraphqlSchemaSnapshot.render()` does, and prints `schema.sdl()`. That output is a convenience; the gate compares introspection output per `RTD-SDL-NORMALIZATION`.

## HTTP layer

`hive-api::http::router` assembles an axum `Router` with these routes: `POST /graphql` to the GraphQL handler; `GET /health`, `GET /health/deployment-worker`, `GET /health/evaluation-worker` to the health handlers; `GET /local-dev/login` to the login handler; and a fallback service that serves `HIVE_WEB_DIST` with the SPA rules `RTD-SPA-SERVING` defines. The GraphQL handler is hand-written rather than the `async-graphql-axum` extractor so it can enforce the JSON-object body rule, the `operationName` check, the 401 path, the 503 classification, and the `X-Request-Id` header. Request-scoped values (principal, correlation identifier, audit request metadata) travel in `async_graphql::Request::data`, and the audit metadata additionally enters a tokio `task_local!` that the persistence crate reads, replacing the thread-local `PostgresAuditRequestContext` the Java repositories consult when they bind the five audit metadata parameters (`PostgresAuditRequestContext.java` lines 29 through 45).

The GraphQL telemetry counters `RTD-HEALTH` exposes live in an `Arc<GraphqlTelemetry>` of atomic counters updated by the handler.

## Persistence layer

### Entities

`RTD-ENTITY-COVERAGE`: `hive-persistence::entity` holds one SeaORM entity module per migration-created table, generated once by `sea-orm-cli generate entity` against a migrated database and then hand-annotated with `Relation` definitions, because the migrations declare no `FOREIGN KEY` (the `dsql-conformance` scanner enforces zero `REFERENCES`) and the generator therefore discovers no relations. Three additional read modules, `organization_read`, `project_read`, and `agent_read`, map the `organizations`, `projects`, and `agents` tables with the column sets the Java read entities expose; SeaORM permits several entity modules over one table. A gate script compares `information_schema.columns` of a migrated database with the `Column` enums of every entity module in both directions, reproducing `hive/scripts/entity-column-coverage.mjs`, and a second script checks every `Relation` `from`/`to` column pair against both tables, reproducing `hive/scripts/entity-relation-coverage.mjs`. A smoke test executes `Entity::find().limit(1)` for every entity against the migrated database, which replaces Hibernate's `validate` strategy by exercising every column decode.

### Repositories

The repository modules keep the Java names without the `Postgres`/`Jpa` prefixes: `deployment`, `evaluation`, `administration`, `configuration`, `agent_draft`, `effective_capability_evaluator`, `audit`, `console`, `project_agent_directory`, `organization_project_directory`, `accessible_organization`, `agent_operational_view`, `project_dashboard`, `organization_overview`, `evaluation_work_store`, and `deployment_approval_evidence_issue`. Each aggregate repository ports its SQL statements verbatim from the Java text blocks, changing only `?` placeholders to `$n` positional parameters, and reproduces the transaction boundaries: one `sqlx::Transaction` per Java `setAutoCommit(false)` block, commit and rollback at the same points, and the same `FOR UPDATE` clauses. The four normalized read repositories and `console::update_preferences` use SeaORM with the tenant condition applied explicitly. Every connection sets `statement_timeout` and `lock_timeout` to 5000 milliseconds on acquisition, best-effort, as `PostgresConnectionFactory.open()` does.

`RTD-SQLSTATE-MAPPING`: the persistence crate maps `sqlx::Error::Database` codes `40001` and `23505` to the typed conflict results the Java repositories return, and raises the synthetic `40001` conflict when a conditional revision update reports zero affected rows.

### Connection factory

The factory parses `HIVE_DATABASE_URL` per `RTD-JDBC-URL-COMPAT`, builds one `sqlx::PgPool` with a 5-second acquire timeout and 5-second connect timeout, and wraps it as a SeaORM `DatabaseConnection` through `SqlxPostgresConnector::from_sqlx_postgres_pool`. Aggregate repositories borrow the underlying pool through `DatabaseConnection::get_postgres_connection_pool()`; there is one pool per process.

### Migrator

The `migrate` subcommand and the startup path of `serve`, `deployment-worker`, and `evaluation-worker` call the same `migrator::migrate_and_seed` that implements `RTD-LEDGER-COMPAT` and `RTD-MIGRATOR-PARITY`. The migration and seed files are embedded with `include_str!` from `hive-rust/db/` so the binary needs no file path at runtime, matching the native-image resource inclusion in `application.properties`.

## Application layer

`hive-application` holds one module per Java package under `dev.hive.application`: `administration`, `agent`, `audit`, `configuration`, `console`, `deployment`, `evaluation`, `organization`, and `project`. Each service keeps its Java name and method set so a reviewer can diff the two trees side by side. Ports are traits (`DeploymentRepository`, `EvaluationWorkStore`, `EffectiveCapabilityEvaluator`, `Clock`, and the others the services inject), and the 19 Java application tests under `hive/service/src/test/java/dev/hive/application/` port to Rust unit tests with in-memory fakes of those traits. `hive-domain` holds the `audit` and `deployment` domain modules, identifier newtypes, lifecycle enums with the SDL's spellings, digest functions, and the timestamp formatter.

## Background processes

The binary exposes `deployment-worker` and `evaluation-worker` subcommands that implement `RTD-WORKER-PARITY`, and `serve` starts the maintenance task that implements `RTD-MAINTENANCE-PARITY`. The workers share the persistence crate with the server and open their own pool from the same environment variables. No worker exposes HTTP.

## Validation architecture

### Gate mapping

The Rust tree keeps the `npm run check:*` naming so `validate:local` reads the same shape of check set. The table maps each Java-era category to its Rust equivalent.

| Java-era check | Rust equivalent | Mechanism |
| --- | --- | --- |
| `check:dsql-conformance` | unchanged script over `hive-rust/db/` plus a Rust scanner for `sqlx::query!`/raw SQL advisory-lock calls | Migration files are byte-identical, so the SQL half passes by construction |
| `check:architecture` | `check:architecture` over `cargo metadata` and source assertions | `RTD-CRATE-DIRECTION`; no SQL text outside `hive-persistence`; no `fetch("/graphql"` outside the data provider |
| `check:generated:graphql` | unchanged | Runs codegen in `hive/web` against `schema/hive.graphql`; output must stay byte-identical to `hive/web/src/generated/graphql` |
| `check:static:*` | Rust source assertions plus `cargo fmt --check` and `cargo clippy -D warnings` | The Java-source regular expressions are re-targeted at the Rust files that carry the same responsibilities |
| `check:unit:*` | `cargo test -p hive-domain -p hive-application` | One invocation runs the whole suite, as the Java `mvnw test` does |
| `check:schema:graphql` | `check:schema:custom-tier` | Introspect the Rust server, normalize, restrict to the custom tier, diff against the same restriction of `schema/java-composed.graphql`; then run the contract regular expressions from `hive/scripts/graphql-schema.mjs` |
| `check:schema:graphql-runtime` | unchanged script | Byte-compares normalized introspection against `schema/hive.graphql` |
| `check:schema:hibernate` | entity smoke test | `Entity::find().limit(1)` for every entity |
| `check:schema:entity-coverage`, `check:schema:entity-relations` | same scripts reading SeaORM modules | `RTD-ENTITY-COVERAGE` |
| `check:integration:*` | unchanged scripts | They drive GraphQL over HTTP and depend only on the launcher |
| `check:e2e:*` | unchanged specs | `RTD-HARNESS-MIRROR` |
| `check:quinoa` | `check:packaging` | Boots the release binary from a temporary directory and runs the same probes against the served `HIVE_WEB_DIST` |
| `check:local-boundary` | unchanged patterns over the Rust tree | The base-commit diff becomes a diff against the phase's recorded baseline |

### Harness reuse

`RTD-HARNESS-MIRROR`: `hive-rust/scripts/` contains byte-identical copies of every `hive/scripts/*.e2e.mjs`, `*-access.mjs`, `*-compatibility.mjs`, and shared fixture script, and a rewritten `local-service.mjs` that exports the same function names (`createIsolatedDatabase`, `startIsolatedLocalService`, `startLocalDeploymentWorker`, `startLocalEvaluationWorker`, `signFixtureSession`, `databaseEnvironment`, `reserveLocalPort`, and the rest) with the same signatures but spawns `target/release/hive serve`, `hive deployment-worker`, and `hive evaluation-worker` instead of `java -jar` and `java -cp`. The readiness probes stay identical: `POST /graphql` until `data.__typename === "Query"` with an `evaluationDefinition` key, the worker stdout line, and the `READY` heartbeat row. The mirror guard diffs every copied spec against its source.

### Differential replay

`RTD-DIFFERENTIAL-REPLAY`: a new script `scripts/contract-replay.mjs` starts the Java server and the Rust server against one isolated database that either implementation migrated (`RTD-LEDGER-COMPAT` makes this safe), executes every query document under `hive/web/src/graphql/` with the fixture variables the access scripts already use, and diffs the JSON bodies after removing `X-Request-Id`. Mutations are excluded from replay because they change state and generate identifiers; the mirrored access scripts and e2e specs cover them. This gate catches serialization drift (timestamp format, `Long` as number, enum spellings, list ordering) that a schema comparison cannot see. It requires a JDK 25 host for the Java side; the plan records that constraint.

### Mirror guards

`RTD-MIGRATION-MIRROR`: `scripts/check-mirrors.mjs` fails when any file under `hive-rust/db/migration`, `hive-rust/db/seed`, `hive-rust/schema/java-composed.graphql`, or a mirrored harness script differs byte-for-byte from its source under `hive/`. The guard runs first in `validate:local` so that a stale mirror never produces a passing gate against the wrong contract.

## Deployment shape

The `Dockerfile` at the repository root builds the release binary in a `rust` builder stage and copies it, together with a built `web/dist`, into a minimal runtime image; the entrypoint is `hive serve` with `HIVE_BIND_ADDRESS=0.0.0.0` and `HIVE_WEB_DIST=/srv/web`. The Terraform stack under `hive/infra/aws/fargate-app` changes only the image reference. This keeps the deployment model identical to the Java service; every serverless consideration stays deferred.

## Open decisions

`RTD-OPEN-LOCAL-DATABASE`, resolved September 17, 2026: a full ordered application of every `hive-rust/db/migration` and `hive-rust/db/seed` file, executed statement-by-statement through the ported Rust migrator (`crates/hive-persistence/src/migrator/`) against a fresh `postgres:16-alpine` container, completes successfully and is idempotent on replay, with zero edits to any migration or seed file. The dialect probe (`SELECT 1 FROM sys.jobs LIMIT 0`, SQLSTATE `42P01` on failure) suppresses the `CREATE INDEX ASYNC` and `ALTER TABLE ASYNC ... VALIDATE CONSTRAINT` rewrites on PostgreSQL while keeping them on real Aurora DSQL. One migrator behavior does carry over unconditionally on both dialects: the `DROP CONSTRAINT IF EXISTS <name>` that always precedes an `ADD CONSTRAINT ... CHECK` statement, which is also what makes it safe for `V015`, `V016`, and `V016_1` to each re-declare `deployments_projection_revision_check` under the ledger's per-file gating. The regression test is `crates/hive-persistence/tests/migrator_integration.rs` (`cargo test -p hive-persistence --test migrator_integration -- --ignored`), which asserts the resulting ledger holds exactly the 30 expected rows (3 seed markers plus 27 ledgered migrations) and that a second run against the same database is a no-op. The Java gate itself was never run (the working host carries OpenJDK 21.0.11; the Java build requires 25), so whether `DatabaseMigrator.java` as published would pass its own `postgres:16-alpine` target remains unverified; that no longer blocks the Rust tree, which is dialect-correct on both databases regardless of the answer.

Update, September 18, 2026: OpenJDK 25.0.4 (Temurin, via `sdk`) became available, and `npm run validate:local` ran against `hive/` for the first time (`RTP-BASELINE`, `hive-rust/evidence/2026-09-18-baseline/validate-local.txt`), answering the question the prior paragraph left open. `DatabaseMigrator.java` as published does not pass its own `postgres:16-alpine` target: `migrateAndSeed()` fails inside `V001__organization_directory.sql` with PostgreSQL error `syntax error at or near "IF"` (position 20; full stack trace retained at `hive-rust/evidence/2026-09-18-baseline/database-migrator-create-index-async-failure.txt`). The cause is `injectAsyncKeyword()`, which unconditionally rewrites every `CREATE INDEX` statement to Aurora DSQL's `CREATE INDEX ASYNC` job-returning form and executes it with `executeQuery()` (expecting a `job_id` row back) — plain PostgreSQL has no `ASYNC` keyword in its `CREATE INDEX` grammar and returns no row set from DDL, so the rewritten statement (`CREATE INDEX ASYNC IF NOT EXISTS ...`) fails to parse. `DatabaseMigrator.java` carries no dialect probe of its own (unlike the ported Rust migrator's `SELECT 1 FROM sys.jobs LIMIT 0` check), so this rewrite is unconditional. This remains true independent of the Rust tree, which the prior paragraph already established is dialect-correct on both databases; no change to the Rust migrator follows from this finding.

Reaching that result first required two fixes to `hive/scripts/local-service.mjs` (the Node harness `validate:local`'s Postgres-backed checks launch through), committed to the `hive` tree (git-initialized for this baseline capture; see that repository's own history for exact diffs) rather than to `hive-rust`: the packaged Quarkus jar these fixtures launch defaults to Quarkus's `%prod` profile absent an override, which `application.properties` routes through the real `aurora-dsql-jdbc-connector`; that connector's `connect()` unconditionally resolves cloud IAM credentials and mints a signed token as the datasource password, with no bypass, so it could not reach a local Postgres fixture at all until every fixture launch (and the Maven `package` build that precedes it, since `quarkus.datasource.db-kind` is build-time fixed) forced an unreserved Quarkus profile name instead. Six further stale assertions across `hive/scripts/*-static.mjs` (checking source and migration shapes that later Aurora DSQL-compatibility refactors had already changed — partial indexes widened to full indexes, an `executeVersionedFile` call gaining an `asyncJobs` parameter, a `BIGINT NOT NULL DEFAULT 1` column declaration split into four DSQL-compatible statements, a nullable-column partial-unique-index guarantee replaced by a fixed sentinel value) were also fixed to reach this result, all unrelated to the dialect question itself.

`check:local-boundary` (one of `validate:local`'s 26 checks) diffs against a hardcoded historical commit SHA that predates M17's audit work in the real project history; the git-initialized snapshot this baseline capture used has no such history, so this one check could not run and was not fixed, to avoid diffing against a fabricated baseline that would make the check pass without checking anything. Every check after `check:local-boundary` in `validate:local`'s sequence needs a live database and was not reached, since each depends on the same `DatabaseMigrator.java` migration this paragraph's finding blocks; `validate:local` exits on the first failing check, so `hive-rust/evidence/2026-09-18-baseline/validate-local.txt` retains output through `check:static:audit` (the eighth of the 26 checks; the sequence's ninth, `check:local-boundary`, is the retained run's final entry), with `check:unit:evaluation` and `check:schema:graphql` (the tenth and eleventh checks) additionally confirmed passing by a separate direct run captured in the same evidence directory.

`RTD-OPEN-SEAOGRAPHY-RC`: Seaography `2.0.0-rc.9` is a release candidate. The bootstrap phase pins it and records the resolved `async-graphql` and `sea-orm` versions. If the release candidate cannot compose the three read entities with the tenant hook, the fallback is to keep the custom tier on `async-graphql` directly and register no generated reads, which changes nothing the console consumes.

`RTD-OPEN-COMPLEXITY-PARITY`: graphql-java's `MaxQueryComplexityInstrumentation` and async-graphql's complexity calculator both default to one unit per field, but list multipliers differ. The bootstrap phase executes the largest console document (`ProjectAdministrationQuery` in `hive/web/src/graphql/administration.graphql`) against both servers and records both complexity outcomes; the limit stays 500 unless the recorded outcome shows a console document exceeding it on the Rust side.

## Decision summary

The transformation keeps every stored fact and every console byte fixed and changes only the process that serves them. Seaography supplies the generated-read tier and the composition mechanism; async-graphql `#[CustomFields]` implementations supply the custom tier; SeaORM supplies the entity model and the normalized reads; verbatim SQL through `sqlx` supplies the aggregate repositories; a ported migrator supplies ledger compatibility; and the mirrored Node harness plus a differential replay supplies the proof. [The implementation plan](./rust-transformation-implementation-plan.md) sequences the work.
