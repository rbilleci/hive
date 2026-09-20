# Leptos console spike (plan phase 0), September 19, 2026

Scope: `/projects/:projectId/agents/:agentId/edit` plus the access shell, `/session-error`, and
`/access-denied`, in `crates/hive-console`. Host: 128 cores, 123 GB RAM, Linux; rustc 1.97.1.

Pinned: leptos 0.8.20, leptos_router 0.8.15, cynic 3.14.0, trunk 0.21.14, wasm-bindgen 0.2.

| Go/no-go measure | Result |
| --- | --- |
| `check:e2e:agent-draft-editor`, script unmodified, `HIVE_CONSOLE=leptos` | pass (also passes with React) |
| Dev rebuild after a one-line edit to the 545-line editor view (`trunk build`) | 4.7 s |
| Release build, size profile + `wasm-opt -Oz`, after the same edit | 24 s (34-60 s from a cold crate) |
| `hive-console_bg.wasm` | 615 kB raw, 253 kB gzip |
| wasm-bindgen glue `hive-console.js` | 52 kB raw, 9 kB gzip |
| CodeMirror bundle `hive-editor.js` (loaded by both consoles in some form) | 681 kB raw, 210 kB gzip |
| React console, all JS and CSS, for comparison | 432 kB gzip |
| CodeMirror bridge | 73 lines of JS, 5 `wasm_bindgen` bindings; typing, save, reload, and language switch verified in a browser |
| Time from navigation to the editor heading, local | 383 ms |
| Browser console errors during the probe | none |

Two findings that set the dev-loop numbers:

- Without `--cfg erase_components` the same one-line edit recompiles in 26-52 s; with it, about 2 s
  of cargo time. It is set for the WASM target in `crates/hive-console/.cargo/config.toml`. Release
  size with it is slightly smaller (615 kB vs 632 kB).
- `data-cargo-profile` on Trunk's `rel="rust"` link applies to dev builds too, which silently made
  every dev build an LTO build (23 s). The size profile is now passed only on the release command
  line (`trunk build --release --cargo-profile wasm-release`).

What the spike does not cover, by design: the sidebar shell and page header drawer, the
approved-model and dependency options in the Model section (needs the configuration queries), and
the unsaved-changes navigation blocker (`useBlocker` has no direct Leptos router equivalent; it
needs a small guard around in-app link clicks).

cynic notes: operations are named after their root struct, so structs keep the React operation
names (`AgentDraft`, `UpdateAgentDraft`, `ValidateAgentDraft`) that the end-to-end checks intercept;
a unit test pins this. The `JSON` scalar maps to `serde_json::Value` with one `impl_scalar!` line.
