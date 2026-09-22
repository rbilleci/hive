# Hive

Govern the release of AI agents across your organization

## What is Hive?

Hive is a service that enables you to author, evaluate, approve, and deploy AI agents under
policy your organization controls. Teams build agents in a shared console, and the service applies
your approval rules, capability model, and audit requirements to every change they make.

## Benefits

**Control who can release an agent**

Define per-environment approval policy for each project, and let the service apply it to every
deployment decision instead of relying on a manual review step.

**Require evidence before a release**

Make a passing evaluation, a validated plan, or a published change summary a precondition of
deployment for the environments you choose.

**Scope every action to a capability**

Grant access through organization and project roles, and have the service resolve the caller's
effective capability on every read and every command.

**Record every change**

Capture an audit event for each authoring, configuration, deployment, and approval action, and
redact sensitive request metadata from readers without the capability to see it.

**Track spend against project budgets**

Set a currency, monthly limit, and warning threshold for each project, and review imported spend
against them alongside the agents that drive it.

**Configure tools without storing credentials**

Publish MCP (Model Context Protocol) server descriptors per project that hold no credential, no
header or environment value, no health result, and no executable state.

**Run on PostgreSQL or Aurora DSQL**

Deploy a single binary that serves both the GraphQL API and the console, on any
PostgreSQL-compatible database reachable with a user and password.

## How it works

An organization contains projects. A project contains agents, the configuration they reference,
the evaluation suites that judge them, and the policies that govern their release. Each stage is a
set of GraphQL mutations that `schema/hive.graphql` defines.

| Stage | What you do |
| --- | --- |
| **Author** | Edit an agent draft section by section, and receive server-side validation diagnostics against the section each one applies to |
| **Configure** | Publish reusable resources and MCP server descriptors for a project, and reference them from agents by typed reference |
| **Publish** | Publish a validated draft as an agent version |
| **Evaluate** | Run a published evaluation definition against a target and record the outcome |
| **Deploy** | Request a version into an environment, collect the approvals your policy requires, and track the deployment through its lifecycle |

Cancel, retry, promote, and roll back a deployment at any point its lifecycle allows. Deployments
and evaluations advance through worker processes you run alongside the service.

## Use cases

**Compliance and auditing**

Report how an agent reached production using the approval decisions, evidence, and audit events the
service records for every release.

**Multi-team agent platforms**

Let many teams ship agents into shared environments, each project carrying its own approval policy,
budget policy, and role assignments.

**Release management**

Promote an agent version across development, staging, and production environments, and roll back to
a prior active target when a release goes wrong.

**Cost management**

Review agent spend against the budget policy you set for each project, and see which agents and
deployments account for it.

## Get started

Rust stable, which `rust-toolchain.toml` selects, with the `wasm32-unknown-unknown` target and
Trunk 0.21 (`cargo install --locked trunk`) for the console. Node.js 22 and npm, for the validation
harness and the console's CodeMirror bundle. Docker, for the local PostgreSQL and for building the
image.

```sh
docker compose -f infra/local/compose.yaml up -d      # PostgreSQL 16 on 127.0.0.1:5432, database/user/password "hive"
npm ci
npm run build                                          # builds crates/hive-console/dist and target/release/hive

HIVE_IDENTITY_SIGNING_KEY=local-development-key \
HIVE_LOCAL_AUTOLOGIN_ENABLED=true \
  target/release/hive serve
```

`serve` applies every migration and seed file, then listens on `127.0.0.1:8080`. Open
<http://127.0.0.1:8080/local-dev/login?principal=00000000-0000-0000-0000-000000000001> to sign in
as the seeded administrator, Ada Lovelace. That login route answers only on loopback and only while
`HIVE_LOCAL_AUTOLOGIN_ENABLED=true`.

Deployments and evaluations progress only while their workers run, each in its own process:

```sh
target/release/hive deployment-worker
target/release/hive evaluation-worker
```

The remaining subcommands are `hive migrate`, which migrates and seeds then exits, and
`hive schema-sdl`, which prints the GraphQL schema and needs no database. Every process exits
cleanly on `SIGTERM` or `SIGINT`.

For console development, keep `hive serve` running and run `trunk serve` in `crates/hive-console`,
building the editor bundle once first with `npm run --workspace @hive/console-editor build`. Trunk
serves the console on port 5173, rebuilds and reloads on every change, and forwards `/graphql`,
`/health`, and `/local-dev` to `http://127.0.0.1:8080` as `Trunk.toml` configures, so the session
cookie stays same-origin.

## Status

Hive is a working system, not a product with customers. The full authoring-to-release lifecycle
runs locally against PostgreSQL, the console drives it, and `npm run validate:local` covers it with
unit, database, integration, packaging, and browser checks as a single gate.

One gap blocks the AWS deployment. Aurora DSQL authenticates with short-lived IAM tokens, and the
connection factory accepts only a static user and password, so the Terraform stack in
`infra/aws/fargate-app` cannot start this image yet. The Known Limitation section of this file
states what implementing it requires.

## Architecture

These constraints shape the codebase more than any framework choice, and a check enforces each one
rather than a convention.

**Aurora DSQL conformance.** The schema uses no foreign keys, triggers, functions, rules,
sequences, or advisory locks. Commands lock the row they will write with `FOR UPDATE` and rely on
the commit-time serialization failure (SQLSTATE 40001) instead of a revision check in a `WHERE`
clause. `npm run check:dsql-conformance` fails on any rejected construct.

