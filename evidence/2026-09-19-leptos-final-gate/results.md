# LFP-GATE-FINAL

Tree: 847cb59db9c99f219aa018317a33dc7f0141d8b8 (branch standalone-repo), September 19, 2026.
Environment: HIVE_CONSOLE=leptos, local Postgres 16 on port 15432, Chromium 1228.

```
e2e administration-settings exit 0
e2e agent-authoring exit 0
e2e agent-draft-editor exit 0
e2e agent-operational-view exit 0
e2e approval exit 0
e2e audit exit 0
e2e configuration exit 0
e2e console-context exit 0
e2e console-navigation exit 0
e2e deployment exit 0
e2e evaluation exit 0
e2e organization-overview exit 0
e2e organization-project-authoring exit 0
e2e organization-project-list exit 0
e2e organization-selector exit 0
e2e project-agent-list exit 0
e2e project-dashboard exit 0
spa-packaging exit 0
mvp-acceptance exit 0
mvp-accessibility exit 0
```

## Container image

`docker build` of the tree after this gate (Trunk console stage) succeeded. The running image served
`index.html` for a deep link (200), the fingerprinted WebAssembly with `content-encoding: br`
(612,374 bytes) and `cache-control: public, max-age=31536000, immutable`, and answered 404 for a
missing asset. Image size 179 MB.

## validate:local

```
validate:local candidate=d46cfe393c61b6147d017b7d1b5f5a3cfaaa6e6d tree=66ab82996be427e631c9977c368b4552dc17db63 state=clean
validate:local candidate=d46cfe393c61b6147d017b7d1b5f5a3cfaaa6e6d tree=66ab82996be427e631c9977c368b4552dc17db63 state=clean checks=46 verified-after-checks
```
