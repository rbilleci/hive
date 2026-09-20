use crate::state::AppState;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::net::SocketAddr;
use uuid::Uuid;

const DEFAULT_PRINCIPAL: &str = "00000000-0000-0000-0000-000000000001";

#[derive(Deserialize)]
pub struct LoginQuery {
    principal: Option<String>,
    redirect: Option<String>,
}

/// Mirrors `LocalDevAutoLoginResource`. `RTD-LOCAL-DEV-LOGIN`: a local testing
/// convenience only, refused unless explicitly enabled and called from loopback.
pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<LoginQuery>,
) -> Response {
    if !state.local_dev_autologin_enabled || !peer.ip().is_loopback() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let principal = match Uuid::parse_str(query.principal.as_deref().unwrap_or(DEFAULT_PRINCIPAL)) {
        Ok(principal) => principal,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "The principal must be a UUID.").into_response()
        }
    };
    let redirect = query.redirect.unwrap_or_else(|| "/".to_string());

    let cookie = state.session_verifier.issue(principal);
    let cookie_header = format!("sf_session={cookie}; Path=/; HttpOnly; SameSite=Lax");

    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie_header).expect("cookie value is valid ASCII"),
    );
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&redirect).unwrap_or_else(|_| HeaderValue::from_static("/")),
    );
    response
}
