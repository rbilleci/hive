# Hive Console: React to Leptos Plan

Status: complete (September 19, 2026). Every route is ported, `LFP-GATE-FINAL` passed, and `web/` is removed; see [Progress](#progress).

## Progress

| Phase | State | End-to-end checks passing with `HIVE_CONSOLE=leptos` |
| --- | --- | --- |
| 0 Spike | done; go/no-go measurements in `evidence/2026-09-19-leptos-spike/measurements.md` | `agent-draft-editor` |
| 1 Foundation | done | `organization-selector`, `console-context` |
| 2 Directory and dashboard | done | `organization-overview`, `organization-project-list`, `organization-project-authoring`, `project-dashboard`, `project-agent-list`, `console-navigation` |
| 3 Agents | done | `agent-operational-view`, `agent-authoring` |
| 4 Configuration | done | `configuration` |
| 5 Administration | done | `administration-settings` |
| 6 Deployment and approval | done | `deployment`, `approval` |
| 7 Evaluation and audit | done | `evaluation`, `audit` |
| 8 Switch and removal | done | all 17, `check:packaging`, `check:mvp-acceptance`, `check:mvp-accessibility`: `evidence/2026-09-19-leptos-final-gate/results.md` |

What phase 8 changed beyond the plan:

- The stylesheets moved to `crates/hive-console/styles`, and the operation documents to `schema/console-operations`. `check:schema:contract` still derives the console-reachable types from them, and the new `check:console:operations` holds their operation names equal to the `cynic` root structs. On its first run it found two roots from phase 5 that had drifted (`OrganizationAdministrationQuery`, `ProjectAdministrationQuery`); both are fixed.
- `RefineSession` was removed from the operation documents: only the Refine auth provider sent it.
- `graphql` and `playwright`, which the harness had resolved through the `web` workspace, are root `devDependencies` at the same versions.
- `HIVE_CONSOLE` is gone; every fixture serves `crates/hive-console/dist`.

Notes from phases 6 and 7:

- GraphQL enums are real `cynic::Enum` types (`api/enums.rs`), checked against the schema at build time. They are not yet shared with `hive-domain`.
- The transport calls `fetch` with a string URL and supports a deadline (`execute_within`, 10 s for deployment, approval, and evaluation requests, as in the React console) and a status-carrying failure (`execute_with_status`, used by audit for 403 and 503 wording).
- `cynic` deserializes strictly, so a test that fulfils a console request with hand-written JSON must include every field the console selects. `deployment.e2e` was updated for that.
- A page whose outer closure reads a raw state signal remounts on every reload, because setting a signal to the value it already holds still notifies. That dropped focus to `<body>` after a confirmed action. The outer closures of the deployment, approval, and evaluation pages read Memos, and the deployment detail renders its header, facts, and dialog separately.
- `ConfirmationDialog` takes focus synchronously; `deployment.e2e` now asserts that, and that focus returns to the opener after a confirmed action re-renders it.

Corrections to the plan as written, found while executing it:

- `console-context` and `console-navigation` open the project dashboard, both directories, and the
  create-project page, so they gate phase 2, not phase 1.
- The two directory pages were near-duplicates in React; they are one `KeysetDirectory` with two
  configurations here.
- Size was analyzed in `evidence/2026-09-19-leptos-size/analysis.md`. The large factors were on the
  loading side, not in compiler flags: the server sent everything uncompressed and uncached, and the
  CodeMirror bundle loaded on every page. Both are fixed; a first load is 324 kB on the wire. Sharing
  GraphQL fragment structs across queries was measured too and saves only about 0.65 kB per struct,
  so do it where it simplifies the API module and not otherwise. Still open: route-level WASM splitting, which Trunk cannot do
  and `cargo-leptos --split` does only for its own project layout; treat it as a separate spike.
- Release `.wasm` size history: 253 kB gzip after phase 0, 391 kB gzip after phase 2
  (about a third of the pages). The CodeMirror bundle adds 210 kB gzip. The React console is 432 kB
  gzip in total. If the trend holds, the finished Leptos console will be roughly twice React's size;
  brotli at the load balancer and a size audit (`twiggy`) before the final gate are the mitigations.
- The router takes the first matching route, so `/agents/new` must be registered before
  `/agents/:agent_id`. It has no `useBlocker`; `navigation_guard.rs` holds link clicks with a
  capture listener on `document` and holds Back/Forward with a `popstate` listener installed before
  the app mounts, because listeners on `window` itself run in registration order and the router's
  own listener unmounts the guarded page first.
- Leptos traps worth knowing before porting a page: `Memo::track()` does not evaluate a lazy memo
  (read it with `get`); a signal set to an equal value still notifies (gate layout on a `Memo`);
  `visibilitychange` needs a `document` listener; Trunk's `data-cargo-profile` applies to dev builds.

## Purpose

Replace the React console in `web/` with a Leptos console compiled to WebAssembly, so the whole product
is one language and a server schema change that breaks a console query fails `cargo build`. The
GraphQL API, the HTTP surface, the database, and the end-to-end harness do not change. The React
console stays the served default until the Leptos console passes every end-to-end check; then it is
deleted.

Requirements carry immutable identifiers of the form `LFP-<SLUG>`.

## Baseline (September 19, 2026)

| Fact | Value | Recompute with |
| --- | --- | --- |
| Hand-written console source | 6,840 lines: 27 `.tsx`, 9 `.ts`, 5 `.css` (921 lines of CSS) | `wc -l web/src/*.tsx web/src/*.ts web/src/providers/*.ts web/src/*.css` |
| Routes | 45 | `grep -oE 'path="[^"]+"' web/src/main.tsx \| sort -u \| wc -l` |
| GraphQL documents | 7 files, 117 operations and fragments | `grep -c "^query\|^mutation\|^fragment" web/src/graphql/*.graphql` |
| State handling | 199 `useState`, 88 `useEffect`; no `useQuery`/`useMutation` | `grep -rhoE "\buse(State\|Effect\|Query\|Mutation)\b" web/src` |
| Refine usage | 4 hook calls (`useGetIdentity`, `useCan`) | same |
| Third-party UI | CodeMirror 6 only (`CodeEditor.tsx`, 146 lines, 6 language modes); no component library | `web/package.json` |
| Behavior gate | 17 Playwright journeys, located by role and text | `ls scripts/*.e2e.mjs` |
| Unit tests | 8 Vitest tests in 2 files | `npm run --workspace web test` |

Two consequences. The console is a thin client: typed `*Api.ts` modules over one `executeGraphql`
function, with hand-rolled fetch-in-effect state, so there is no cache or data framework to replace.
And the end-to-end checks are framework-neutral, so they define "done" as long as the Leptos console
keeps the same accessible names, roles, text, and URLs.

## Decisions

`LFP-CSR`: the console is client-side rendered and built to static files. No server-side rendering,
no hydration, no Leptos server functions. Every data call goes through `POST /graphql`, so the
handler's authentication, audit metadata, `X-Request-Id`, telemetry, and 401/503 classification keep
covering all console traffic, and the integration harness keeps testing the path the console uses.
`crates/hive-api/src/spa.rs` and the `Dockerfile`'s serving shape do not change.

`LFP-CYNIC`: GraphQL access uses `cynic`, with query structs checked at compile time against
`schema/hive.graphql`. This replaces `graphql-codegen`. `check:generated:graphql` is retired at the
switch; `check:schema:contract` stays and now guards the Leptos console's contract the same way.

`LFP-CRATE`: the console is a new workspace member, `crates/hive-console`, targeting
`wasm32-unknown-unknown`, built with Trunk into `crates/hive-console/dist`. It may depend on
`hive-domain` and nothing else in the workspace. `RTD-CRATE-DIRECTION` extends accordingly:
`hive-domain` must compile for WASM, which today needs only `uuid`'s `js` feature enabled for that
target.

`LFP-DOMAIN-TYPES`: wire enums are declared in the console with `cynic` derives and converted with
`From` into `hive-domain` enums where the UI makes a lifecycle decision (deployment, approval,
evaluation status), so the domain's exhaustive `match` predicates are reused rather than re-spelled.
`hive-domain` gains no GraphQL or serde-wire dependency for this.

`LFP-PARITY`: accessible names, ARIA roles, visible text, CSS class names the harness locates
(`.organization-list`, `.directory-table-scroll`, `.organization-overview`, `.navigation-tree-*`,
`main.agent-directory`, `main.project-directory`), route paths, and query-string parameters are
frozen. The five CSS files move over unchanged. A harness script is edited only to fix a defect in the
script, never to accommodate the new console.

`LFP-SWITCH`: both consoles build side by side. `HIVE_WEB_DIST` selects which one a server serves, and
the harness gains `HIVE_CONSOLE=react|leptos` to set it for every fixture. React stays the default
until `LFP-GATE-FINAL`.

`LFP-CODEMIRROR`: CodeMirror stays a JavaScript dependency. A small ES module (bundled by Trunk as an
asset) exposes `create`, `setDocument`, `setLanguage`, `onChange`, and `destroy`; a `wasm-bindgen`
extern block wraps it in one Leptos component. No attempt is made to replace the editor with a Rust
one.

## Phases

Each phase ends with a gate: the named end-to-end checks pass with `HIVE_CONSOLE=leptos`, and
`npm run validate:local` still passes with the React default.

| # | Phase | Work | Gate (with `HIVE_CONSOLE=leptos`) |
| --- | --- | --- | --- |
| 0 | Spike and go/no-go | Agent draft editor route only: Trunk build, `cynic` query and mutation through the `operationName` check, CodeMirror shim, session cookie, one CSS file. Pin Leptos, `cynic`, and Trunk versions here. | `check:e2e:agent-draft-editor`; measurements in [Go/no-go](#go-no-go) recorded under `evidence/` |
| 1 | Foundation | `hive-console` crate; router with all 45 paths (unported routes render a placeholder); `graphql` module (execute, transport errors, 401 to `/session-error`); session and capability context replacing `useGetIdentity`/`useCan`; `ConsoleShell`, `ConsoleNavigation`, `PageHeader`, `ConfirmationDialog`, `JsonViewer`; display preferences and theme | `console-context`, `console-navigation`, `organization-selector` |
| 2 | Directory and dashboard | Organization list and overview, project directory, project dashboard, agent directory, create project; shared paged-table and filter/search-in-URL pattern | `organization-overview`, `organization-project-list`, `organization-project-authoring`, `project-dashboard`, `project-agent-list` |
| 3 | Agents | Operational overview, tabs, draft editor (from phase 0), versions, compare, authoring routes | `agent-operational-view`, `agent-draft-editor`, `agent-authoring` |
| 4 | Configuration | Prompts, policies, model profiles, MCP servers, catalog, environments | `configuration` |
| 5 | Administration | Organization and project settings, memberships, budget and approval policy, archive/restore | `administration-settings` |
| 6 | Deployment and approval | Deployment list, detail, timeline, preview, deploy/cancel/retry/promote/rollback, polling; approval inbox and decision | `deployment`, `approval` |
| 7 | Evaluation and audit | Definitions, drafts, versions, runs, targets; audit list and detail drawer | `evaluation`, `audit` |
| 8 | Switch and removal | See [Final gate](#final-gate) | all 17 |

Phases 2 through 7 are independent once phase 1 lands, and can be reordered or run in parallel.

### Per-feature porting recipe

1. Translate the feature's `.graphql` operations into `cynic` query structs and fragments in
   `hive-console/src/api/<feature>.rs`, mirroring the existing `<feature>Api.ts` function names.
2. Port each component. `useState` becomes a signal; a fetch-in-`useEffect` becomes a `Resource` (or
   `LocalResource`) keyed on the same inputs, rendered under `<Suspense>`; a mutation becomes an
   `Action` that refetches the affected resources, which is what the React code does by hand today.
3. Keep markup, class names, labels, and text identical; diff the rendered HTML against the React
   page where practical.
4. Port unit-level logic (`consoleModel.ts`, the transport tests) as `#[test]`s; add
   `wasm-bindgen-test` only where a browser API is involved.
5. Run the feature's end-to-end check against both consoles.

## Build and tooling changes

- `package.json`: `build:console` runs `trunk build --release`; `check:e2e:*` and `check:packaging`
  honor `HIVE_CONSOLE`; `validate:local` runs the end-to-end set for both consoles from phase 1 until
  the switch.
- `scripts/local-service.mjs`: sets `HIVE_WEB_DIST` from `HIVE_CONSOLE`.
- `scripts/spa-packaging.mjs`: asset discovery reads the chosen `dist`; add a probe that `.wasm` is
  served with `Content-Type: application/wasm` (required for streaming compilation). Trunk emits hashed
  files beside `index.html`, not under `/assets`; either configure Trunk's output directory to
  `assets/` or extend `spa.rs` so a missing `.wasm`/`.js` file returns 404 instead of `index.html`.
- `check:rust`: `cargo clippy` and `cargo test` exclude the WASM crate from the host build and add a
  `--target wasm32-unknown-unknown` pass for it.
- `Dockerfile`: the `web` stage becomes a Rust stage with the WASM target and Trunk; the Node stage
  remains only if the CodeMirror bundle needs it.
- Developer loop: `trunk serve` with a proxy for `/graphql`, `/health`, `/local-dev`, replacing the
  Vite proxy.

## Go/no-go

Recorded at the end of phase 0, decided before phase 1 starts.

| Measure | Proceed if |
| --- | --- |
| Incremental rebuild after a one-line component edit | acceptable to the people doing the port; record the number |
| Release `.wasm` size, gzip and brotli, after `wasm-opt` | same order of magnitude as today's JavaScript (434 kB gzip across all JS and CSS chunks, of which the CodeMirror chunks remain either way) |
| CodeMirror shim | under ~150 lines of glue, no lost editor behavior in the e2e check |
| `cynic` ergonomics | the draft editor's fragments and the `JSON`/`Long` scalars express cleanly |
| `check:e2e:agent-draft-editor` | passes unmodified |

If the spike fails on rebuild time or the editor shim, stop; the fallback is to keep React and remove
Refine and TanStack Query, which is worthwhile regardless.

## Final gate

`LFP-GATE-FINAL`: with `HIVE_CONSOLE=leptos`, all 17 end-to-end checks, `check:packaging`,
`check:mvp-acceptance`, and `check:mvp-accessibility` pass on one committed tree. Then, in one commit:
the default flips; `web/`, the root npm workspace entry for it, `check:generated:graphql`, `check:web`,
and the Vite proxy are deleted; `HIVE_WEB_DIST` defaults to the Leptos `dist`; `README.md`, the
`Dockerfile`, and `check:standalone`'s required-path list are updated. Node remains a dependency of the
harness only.

## Risks

| Risk | Mitigation |
| --- | --- |
| Edit-compile-reload is seconds, not milliseconds | Measured in phase 0 and is a stop condition; keep the crate small and split by feature module to help incremental builds |
| Subtle behavior drift (focus management, keyboard navigation, debounced search-in-URL, polling intervals) | The end-to-end checks already assert these; port them deliberately, and compare against React side by side while both exist |
| Accessibility regressions the harness does not cover | `mvp-accessibility` covers only the audit route; add an axe pass over the main routes for both consoles in phase 1 so regressions show as a diff |
| Leptos API churn | Pin versions in phase 0; upgrade only between phases |
| WASM bundle size | `opt-level = "z"`, LTO, `wasm-opt`, brotli at the load balancer; CodeMirror stays a lazily loaded JS chunk |
| Long dual-maintenance window | Freeze React feature work during the port, or require every console change to land in both until the switch |
| Fewer people can maintain a Leptos UI | Accept explicitly before phase 1; this is the main non-technical cost |

## Out of scope

Server-side rendering, server functions, replacing GraphQL, visual redesign, new console features,
mobile layouts beyond what the CSS already does, and any change to the API contract.
