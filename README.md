# Hive

Hive is a control plane for AI agents that treats an agent the way a regulated platform treats
any other deployable artifact: authored as a draft, validated by the server, frozen into an
immutable version, evaluated against a suite, and released into an environment only once the
approvals its project's policy demands have been recorded.

The premise is that the hard part of running agents in an organization is not invoking a model.
It is answering, months later, who changed the instructions, which version served production on a
given day, what evidence justified that release, and who signed it off. Hive stores those answers
as ordinary rows and serves them over a typed API.

The whole product lives in this repository: the Rust service, the WebAssembly console it serves,
the migrations, the schema contract, the validation harness, and the infrastructure definitions.
`npm run check:standalone` enforces that nothing reaches outside the checkout.

## What it does

An organization contains projects; a project contains agents, the configuration they reference,
the evaluation suites that judge them, and the policies that govern their release. Work moves
through the stages below, each one a set of GraphQL mutations that `schema/hive.graphql`
defines.

| Stage | What happens |
| --- | --- |
| Author | An agent draft is edited section by section, over the sections `SECTIONS` freezes in `crates/hive-application/src/agent/canonical_document.rs`. The server validates it and returns diagnostics against editor paths; no client-supplied digest or validation result is trusted |
| Configure | Reusable resources and MCP (Model Context Protocol) server descriptors are published per project and referenced by agents through typed references, so a change to a shared resource is a versioned event rather than an edit in place. The descriptors are inert: no column stores a credential value, a raw header or environment value, a health result, or executable state |
| Publish | A validated draft becomes an immutable agent version. Later edits start a new draft; the published version never changes |
| Evaluate | An evaluation definition, itself drafted and published as a version, runs against a target and lands in one of the categories `EvaluationOutcomeCategory` defines |
| Deploy | An agent version is requested into an environment under one of the strategies `DeploymentStrategy` defines, gathers the approvals its project's policy requires, and then progresses through the states `DeploymentLifecycleStatus` defines |

Deployments can be canceled, retried, rolled back to a prior active target, and promoted. A
promotion records the runtime-health generation it was taken against, so a later observation
cannot be mistaken for the one that justified it. Each transition writes an audit event.

### Approval is a server decision, not a UI affordance

The approval path is the part of Hive built to be argued with after the fact. A project's approval
policy names, per environment, how many approvers a release needs and which evidence codes must be
satisfied first, drawn from the set `EVIDENCE` freezes in
`crates/hive-application/src/administration/service.rs`.

When someone decides an approval, the server locks the requirement row, reads the facts it will
judge from that locked state, and selects an outcome in a pure function
(`crates/hive-application/src/deployment/policy.rs`) that performs no I/O and reads nothing the
browser supplied. That function refuses a requester approving their own release
(`SELF_APPROVAL_FORBIDDEN`), a repeat decision from an approver already counted
(`DUPLICATE_APPROVER`), an approver the policy does not qualify (`APPROVER_INELIGIBLE`), and a
rejection carrying no reason (`REJECTION_REASON_REQUIRED`).

Approvals are decided against a frozen cycle: a policy snapshot and a plan, compared by digest.
When a later change means those frozen facts can no longer be satisfied, reconciliation
terminalizes the cycle with an invalidation code
(`crates/hive-persistence/src/deployment/approval/reconciliation.rs`), and a decision arriving
afterward is refused with that code. A stale approval cannot quietly carry a release.

A project's approval policy may only become stronger. The check lives beside the policy's
canonical encoding and digest in `crates/hive-application/src/administration/rules.rs`, so
weakening a policy is a refused command rather than a race against an in-flight release.

### Authorization is computed, not stored

Role names are stored facts; they are not the authority. Every read and every command resolves an
effective capability for a principal on a scope through the evaluator in
`crates/hive-persistence/src/capability/`, whose allow-list is the authority the code consults.
A command re-evaluates that capability inside its own transaction, with the rows it read locked,
so authority cannot change between the check and the write.

Audit history reads through a projection over the per-domain event tables. A principal without
`AUDIT_SENSITIVE.VIEW` at an event's scope sees `sourceIp` and `userAgent` as null and a
`sensitiveFieldsRedacted` flag rather than a gap they must infer; those two columns are not
generated fields at all, so no client can select, filter, or order on them.

