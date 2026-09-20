# Leptos console size analysis, September 19, 2026

Measured at commit 30b398e (9 of 17 journeys ported). Sizes in kB; brotli quality 11, gzip level 9.

## What is loaded

| | raw | gzip | brotli |
| --- | ---: | ---: | ---: |
| React: everything | 1686 | 434 | 369 |
| React: initial load (what `index.html` references; routes are `React.lazy`) | 918 | 174 | 150 |
| React: editor chunks, fetched on the editor route only | 560 | 197 | 165 |
| Leptos: `.wasm` + glue + CSS | 1132 | 413 | 319 |
| Leptos: CodeMirror bundle, imported eagerly by the wasm-bindgen glue | 681 | 213 | 173 |
| Leptos: initial load today | 1813 | 626 | 492 |

The server sends neither `Content-Encoding` nor `Cache-Control` (tower-http is built with `fs` and
`trace` only), so today every byte in the "raw" column crosses the wire, for both consoles, on every
visit that misses the heuristic cache.

## Where the `.wasm` bytes are (pre-`wasm-opt`, code and data, 1.68 MB)

| Share | What |
| ---: | --- |
| 24.8% | unattributed: wasm-bindgen glue, tables, type section |
| 20.1% | `hive_console` functions |
| 12.6% | library generics instantiated with `hive_console` types (counted inside the rows below) |
| 10.6% | `core` |
| 8.7% | `reactive_graph` |
| 8.6% | `tachys` (Leptos renderer) |
| 6.5% | `leptos_router` |
| 5.6% | static data |
| 4.3% | `alloc` |
| 2.7% | `serde_json` |

`url`, `idna`, ICU tables, and `regex` are in the dependency graph (via `leptos_router`,
`server_fn`, `leptos_config`) but contribute zero bytes: LTO removes them.

All code that mentions a `hive_console` type (548 kB pre-opt) splits into: views and components 62%,
serde (de)serialization of GraphQL types 15%, other 15%, reactive closures 6%, cynic query building
1.5%. By module, the three `api::*` modules are 29% of it: 56 `QueryFragment` structs at roughly
2.9 kB each, because serde monomorphizes a visitor per struct.

## Build options, measured through wasm-bindgen and `wasm-opt`

| Variant | raw | gzip | brotli |
| --- | ---: | ---: | ---: |
| current: `opt-level="z"`, fat LTO, 1 CGU, `panic=abort`, `erase_components`, `wasm-opt -Oz` | 1028 | 393 | 302 |
| + `wasm-opt --converge --strip-*` | 1025 | 395 | 303 |
| `opt-level="s"` | 1238 | 445 | 330 |
| without `erase_components` | 1105 | 442 | 339 |
| nightly `build-std` (`optimize_for_size`), `-Zlocation-detail=none -Zfmt-debug=none` | 979 | 376 | 289 |
| + `-Cpanic=immediate-abort` | 951 | 362 | 279 |

## After the first two measures (same day)

Precompressed brotli/gzip siblings served by `spa.rs` with `Cache-Control: immutable` on fingerprinted
files and `no-cache` on the entry point, and CodeMirror fetched by a dynamic `import()` on first use.
Measured in Chromium over CDP (`encodedDataLength`), first visit, empty cache:

| | before | after |
| --- | ---: | ---: |
| Leptos, project dashboard first load | 1813 kB | 324 kB |
| Leptos, additional on first editor use | 0 (already loaded) | 174 kB |
| React, project dashboard first load | 918 kB | 155 kB |

A repeat visit transfers only `index.html` and any unfingerprinted file's revalidation.

## Fragment consolidation, measured

Removing six duplicate `QueryFragment` structs (three `PageInfo` variants to one, shared project and
agent connections) took the release `.wasm` from 1,027,534 to 1,023,617 bytes raw: about 0.65 kB per
struct after `wasm-opt`, not the 2.9 kB the pre-optimization profile suggested, because `wasm-opt`
already merges much of the duplicated visitor code. Worth doing where it also simplifies the API
module; not worth shaping queries around. The earlier estimate of 5-10% was wrong; expect about 1%.
