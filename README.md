# Hive

Hive is a control plane for organizations, projects, and agents: authoring, configuration,
deployment with approval policy, evaluation, and audit history. This repository holds the whole
product:

| Path | Contents |
| --- | --- |
| `crates/` | The Rust service (domain, application services, PostgreSQL persistence, GraphQL/HTTP API, and the `hive` binary) and `hive-console`, the Leptos console compiled to WebAssembly |
| `db/` | Migrations and seed data, embedded into the binary at compile time |
| `schema/` | `hive.graphql`, the SDL the service serves; the console compiles its operations against it |
| `scripts/` | The Node validation harness: integration, end-to-end, packaging, and conformance checks |
| `infra/` | Local PostgreSQL (Docker Compose) and the AWS Terraform stacks |
| `docs/` | The design the service is built to, and the register of deviations from it |

Nothing here builds, runs, or validates against another checkout; `npm run check:standalone`
enforces that.

## Prerequisites

- Rust stable (`rust-toolchain.toml` selects the channel) with the `wasm32-unknown-unknown` target, and Trunk 0.21 (`cargo install --locked trunk`), for the console
- Node.js 22 and npm, for the validation harness and the console's CodeMirror bundle
- Docker, for the local PostgreSQL and for building the image

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
<http://127.0.0.1:8080/local-dev/login?principal=00000000-0000-0000-0000-000000000001> to sign in as
the seeded administrator, Ada Lovelace; the login route answers only on loopback and only while
`HIVE_LOCAL_AUTOLOGIN_ENABLED=true`.

Deployments and evaluations progress only while their workers run, each in its own process:

```sh
target/release/hive deployment-worker
target/release/hive evaluation-worker
```

The other subcommands are `hive migrate` (migrate and seed, then exit) and `hive schema-sdl` (print
the GraphQL schema; needs no database). Every process exits cleanly on `SIGTERM` or `SIGINT`.

For console development, keep `hive serve` running and run `trunk serve` in `crates/hive-console`
(build the editor bundle once first: `npm run --workspace @hive/console-editor build`). Trunk serves
the console on port 5173, rebuilds and reloads on every change, and forwards `/graphql`, `/health`,
and `/local-dev` to `http://127.0.0.1:8080` (`Trunk.toml`), so the session cookie stays same-origin.

### Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `HIVE_DATABASE_URL` | `jdbc:postgresql://127.0.0.1:5432/hive` | `jdbc:postgresql://` and `postgres://` forms are accepted |
| `HIVE_DATABASE_USER` / `HIVE_DATABASE_PASSWORD` | `hive` / `hive` | |
| `HIVE_IDENTITY_SIGNING_KEY` | none, required by `serve` | HMAC key for the `sf_session` cookie |
| `HIVE_BIND_ADDRESS` / `HIVE_PORT` | `127.0.0.1` / `8080` | The image sets the address to `0.0.0.0` |
| `HIVE_WEB_DIST` | `crates/hive-console/dist`, relative to the working directory | The image sets `/srv/web` |
| `HIVE_LOCAL_AUTOLOGIN_ENABLED` | `false` | Enables `/local-dev/login` on loopback |
| `RUST_LOG` | `info` | Standard `tracing` filter syntax |

## Validate

`npm run validate:local` is the full gate. It requires a clean committed tree, builds once, and runs
every check below against that build. Each check also runs alone.

| Check | Proves |
| --- | --- |
| `check:standalone` | No file reaches outside this repository |
| `check:dsql-conformance` | Migrations and persistence code avoid what Aurora DSQL rejects (foreign keys, triggers, functions, rules, sequences, advisory locks) |
| `check:rust` | `rustfmt`, `clippy -D warnings`, and the unit tests |
| `check:rust:database` | The database-backed Rust tests, against an isolated, pre-migrated database |
| `check:schema:contract` | `schema/hive.graphql` equals the served SDL and is valid GraphQL |
| `check:console` | `clippy -D warnings` for `wasm32-unknown-unknown` and the console's unit tests. `cynic` checks every operation against `schema/hive.graphql` when the console compiles |
| `check:idiomatic` | Fails on any raw SQL, hand-built GraphQL, unregistered entity or missing relation |
| `check:integration:*` | GraphQL behavior per feature, each against its own database and service process |
| `check:packaging` | A copied console build serves correctly: history fallback, asset 404s, server-owned paths |
| `check:e2e:*` | Browser journeys through the built console |

The harness reaches PostgreSQL on `HIVE_POSTGRES_PORT` (default `5432`) and creates and drops one
database per check. `HIVE_BINARY` names a prebuilt binary; without it the harness builds one.
`HIVE_FIXTURE_LOG=<file>` captures every fixture process's output. The end-to-end checks use
Playwright's Chromium; if the pinned revision is not installed, `HIVE_CHROMIUM_PATH` names another
Chromium executable.

After a schema change, run `npm run generate:schema` and commit the result; the console then fails to
compile wherever it no longer matches.

## How the console is served

`npm run build:console` writes `.br` and `.gz` siblings beside every
compressible file (`scripts/precompress.mjs`), and the server sends the one a client accepts. Files
with a content fingerprint in their name are served `Cache-Control: public, max-age=31536000,
immutable`; `index.html` and anything unfingerprinted are served `no-cache`, so a release is picked up
on the next navigation. A request that names a file which does not exist answers `404`, never the
`index.html` fallback.

## Container image

```sh
docker buildx build --platform linux/arm64 -t hive-service .   # the Fargate task definition runs ARM64
```

The image holds the `hive` binary and the console build, runs as an unprivileged user, and defaults to
`hive serve` on port 8080. Run a worker from the same image by passing `deployment-worker` or
`evaluation-worker` as the command.

## Known limitation: Aurora DSQL authentication

`infra/aws/fargate-app` configures the service with a `jdbc:aws-dsql:postgresql://` URL and no
password, because Aurora DSQL authenticates with short-lived IAM tokens instead of a stored password.
The connection factory accepts only a static user and password and does not recognize that URL form,
so **the AWS stack cannot start this image until token authentication is implemented** (generate a
SigV4 DSQL auth token, use it as the password, and refresh it before expiry for new pool
connections). The migrator and every query are already DSQL-conformant
(`check:dsql-conformance`, and the migrator's dialect probe); only authentication is missing. Any
PostgreSQL-compatible database reachable with a user and password works today.
