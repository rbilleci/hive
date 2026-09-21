//! The HTTP surface itself: static-asset serving and its cache headers, `/health`, the session
//! cookie `/graphql` demands, and the GraphQL request envelope. Nothing here reads a domain.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test http -- --ignored

mod common;

use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use std::net::SocketAddr;
use tower::ServiceExt;

#[tokio::test]
#[ignore]
async fn spa_serves_index_html_at_root_and_on_unknown_routes() {
    let router = build_test_router().await;

    let root = router
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(root.status(), StatusCode::OK);

    let deep_link = router
        .clone()
        .oneshot(
            Request::get("/organizations/anything")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deep_link.status(), StatusCode::OK);
    let body = deep_link.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("hive console placeholder"));
}

#[tokio::test]
#[ignore]
async fn missing_asset_404s_instead_of_falling_back_to_index() {
    let router = build_test_router().await;
    let response = router
        .oneshot(
            Request::get("/assets/does-not-exist.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore]
async fn missing_root_file_404s_while_a_dotless_route_falls_back() {
    let router = build_test_router().await;
    for path in ["/hive-console-0123456789abcdef_bg.wasm", "/missing.js"] {
        let response = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
#[ignore]
async fn fingerprinted_files_are_immutable_and_the_entry_point_revalidates() {
    let router = build_test_router().await;
    let cache_control = |response: &axum::response::Response| {
        response
            .headers()
            .get("cache-control")
            .map(|value| value.to_str().unwrap().to_string())
    };
    let asset = router
        .clone()
        .oneshot(Request::get("/assets/app.js").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        cache_control(&asset).as_deref(),
        Some("public, max-age=31536000, immutable")
    );
    for path in ["/", "/organizations/anything"] {
        let entry = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(cache_control(&entry).as_deref(), Some("no-cache"), "{path}");
    }
}

#[tokio::test]
#[ignore]
async fn a_precompressed_sibling_is_served_to_a_client_that_accepts_it() {
    let router = build_test_router().await;
    let web_dist = std::env::temp_dir().join("hive-api-http-integration-web-dist");
    std::fs::write(
        web_dist.join("assets").join("app.js.br"),
        b"not really brotli",
    )
    .unwrap();
    let compressed = router
        .clone()
        .oneshot(
            Request::get("/assets/app.js")
                .header("accept-encoding", "br")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(compressed.headers().get("content-encoding").unwrap(), "br");
    let plain = router
        .oneshot(Request::get("/assets/app.js").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(plain.headers().get("content-encoding").is_none());
}

#[tokio::test]
#[ignore]
async fn health_reports_the_pre_first_tick_default_state() {
    let router = build_test_router().await;
    let response = router
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = json_body(response).await;
    assert_eq!(body["status"], "degraded");
    // While archive reconciliation (`upgrade`) is unhealthy, `/health` reports it under
    // `approvalMaintenance*` instead of expiry's own `MAINTENANCE_NOT_COMPLETED` default,
    // whatever expiry's state. That holds from boot until the first successful
    // archive-reconciliation pass, not only while genuinely degraded later.
    assert_eq!(
        body["approvalMaintenanceFailureCode"],
        "COMPATIBILITY_BACKFILL_PENDING"
    );
    assert_eq!(
        body["approvalUpgradeMaintenanceFailureCode"],
        "COMPATIBILITY_BACKFILL_PENDING"
    );
}

#[tokio::test]
#[ignore]
async fn graphql_requires_a_session_cookie_before_parsing_the_query() {
    let router = build_test_router().await;
    let response = router
        .oneshot(
            Request::post("/graphql")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"{ __typename }"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn graphql_rejects_a_body_with_no_query_before_auth_details_matter() {
    let router = build_test_router().await;
    let response = router
        .oneshot(
            Request::post("/graphql")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore]
async fn graphql_accepts_a_cookie_minted_by_local_dev_login() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let response = router
        .oneshot(
            Request::post("/graphql")
                .header("content-type", "application/json")
                .header("cookie", cookie)
                .body(Body::from(
                    r#"{"query":"{ principals { nodes { id subject } } }"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    // "Who is this cookie" is the generated `principals` read: Ada administers no organization and
    // views no project's memberships, so the tenant rule answers her own row and nothing else.
    // `subject` is the stored column.
    assert_eq!(
        body["data"]["principals"]["nodes"],
        serde_json::json!([{ "id": ADA, "subject": "ada.fixture" }])
    );
}

#[tokio::test]
#[ignore]
async fn graphql_root_type_name_is_query_not_the_rust_struct_name() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let response = router
        .oneshot(
            Request::post("/graphql")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(r#"{"query":"{ __typename }"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["data"]["__typename"], "Query");
}

#[tokio::test]
#[ignore]
async fn graphql_rejects_an_operation_name_the_document_does_not_define() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let response = router
        .oneshot(
            Request::post("/graphql")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(
                    r#"{"query":"query Foo { principals { nodes { id } } }","operationName":"Bar"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(
        body["errors"][0]["message"],
        "The GraphQL operationName does not match the document."
    );
}

#[tokio::test]
#[ignore]
async fn graphql_accepts_an_operation_name_the_document_defines() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let response = router
        .oneshot(
            Request::post("/graphql")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(
                    r#"{"query":"query Foo { principals { nodes { id } } }","operationName":"Foo"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await["data"]["principals"]["nodes"][0]["id"],
        ADA
    );
}

#[tokio::test]
#[ignore]
async fn local_dev_login_refuses_a_non_loopback_peer() {
    let router = build_test_router().await;
    let mut request = Request::get("/local-dev/login")
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 5], 12345))));
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
