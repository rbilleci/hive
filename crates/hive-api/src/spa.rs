use crate::state::AppState;
use axum::extract::Request;
use axum::http::header::{HeaderValue, CACHE_CONTROL, CONTENT_TYPE, VARY};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use std::path::Path;
use tower_http::services::{ServeDir, ServeFile};

/// Mirrors Quinoa's `enable-spa-routing` plus `ignored-path-prefixes=/graphql,/health,/assets,/local-dev`.
/// Explicit routes for `/graphql`, `/health*`, and `/local-dev/login`
/// always win over this fallback because axum matches an exact route before a
/// fallback service; `/assets` is nested separately so a missing asset 404s instead
/// of falling back to `index.html`.
///
/// A console build may carry `.br` and `.gz` siblings (`scripts/precompress.mjs`); a client that
/// accepts one receives it, and a build without them is served as it is.
pub fn merge_spa_routes(router: Router<AppState>, web_dist: &Path) -> Router<AppState> {
    let index_html = web_dist.join("index.html");
    let assets_dir = web_dist.join("assets");
    let compressed = |directory: &Path| {
        ServeDir::new(directory)
            .precompressed_br()
            .precompressed_gzip()
    };

    let console = Router::new()
        .nest_service("/assets", compressed(&assets_dir))
        .fallback_service(
            compressed(web_dist).fallback(
                ServeFile::new(index_html)
                    .precompressed_br()
                    .precompressed_gzip(),
            ),
        )
        .layer(middleware::from_fn(cache_policy));
    router.merge(console)
}

/// A fingerprinted file never changes under its name, so a browser may keep it for good. Everything
/// else, `index.html` above all, is revalidated on each use; otherwise a release would stay
/// invisible behind a cached entry point that still names the previous release's files.
async fn cache_policy(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let fingerprinted = is_fingerprinted(&path);
    let mut response = next.run(request).await;
    // A console route never names a file. A request for one (`/app-<hash>.wasm` from a stale
    // entry point) that reached the `index.html` fallback is a missing file, and answering it with
    // HTML would hand a browser markup where it expects a module.
    let is_html = response
        .headers()
        .get(CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(b"text/html"));
    if names_a_file(&path) && is_html && !path.ends_with(".html") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if response.status().is_success() {
        let policy = if fingerprinted {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static(policy));
        response
            .headers_mut()
            .insert(VARY, HeaderValue::from_static("accept-encoding"));
    }
    response
}

fn names_a_file(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|segment| segment.contains('.'))
}

/// Vite fingerprints every file under `/assets/`. Trunk writes `name-<16 hex>.ext` beside
/// `index.html` and puts wasm-bindgen snippets under `/snippets/<crate>-<16 hex>/`.
fn is_fingerprinted(path: &str) -> bool {
    if path.starts_with("/assets/") {
        return true;
    }
    path.split(['/', '.', '_']).any(|segment| {
        segment.rsplit_once('-').is_some_and(|(_, hash)| {
            hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::is_fingerprinted;

    #[test]
    fn recognizes_vite_and_trunk_fingerprints_only() {
        for path in [
            "/assets/index-CUl5bPcl.js",
            "/hive-console-28164612ad6e1ff8_bg.wasm",
            "/hive-console-28164612ad6e1ff8.js",
            "/styles-4b95124f653b1ecc.css",
            "/snippets/hive-console-03ccd076a8d43454/editor-js/dist/hive-editor.js",
        ] {
            assert!(is_fingerprinted(path), "{path}");
        }
        for path in [
            "/",
            "/index.html",
            "/organizations",
            "/hive-editor.js",
            "/projects/50000000-0000-0000-0000-000000000001",
        ] {
            assert!(!is_fingerprinted(path), "{path}");
        }
    }
}