## Design commitments

These constraints shape this codebase more than any framework choice does. Each is enforced by a
check rather than by convention.

The service targets Aurora DSQL, so the schema uses none of what DSQL rejects: no foreign keys,
triggers, functions, rules, sequences, or advisory locks. Commands lock the row they will write
with `FOR UPDATE` and rely on the commit-time serialization failure (SQLSTATE 40001) instead of a
revision check in a `WHERE` clause. `npm run check:dsql-conformance` fails on any of the rejected
constructs. Any PostgreSQL-compatible database reachable with a user and password runs it today.

The read tier is generated. Entities are registered with Seaography, which composes the queries,
filters, pagination, and relations; hand-written resolvers exist for commands and for computed
fields. `npm run check:idiomatic` fails on raw SQL, on hand-built GraphQL in the API crate, on an
entity that is not registered, and on a foreign-key-like column with no declared relation.

The dependency direction is inverted and checked. `hive-application` holds the rules and the
ports; `hive-persistence` depends on it to implement those ports, not the reverse. That crate's
manifest names no database and no transport, so a service runs in a unit test against a stub port
with no PostgreSQL in reach. `npm run check:architecture` scans the manifests, because Cargo
proves a dependency graph exists but not which way it points.

The console is type-checked against the served schema. `schema/hive.graphql` is a committed
contract; `cynic` checks every console operation against it at compile time, so a schema change
that breaks the console fails the console build rather than a browser request.

## Repository layout

| Path | Contents |
| --- | --- |
| `crates/` | The Rust service — `hive-application` (rules and ports), `hive-persistence` (SeaORM adapters), `hive-api` (GraphQL and HTTP), `hive` (the binary) — and `hive-console`, the Leptos console compiled to WebAssembly |
| `db/` | Migrations and seed data, embedded into the binary at compile time |
| `schema/` | `hive.graphql`, the SDL the service serves and the console compiles against |
| `scripts/` | The Node validation harness: integration, end-to-end, packaging, and conformance checks |
| `infra/` | Local PostgreSQL (Docker Compose) and the AWS Terraform stacks |
| `docs/` | The design the service is built to, and the register of deviations from it |

## Prerequisites

Rust stable, which `rust-toolchain.toml` selects, with the `wasm32-unknown-unknown` target and
Trunk 0.21 (`cargo install --locked trunk`) for the console. Node.js 22 and npm, for the
validation harness and the console's CodeMirror bundle. Docker, for the local PostgreSQL and for
building the image.

## Run it locally

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
as the seeded administrator, Ada Lovelace. That login route answers only on loopback and only
while `HIVE_LOCAL_AUTOLOGIN_ENABLED=true`.

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

## Validate

`npm run validate:local` is the full gate. It requires a clean committed tree, builds once, and
runs every check below against that build. Each check also runs alone.

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

## How the console is served

`npm run build:console` writes `.br` and `.gz` siblings beside every compressible file, which
`scripts/precompress.mjs` produces, and the server sends the one a client accepts. Files carrying
a content fingerprint in their name are served `Cache-Control: public, max-age=31536000,
immutable`; `index.html` and anything unfingerprinted are served `no-cache`, so a release is
picked up on the next navigation. A request naming a file that does not exist answers `404`, never
the `index.html` fallback.

## Container image

```sh
docker buildx build --platform linux/arm64 -t hive-service .   # the Fargate task definition runs ARM64
```

The image holds the `hive` binary and the console build, runs as an unprivileged user, and
defaults to `hive serve` on port 8080. Run a worker from the same image by passing
`deployment-worker` or `evaluation-worker` as the command.

## Known limitation: Aurora DSQL authentication

`infra/aws/fargate-app` configures the service with a `jdbc:aws-dsql:postgresql://` URL and no
password, because Aurora DSQL authenticates with short-lived IAM tokens rather than a stored
password. The connection factory accepts only a static user and password and does not recognize
that URL form, so **the AWS stack cannot start this image until token authentication is
implemented**: generate a SigV4 DSQL auth token, use it as the password, and refresh it before
expiry for new pool connections. The migrator and every query are already DSQL-conformant, which
`check:dsql-conformance` and the migrator's dialect probe establish; only authentication is
missing.