**A generated read tier.** Entities register with Seaography, which composes queries, filters,
pagination, and relations; hand-written resolvers exist only for commands and computed fields.
`npm run check:idiomatic` fails on raw SQL, on hand-built GraphQL in the API crate, on an
unregistered entity, and on a foreign-key-like column with no declared relation.

**Inverted dependencies.** `hive-application` holds the rules and the ports; `hive-persistence`
depends on it to implement those ports, not the reverse. That crate's manifest names no database
and no transport, so a service runs in a unit test against a stub port with no PostgreSQL in reach.
`npm run check:architecture` scans the manifests, because Cargo proves a dependency graph exists
but not which way it points.

**A compiled schema contract.** `schema/hive.graphql` is committed, and `cynic` checks every
console operation against it at compile time, so a schema change that breaks the console fails the
console build rather than a browser request.

| Path | Contents |
| --- | --- |
| `crates/` | The Rust service — `hive-application` (rules and ports), `hive-persistence` (SeaORM adapters), `hive-api` (GraphQL and HTTP), `hive` (the binary) — and `hive-console`, the Leptos console compiled to WebAssembly |
| `db/` | Migrations and seed data, embedded into the binary at compile time |
| `schema/` | `hive.graphql`, the SDL the service serves and the console compiles against |
| `scripts/` | The Node validation harness: integration, end-to-end, packaging, and conformance checks |
| `infra/` | Local PostgreSQL (Docker Compose) and the AWS Terraform stacks |
| `docs/` | The design the service is built to, and the register of deviations from it |

Nothing here builds, runs, or validates against another checkout; `npm run check:standalone`
enforces that.

### Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `HIVE_DATABASE_URL` | `jdbc:postgresql://127.0.0.1:5432/hive` | Both the `jdbc:postgresql://` and `postgres://` forms are accepted |
| `HIVE_DATABASE_USER` / `HIVE_DATABASE_PASSWORD` | `hive` / `hive` | |
| `HIVE_IDENTITY_SIGNING_KEY` | none; `serve` requires it | HMAC key for the `sf_session` cookie |
| `HIVE_BIND_ADDRESS` / `HIVE_PORT` | `127.0.0.1` / `8080` | The image sets the address to `0.0.0.0` |
| `HIVE_WEB_DIST` | `crates/hive-console/dist`, relative to the working directory | The image sets `/srv/web` |
| `HIVE_LOCAL_AUTOLOGIN_ENABLED` | `false` | Enables `/local-dev/login` on loopback |
| `RUST_LOG` | `info` | Standard `tracing` filter syntax |

### Validate

`npm run validate:local` is the full gate. It requires a clean committed tree, builds once, and
runs every check in the table that follows against that build. Each check also runs alone.

| Check | Proves |
| --- | --- |
| `check:standalone` | No file reaches outside this repository |
| `check:dsql-conformance` | Migrations and persistence code avoid what Aurora DSQL rejects |
| `check:rust` | `rustfmt`, `clippy -D warnings`, and the unit tests |
| `check:rust:database` | The database-backed Rust tests, against an isolated, pre-migrated database |
| `check:architecture` | No crate depends on a database or transport crate it may not, and `hive-api` builds no sea-query statement directly |
| `check:schema:contract` | `schema/hive.graphql` equals the served SDL and is valid GraphQL |
| `check:console` | `clippy -D warnings` for `wasm32-unknown-unknown` and the console's unit tests |
| `check:idiomatic` | No raw SQL, hand-built GraphQL, unregistered entity, or missing relation |
| `check:integration:*` | GraphQL behavior per feature, each against its own database and service process |
| `check:packaging` | A copied console build serves correctly: history fallback, asset 404s, server-owned paths |
| `check:e2e:*` | Browser journeys through the built console |

The harness reaches PostgreSQL on `HIVE_POSTGRES_PORT`, default `5432`, and creates and drops one
database per check. `HIVE_BINARY` names a prebuilt binary; without it the harness builds one.
`HIVE_FIXTURE_LOG=<file>` captures every fixture process's output. The end-to-end checks use
Playwright's Chromium; when the pinned revision is absent, `HIVE_CHROMIUM_PATH` names another
Chromium executable.

After a schema change, run `npm run generate:schema` and commit the result. The console then fails
to compile wherever it no longer matches.

### How the console is served

`npm run build:console` writes `.br` and `.gz` siblings beside every compressible file, which
`scripts/precompress.mjs` produces, and the server sends the one a client accepts. Files carrying a
content fingerprint in their name are served `Cache-Control: public, max-age=31536000, immutable`;
`index.html` and anything unfingerprinted are served `no-cache`, so a release is picked up on the
next navigation. A request naming a file that does not exist answers `404`, never the `index.html`
fallback.

### Container image

```sh
docker buildx build --platform linux/arm64 -t hive-service .   # the Fargate task definition runs ARM64
```

The image holds the `hive` binary and the console build, runs as an unprivileged user, and defaults
to `hive serve` on port 8080. Run a worker from the same image by passing `deployment-worker` or
`evaluation-worker` as the command.

## Known limitation: Aurora DSQL authentication

`infra/aws/fargate-app` configures the service with a `jdbc:aws-dsql:postgresql://` URL and no
password, because Aurora DSQL authenticates with short-lived IAM tokens rather than a stored
password. The connection factory accepts only a static user and password and does not recognize
that URL form, so **the AWS stack cannot start this image until token authentication is
implemented**: generate a SigV4 DSQL auth token, use it as the password, and refresh it before
expiry for new pool connections. The migrator and every query are already DSQL-conformant, which
`check:dsql-conformance` and the migrator's dialect probe establish; only authentication is
missing.
