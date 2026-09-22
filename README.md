# Hive

**An AI agent is a text file with production credentials.**

Software that can act in production earned its release controls the hard way: version control,
code review, staged environments, sign-off, rollback, an audit trail. Agents arrived with all of
that blast radius and none of those controls. An agent calls internal tools, spends money against
a real budget, and acts on customer data — and it is authored as prose, changed in an afternoon by
someone who does not write code, and shipped by editing a field.

Hive closes that gap. It is a control plane that makes an agent a governed, deployable artifact:
authored as a draft, validated by the server, frozen into an immutable version, evaluated against
a suite, and released into an environment only when the approvals its project's policy demands
have actually been recorded.

## Why this becomes urgent

One agent is a manageable exception. A director approves it in Slack, an engineer edits the
prompt, and everyone involved remembers what happened.

The second, fifth, and twentieth agent break that. Teams stand them up independently, each with
tool access and spend authority, each changing weekly. The governance that was a conversation at
one agent has no mechanism at twenty, and the failure mode is not that an agent answers badly. It
is that when one does, nobody can say what changed, who approved it, which version was live, or
what evidence anyone relied on. Manual processes do not degrade gracefully here; they stop
existing.

Two forces make that arrive sooner than most organizations plan for. Agents are moving from
drafting text to taking actions, which converts a quality problem into a liability problem. And
the people who must sign off — risk, security, finance, compliance — are not the people building,
so sign-off has to be a system rather than a habit.

## What Hive is

An organization contains projects; a project contains agents, the configuration they reference,
the evaluation suites that judge them, and the policies that govern their release. Work moves
through the stages in the table that follows, each a set of GraphQL mutations that
`schema/hive.graphql` defines.

| Stage | What happens |
| --- | --- |
| Author | An agent draft is edited section by section, over the sections `SECTIONS` freezes in `crates/hive-application/src/agent/canonical_document.rs`. The server validates it and answers with diagnostics against editor paths; no client-supplied digest or validation verdict is trusted |
| Configure | Reusable resources and MCP (Model Context Protocol) server descriptors are published per project and referenced through typed references, so changing a shared resource is a versioned event rather than an edit in place. The descriptors are inert: no column stores a credential, a raw header or environment value, a health result, or executable state |
| Publish | A validated draft becomes an immutable agent version. Later edits open a new draft; the published version never changes |
| Evaluate | An evaluation definition, itself drafted and published as a version, runs against a target and lands in one of the categories `EvaluationOutcomeCategory` defines |
| Deploy | A version is requested into an environment under one of the strategies `DeploymentStrategy` defines, gathers the approvals its project's policy requires, then moves through the states `DeploymentLifecycleStatus` defines |

Deployments can be canceled, retried, rolled back to a prior active target, and promoted. A
promotion records the runtime-health generation it was taken against, so a later observation
cannot be mistaken for the one that justified it. Every transition writes an audit event, and
every project carries a budget policy with a currency, a monthly limit, and a warning threshold,
reported against imported spend.

## Why this is hard to copy

Governance that can be bypassed is theater, and bypassable is the default outcome when controls
live in a user interface. The guarantees this section names hold because the server enforces each
one against state it has locked, in code that performs no I/O and reads nothing the browser supplied. That is
an architectural commitment made early, not a feature added later.

An approval decision runs as a pure function over locked facts
(`crates/hive-application/src/deployment/policy.rs`). It refuses a requester approving their own
release, a second decision from an approver already counted, an approver the policy does not
qualify, and a rejection carrying no reason. Approvals are judged against a frozen cycle — a
policy snapshot and a plan compared by digest — so when a later change means those facts can no
longer hold, reconciliation terminalizes the cycle with an invalidation code and any decision
arriving afterward is refused. A stale approval cannot quietly carry a release.

A project's approval policy may only become stronger. Weakening it is a refused command rather
than a race against an in-flight deployment.

Authority is computed, never stored. Role names are facts; the capability allow-list in
`crates/hive-persistence/src/capability/` is what the code consults, and every command
re-evaluates it inside its own transaction against rows it has locked, so authority cannot shift
between the check and the write. Audit history redacts by the same mechanism: a principal without
`AUDIT_SENSITIVE.VIEW` at an event's scope sees a `sensitiveFieldsRedacted` flag instead of a gap
to infer, and the underlying columns are not exposed as fields at all, so no client can select,
filter, or order on them.

A competitor starting from a prompt store with a review screen cannot retrofit these properties.
They are the difference between a workflow that records intent and a system that can be relied on
when someone disputes what happened.

## Where it sits

Three categories of tooling already touch this space, and none occupies it.

| Category | What it does | What it leaves open |
| --- | --- | --- |
| Observability and evaluation platforms | Measure how an agent performed, trace runs, score outputs | Reporting, not gating. They describe a release; they do not decide whether it may happen |
| Agent frameworks and SDKs | Help an engineer build and run one agent | Build-time concerns. They have no opinion about who may ship a change, or into which environment |
| Prompt and config management | Version a string and diff it | Versioning is not a release process. No policy, no approval, no evidence, no environment promotion |

Hive treats evaluation as one evidence input to a release decision rather than as the product, and
puts the decision itself under policy.

## Status

This is a complete, working system rather than a product with customers. The service runs locally against PostgreSQL today: the full authoring-to-release
lifecycle executes, the console drives it, and the validation harness in `scripts/` covers
it with unit, database, integration, packaging, and browser checks that `npm run validate:local`
runs as one gate.

The service targets Aurora DSQL for horizontal scale, and every migration and query already
conforms to what DSQL accepts. One gap blocks the AWS deployment: DSQL authenticates with
short-lived IAM tokens, and the connection factory accepts only a static user and password. The
Known Limitation section of this file states what implementing it requires.

## Architecture

These constraints shape this codebase more than any framework choice, and a check enforces each
one rather than a convention.

The schema uses nothing Aurora DSQL rejects: no foreign keys, triggers, functions, rules,
sequences, or advisory locks. Commands lock the row they will write with `FOR UPDATE` and rely on
the commit-time serialization failure (SQLSTATE 40001) instead of a revision check in a `WHERE`
clause. `npm run check:dsql-conformance` fails on any rejected construct. Any PostgreSQL-compatible
database reachable with a user and password runs it today.

The read tier is generated. Entities register with Seaography, which composes queries, filters,
pagination, and relations; hand-written resolvers exist only for commands and computed fields.
`npm run check:idiomatic` fails on raw SQL, on hand-built GraphQL in the API crate, on an
unregistered entity, and on a foreign-key-like column with no declared relation.

The dependency direction is inverted and checked. `hive-application` holds the rules and the
ports; `hive-persistence` depends on it to implement those ports, not the reverse. That crate's
manifest names no database and no transport, so a service runs in a unit test against a stub port
with no PostgreSQL in reach. `npm run check:architecture` scans the manifests, because Cargo proves
a dependency graph exists but not which way it points.

The console is type-checked against the served schema. `schema/hive.graphql` is a committed
contract, and `cynic` checks every console operation against it at compile time, so a schema change
that breaks the console fails the console build rather than a browser request.

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

## Run it locally

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

## How the console is served

`npm run build:console` writes `.br` and `.gz` siblings beside every compressible file, which
`scripts/precompress.mjs` produces, and the server sends the one a client accepts. Files carrying a
content fingerprint in their name are served `Cache-Control: public, max-age=31536000, immutable`;
`index.html` and anything unfingerprinted are served `no-cache`, so a release is picked up on the
next navigation. A request naming a file that does not exist answers `404`, never the `index.html`
fallback.

## Container image

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
