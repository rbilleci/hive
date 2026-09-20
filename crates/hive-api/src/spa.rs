use crate::state::AppState;
use axum::Router;
use std::path::Path;
use tower_http::services::{ServeDir, ServeFile};

/// Mirrors Quinoa's `enable-spa-routing` plus `ignored-path-prefixes=/graphql,/health,/assets,/local-dev`.
/// `RTD-SPA-SERVING`. Explicit routes for `/graphql`, `/health*`, and `/local-dev/login`
/// always win over this fallback because axum matches an exact route before a
/// fallback service; `/assets` is nested separately so a missing asset 404s instead
/// of falling back to `index.html`.
pub fn merge_spa_routes(router: Router<AppState>, web_dist: &Path) -> Router<AppState> {
    let index_html = web_dist.join("index.html");
    let assets_dir = web_dist.join("assets");

    router
        .nest_service("/assets", ServeDir::new(assets_dir))
        .fallback_service(ServeDir::new(web_dist).fallback(ServeFile::new(index_html)))
}
