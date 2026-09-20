//! Requires a live, empty PostgreSQL database named by `HIVE_TEST_DATABASE_URL`
//! (falls back to `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test http_integration -- --ignored

use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use sqlx::postgres::PgPoolOptions;
use std::net::{Ipv4Addr, SocketAddr};
use tower::ServiceExt;

fn test_database_url() -> String {
    std::env::var("HIVE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hive:hive@127.0.0.1:15432/hive".to_string())
}

/// Tests run concurrently by default and share one live database. Every test
/// that lists Product's (10000000-...-0001) projects asserts an exact
/// slug/count set, so a test that creates (even temporarily) a project under
/// Product must serialize against them — otherwise a listing test can observe
/// the extra row mid-run. Acquired by both sides: the project-listing tests
/// below and the two administration tests that create a throwaway project.
static PRODUCT_PROJECTS_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

async fn lock_product_projects() -> tokio::sync::MutexGuard<'static, ()> {
    PRODUCT_PROJECTS_LOCK.lock().await
}

/// Same concern, scoped to project 50000000-...-0001's (Customer Feedback
/// Copilot) exact agent list/count: acquired by the agent-listing tests below
/// and by the agent-draft test that temporarily creates a throwaway agent
/// under that project.
static PROJECT_AGENTS_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

async fn lock_project_agents() -> tokio::sync::MutexGuard<'static, ()> {
    PROJECT_AGENTS_LOCK.lock().await
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn authenticated_cookie(router: &axum::Router) -> String {
    authenticated_cookie_for(router, "00000000-0000-0000-0000-000000000001").await
}

async fn authenticated_cookie_for(router: &axum::Router, principal: &str) -> String {
    let mut request = Request::get(format!("/local-dev/login?principal={principal}"))
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 54321))));
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

async fn build_test_router() -> axum::Router {
    // A plain eager connection here, separate from `connect_test_dynamic`'s deliberately lazy
    // one, since migrating issues real DDL immediately.
    // `max_connections(1)`: this handle only ever migrates, sequentially, then is dropped; the
    // default pool size multiplied by 65 concurrent tests is what exhausted Postgres's connection
    // limit the first time this was eager and unbounded (`ConnectionAcquire(Timeout)` from every
    // one of them).
    // Tests start together and each re-runs the migrator and seeds (which is also what resets
    // seed state between them). Queue them here, in-process, where waiting has no deadline;
    // queued on the migrator's own database lock, the last of 60+ tests hit its 30 s timeout.
    static MIGRATION_QUEUE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _turn = MIGRATION_QUEUE.lock().await;
    let mut migrator_options = sea_orm::ConnectOptions::new(test_database_url());
    migrator_options.max_connections(1);
    let migrator_db = sea_orm::Database::connect(migrator_options)
        .await
        .expect("connect the migrator to the test database");
    hive_persistence::migrate_and_seed(&migrator_db)
        .await
        .expect("migrate the test database");
    drop(_turn);

    let web_dist = std::env::temp_dir().join("hive-api-http-integration-web-dist");
    std::fs::create_dir_all(web_dist.join("assets")).unwrap();
    std::fs::write(
        web_dist.join("index.html"),
        "<html>hive console placeholder</html>",
    )
    .unwrap();
    std::fs::write(web_dist.join("assets").join("app.js"), "console.log(1)").unwrap();
    std::env::set_var("HIVE_WEB_DIST", &web_dist);
    std::env::set_var("HIVE_LOCAL_AUTOLOGIN_ENABLED", "true");

    let state = hive_api::test_state(&test_database_url(), "integration-test-signing-key").await;
    hive_api::build_router(state)
}

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
    // Ports `approvalMaintenanceStatus()`'s masking: while archive reconciliation (`upgrade`) is
    // unhealthy, it is reported under `approvalMaintenance*` instead of expiry's own
    // `MAINTENANCE_NOT_COMPLETED` default, regardless of expiry's state — true from boot until the
    // first successful archive-reconciliation pass, not just while genuinely degraded later.
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
                    r#"{"query":"{ currentPrincipal { id subject } }"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(
        body["data"]["currentPrincipal"]["id"],
        "00000000-0000-0000-0000-000000000001"
    );
    assert_eq!(
        body["data"]["currentPrincipal"]["subject"],
        "00000000-0000-0000-0000-000000000001"
    );
}

async fn generated(router: &axum::Router, cookie: &str, query: &str) -> serde_json::Value {
    let response = router
        .clone()
        .oneshot(
            Request::post("/graphql")
                .header("content-type", "application/json")
                .header("cookie", cookie)
                .body(Body::from(
                    serde_json::json!({ "query": query }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert!(body["errors"].is_null(), "{body:?}");
    body["data"].clone()
}

/// The generated API returns a member their organization, its projects and their agents through
/// Seaography's relation fields, filters and page pagination.
#[tokio::test]
#[ignore]
async fn generated_reads_traverse_relations_for_a_member() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let data = generated(
        &router,
        &cookie,
        r#"{ organizations(filters: { slug: { eq: "product" } }) { nodes { slug lifecycleStatus
             projects(orderBy: { displayName: ASC }, pagination: { page: { limit: 1, page: 0 } }) {
               paginationInfo { total pages }
               nodes { slug organizations { slug } agents { nodes { slug projects { slug } } } } } } } }"#,
    )
    .await;
    let organization = &data["organizations"]["nodes"][0];
    assert_eq!(organization["slug"], "product");
    assert_eq!(organization["lifecycleStatus"], "ACTIVE");
    let projects = &organization["projects"];
    assert!(
        projects["paginationInfo"]["total"].as_i64().unwrap() >= 1,
        "{projects:?}"
    );
    assert_eq!(
        projects["nodes"].as_array().unwrap().len(),
        1,
        "{projects:?}"
    );
    let project = &projects["nodes"][0];
    assert_eq!(project["organizations"]["slug"], "product");
    for agent in project["agents"]["nodes"].as_array().unwrap() {
        assert_eq!(agent["projects"]["slug"], project["slug"]);
    }
}

/// A principal with no membership gets no row from any generated root field. A filter that
/// returned `None` would fail open, so each root is checked, and the member case proves the same
/// filter admits rows when it should.
#[tokio::test]
#[ignore]
async fn generated_reads_return_nothing_without_a_membership() {
    let router = build_test_router().await;
    let member = authenticated_cookie(&router).await;
    let stranger = authenticated_cookie_for(&router, "99999999-9999-9999-9999-999999999999").await;
    let query =
        "{ organizations { nodes { id } } projects { nodes { id } } agents { nodes { id } } }";

    let seen = generated(&router, &member, query).await;
    let hidden = generated(&router, &stranger, query).await;
    for root in ["organizations", "projects", "agents"] {
        assert!(
            !seen[root]["nodes"].as_array().unwrap().is_empty(),
            "{root}: {seen:?}"
        );
        assert!(
            hidden[root]["nodes"].as_array().unwrap().is_empty(),
            "{root}: {hidden:?}"
        );
    }
}

/// Generated writes are not part of the API.
#[tokio::test]
#[ignore]
async fn generated_mutations_are_not_exposed() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let data = generated(
        &router,
        &cookie,
        "{ __schema { mutationType { fields { name } } } }",
    )
    .await;
    let names: Vec<&str> = data["__schema"]["mutationType"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field["name"].as_str().unwrap())
        .collect();
    for generated_name in ["organizationsCreateOne", "projectsUpdate", "agentsDelete"] {
        assert!(!names.contains(&generated_name), "{names:?}");
    }
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
                    r#"{"query":"query Foo { currentPrincipal { id } }","operationName":"Bar"}"#,
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
                    r#"{"query":"query Foo { currentPrincipal { id } }","operationName":"Foo"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await["data"]["currentPrincipal"]["id"],
        "00000000-0000-0000-0000-000000000001"
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

async fn graphql_as(router: &axum::Router, cookie: &str, query: &str) -> serde_json::Value {
    let response = router
        .clone()
        .oneshot(
            Request::post("/graphql")
                .header("content-type", "application/json")
                .header("cookie", cookie)
                .body(Body::from(
                    serde_json::json!({ "query": query }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

// Ada Lovelace (seeded principal 00000000-...-0001, organization-directory.sql) holds an
// active membership in organization 10000000-...-0001 (Product, ACTIVE) and
// 10000000-...-0002 (Support, ACTIVE) and 10000000-...-0003 (Quality Assurance,
// ARCHIVED), and a membership in 10000000-...-0004 (SRE, ACTIVE) that ended a day
// before the seed's fixed CURRENT_TIMESTAMP baseline. Beatrice Hopper (00000000-...-0002)
// is an active member of SRE only.

const PRODUCT: &str = "10000000-0000-0000-0000-000000000001";
const CUSTOMER_FEEDBACK_COPILOT: &str = "50000000-0000-0000-0000-000000000001";
const FEEDBACK_TRIAGE_AGENT: &str = "60000000-0000-0000-0000-000000000001";
const BEATRICE: &str = "00000000-0000-0000-0000-000000000002";

fn slugs(connection: &serde_json::Value) -> Vec<&str> {
    connection["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["slug"].as_str().unwrap())
        .collect()
}

#[tokio::test]
#[ignore]
async fn generated_organizations_exclude_an_ended_membership_and_order_by_display_name() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let data = generated(
        &router,
        &cookie,
        "{ organizations(orderBy: { displayName: ASC }) { nodes { slug } paginationInfo { total } } }",
    )
    .await;

    assert_eq!(
        slugs(&data["organizations"]),
        vec!["product", "quality-assurance", "support"],
        "ordered by displayName; sre's membership ended"
    );
    assert_eq!(data["organizations"]["paginationInfo"]["total"], 3);
}

#[tokio::test]
#[ignore]
async fn generated_organizations_lifecycle_filter_hides_archived_and_never_admits_an_ended_membership(
) {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let data = generated(
        &router,
        &cookie,
        r#"{ organizations(filters: { lifecycleStatus: { eq: "ACTIVE" } }, orderBy: { displayName: ASC }) {
             nodes { slug lifecycleStatus } paginationInfo { total } } }"#,
    )
    .await;

    assert_eq!(
        slugs(&data["organizations"]),
        vec!["product", "support"],
        "quality-assurance is archived; sre is active but its membership ended"
    );
    assert_eq!(data["organizations"]["paginationInfo"]["total"], 2);
}

#[tokio::test]
#[ignore]
async fn generated_organizations_page_pagination_reaches_every_row_exactly_once() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let mut seen = Vec::new();
    for page in 0..3 {
        let query = format!(
            "{{ organizations(orderBy: {{ displayName: ASC }}, pagination: {{ page: {{ limit: 1, page: {page} }} }}) {{ \
                nodes {{ slug }} paginationInfo {{ total pages current }} pageInfo {{ hasNextPage hasPreviousPage }} }} }}"
        );
        let data = generated(&router, &cookie, &query).await;
        let connection = &data["organizations"];
        assert_eq!(connection["paginationInfo"]["total"], 3, "{connection:?}");
        assert_eq!(connection["paginationInfo"]["pages"], 3, "{connection:?}");
        assert_eq!(connection["pageInfo"]["hasNextPage"], page < 2);
        assert_eq!(connection["pageInfo"]["hasPreviousPage"], page > 0);
        let page_slugs = slugs(connection);
        assert_eq!(page_slugs.len(), 1, "{connection:?}");
        seen.push(page_slugs[0].to_string());
    }
    assert_eq!(seen, vec!["product", "quality-assurance", "support"]);
}

// An organization by id and its projects relation: organization 0001 (Product) owns two
// projects, 50000000-...-0001 (Customer Feedback Copilot, ACTIVE) and 50000000-...-0002
// (Usage Analytics, ARCHIVED). Ada is a member of Product; Beatrice is not.

#[tokio::test]
#[ignore]
async fn generated_organization_by_id_is_hidden_from_a_principal_who_is_not_a_member() {
    let router = build_test_router().await;
    // `approval_inbox_and_decide_round_trip` grants Beatrice a Product membership for a moment,
    // under this lock.
    let _guard = lock_project_agents().await;
    let cookie = authenticated_cookie_for(&router, BEATRICE).await;

    let query = format!(
        "{{ organizations(filters: {{ id: {{ eq: \"{PRODUCT}\" }} }}) {{ nodes {{ id }} }} \
            visible: organizations {{ nodes {{ slug projects {{ nodes {{ slug agents {{ nodes {{ slug }} }} }} }} }} }} }}"
    );
    let data = generated(&router, &cookie, &query).await;

    assert_eq!(data["organizations"]["nodes"], serde_json::json!([]));
    assert_eq!(
        data["visible"]["nodes"],
        serde_json::json!([{
            "slug": "sre",
            "projects": { "nodes": [{
                "slug": "incident-response",
                "agents": { "nodes": [{ "slug": "incident-triage-agent" }] }
            }] }
        }]),
        "Beatrice's own organization, and only it, is visible through the same fields"
    );
}

#[tokio::test]
#[ignore]
async fn generated_organization_by_id_returns_the_row_for_a_member() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let query = format!(
        "{{ organizations(filters: {{ id: {{ eq: \"{PRODUCT}\" }} }}) {{ nodes {{ id slug displayName lifecycleStatus }} }} }}"
    );
    let data = generated(&router, &cookie, &query).await;

    assert_eq!(
        data["organizations"]["nodes"],
        serde_json::json!([{
            "id": PRODUCT,
            "slug": "product",
            "displayName": "Product",
            "lifecycleStatus": "ACTIVE"
        }])
    );
}

/// Ada's SRE membership ended, so nothing under SRE is reachable from any root, by id or by slug.
#[tokio::test]
#[ignore]
async fn generated_reads_hide_everything_under_an_organization_whose_membership_ended() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let data = generated(
        &router,
        &cookie,
        r#"{ organizations(filters: { slug: { eq: "sre" } }) { nodes { id } }
             projects(filters: { id: { eq: "50000000-0000-0000-0000-000000000003" } }) { nodes { id } }
             agents(filters: { slug: { eq: "incident-triage-agent" } }) { nodes { id } } }"#,
    )
    .await;

    for root in ["organizations", "projects", "agents"] {
        assert_eq!(data[root]["nodes"], serde_json::json!([]), "{root}");
    }
}

#[tokio::test]
#[ignore]
async fn generated_organization_projects_default_to_every_lifecycle_status_ordered_by_display_name()
{
    let router = build_test_router().await;
    let _guard = lock_product_projects().await;
    let cookie = authenticated_cookie(&router).await;

    let query = format!(
        "{{ organizations(filters: {{ id: {{ eq: \"{PRODUCT}\" }} }}) {{ nodes {{ \
            projects(orderBy: {{ displayName: ASC }}) {{ nodes {{ slug lifecycleStatus }} paginationInfo {{ total }} }} }} }} }}"
    );
    let data = generated(&router, &cookie, &query).await;

    let projects = &data["organizations"]["nodes"][0]["projects"];
    assert_eq!(
        projects["nodes"],
        serde_json::json!([
            { "slug": "customer-feedback-copilot", "lifecycleStatus": "ACTIVE" },
            { "slug": "usage-analytics", "lifecycleStatus": "ARCHIVED" }
        ])
    );
    assert_eq!(projects["paginationInfo"]["total"], 2);
}

#[tokio::test]
#[ignore]
async fn generated_organization_projects_lifecycle_filter_narrows_the_result() {
    let router = build_test_router().await;
    let _guard = lock_product_projects().await;
    let cookie = authenticated_cookie(&router).await;

    let query = format!(
        "{{ organizations(filters: {{ id: {{ eq: \"{PRODUCT}\" }} }}) {{ nodes {{ \
            projects(filters: {{ lifecycleStatus: {{ eq: \"ARCHIVED\" }} }}) {{ nodes {{ slug }} paginationInfo {{ total }} }} }} }} }}"
    );
    let data = generated(&router, &cookie, &query).await;

    let projects = &data["organizations"]["nodes"][0]["projects"];
    assert_eq!(slugs(projects), vec!["usage-analytics"]);
    assert_eq!(projects["paginationInfo"]["total"], 1);
}

#[tokio::test]
#[ignore]
async fn generated_organization_projects_search_matches_a_substring_case_insensitively() {
    let router = build_test_router().await;
    let _guard = lock_product_projects().await;
    let cookie = authenticated_cookie(&router).await;

    let query = format!(
        "{{ organizations(filters: {{ id: {{ eq: \"{PRODUCT}\" }} }}) {{ nodes {{ \
            projects(filters: {{ displayName: {{ ilike: \"%feedback%\" }} }}) {{ nodes {{ slug }} paginationInfo {{ total }} }} }} }} }}"
    );
    let data = generated(&router, &cookie, &query).await;

    let projects = &data["organizations"]["nodes"][0]["projects"];
    assert_eq!(slugs(projects), vec!["customer-feedback-copilot"]);
    assert_eq!(projects["paginationInfo"]["total"], 1);
}

#[tokio::test]
#[ignore]
async fn generated_organization_projects_page_pagination_reports_totals_in_either_order() {
    let router = build_test_router().await;
    let _guard = lock_product_projects().await;
    let cookie = authenticated_cookie(&router).await;

    let query = format!(
        "{{ organizations(filters: {{ id: {{ eq: \"{PRODUCT}\" }} }}) {{ nodes {{ \
            first: projects(orderBy: {{ displayName: ASC }}, pagination: {{ page: {{ limit: 1, page: 0 }} }}) {{ \
                nodes {{ slug }} paginationInfo {{ total pages }} pageInfo {{ hasNextPage hasPreviousPage }} }} \
            second: projects(orderBy: {{ displayName: ASC }}, pagination: {{ page: {{ limit: 1, page: 1 }} }}) {{ \
                nodes {{ slug }} paginationInfo {{ total pages }} pageInfo {{ hasNextPage hasPreviousPage }} }} \
            descending: projects(orderBy: {{ displayName: DESC }}) {{ nodes {{ slug }} }} }} }} }}"
    );
    let data = generated(&router, &cookie, &query).await;

    let organization = &data["organizations"]["nodes"][0];
    assert_eq!(
        slugs(&organization["first"]),
        vec!["customer-feedback-copilot"]
    );
    assert_eq!(slugs(&organization["second"]), vec!["usage-analytics"]);
    for page in ["first", "second"] {
        assert_eq!(organization[page]["paginationInfo"]["total"], 2, "{page}");
        assert_eq!(organization[page]["paginationInfo"]["pages"], 2, "{page}");
    }
    assert_eq!(organization["first"]["pageInfo"]["hasNextPage"], true);
    assert_eq!(organization["first"]["pageInfo"]["hasPreviousPage"], false);
    assert_eq!(organization["second"]["pageInfo"]["hasNextPage"], false);
    assert_eq!(organization["second"]["pageInfo"]["hasPreviousPage"], true);
    assert_eq!(
        slugs(&organization["descending"]),
        vec!["usage-analytics", "customer-feedback-copilot"]
    );
}

// A project by id and its agents relation: project 50000000-...-0001 (Customer Feedback
// Copilot, in organization 0001/Product) owns three agents: 60000000-...-0001 (Feedback
// Triage Agent, ACTIVE), 60000000-...-0002 (Sentiment Analyst, DEPRECATED),
// 60000000-...-0003 (Feedback Digest Scribe, ARCHIVED). No agent_versions are seeded.

#[tokio::test]
#[ignore]
async fn generated_project_by_id_is_hidden_from_a_principal_who_is_not_a_member() {
    let router = build_test_router().await;
    let _guard = lock_project_agents().await;
    let cookie = authenticated_cookie_for(&router, BEATRICE).await;

    let query = format!(
        "{{ projects(filters: {{ id: {{ eq: \"{CUSTOMER_FEEDBACK_COPILOT}\" }} }}) {{ nodes {{ id }} }} \
            agents(filters: {{ projectId: {{ eq: \"{CUSTOMER_FEEDBACK_COPILOT}\" }} }}) {{ nodes {{ id }} }} }}"
    );
    let data = generated(&router, &cookie, &query).await;

    assert_eq!(data["projects"]["nodes"], serde_json::json!([]));
    assert_eq!(data["agents"]["nodes"], serde_json::json!([]));
}

#[tokio::test]
#[ignore]
async fn generated_project_agents_are_ordered_by_display_name_with_no_published_versions() {
    let router = build_test_router().await;
    let _guard = lock_project_agents().await;
    let cookie = authenticated_cookie(&router).await;

    let query = format!(
        "{{ projects(filters: {{ id: {{ eq: \"{CUSTOMER_FEEDBACK_COPILOT}\" }} }}) {{ nodes {{ \
            agents(orderBy: {{ displayName: ASC }}) {{ paginationInfo {{ total }} \
                nodes {{ slug lifecycleStatus agentVersions {{ nodes {{ versionNumber }} }} }} }} }} }} }}"
    );
    let data = generated(&router, &cookie, &query).await;

    let agents = &data["projects"]["nodes"][0]["agents"];
    assert_eq!(
        agents["nodes"],
        serde_json::json!([
            { "slug": "feedback-digest-scribe", "lifecycleStatus": "ARCHIVED", "agentVersions": { "nodes": [] } },
            { "slug": "feedback-triage-agent", "lifecycleStatus": "ACTIVE", "agentVersions": { "nodes": [] } },
            { "slug": "sentiment-analyst", "lifecycleStatus": "DEPRECATED", "agentVersions": { "nodes": [] } }
        ])
    );
    assert_eq!(agents["paginationInfo"]["total"], 3);
}

#[tokio::test]
#[ignore]
async fn generated_project_agents_lifecycle_and_search_filters_narrow_the_result() {
    let router = build_test_router().await;
    let _guard = lock_project_agents().await;
    let cookie = authenticated_cookie(&router).await;

    let query = format!(
        "{{ projects(filters: {{ id: {{ eq: \"{CUSTOMER_FEEDBACK_COPILOT}\" }} }}) {{ nodes {{ \
            unarchived: agents(filters: {{ lifecycleStatus: {{ ne: \"ARCHIVED\" }} }}, orderBy: {{ displayName: ASC }}) {{ nodes {{ slug }} }} \
            searched: agents(filters: {{ displayName: {{ ilike: \"%TRIAGE%\" }} }}) {{ nodes {{ slug }} }} \
            both: agents(filters: {{ displayName: {{ ilike: \"%feedback%\" }}, lifecycleStatus: {{ eq: \"ARCHIVED\" }} }}) {{ nodes {{ slug }} }} }} }} }}"
    );
    let data = generated(&router, &cookie, &query).await;

    let project = &data["projects"]["nodes"][0];
    assert_eq!(
        slugs(&project["unarchived"]),
        vec!["feedback-triage-agent", "sentiment-analyst"]
    );
    assert_eq!(slugs(&project["searched"]), vec!["feedback-triage-agent"]);
    assert_eq!(slugs(&project["both"]), vec!["feedback-digest-scribe"]);
}

/// The whole chain, organization → projects → agents → agentVersions: the latest published
/// version and its model are the first row of the versions relation ordered by number, and a
/// principal outside the organization sees none of those versions.
#[tokio::test]
#[ignore]
async fn generated_agent_versions_report_the_highest_numbered_published_version_and_its_model() {
    use hive_persistence::entity::agent_versions;
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

    let router = build_test_router().await;
    let _guard = lock_project_agents().await;
    let db = sea_orm::Database::connect(test_database_url())
        .await
        .expect("connect to test database");
    let agent_id = uuid::Uuid::parse_str(FEEDBACK_TRIAGE_AGENT).unwrap();
    let version_ids = [
        uuid::Uuid::parse_str("99999999-1000-0000-0000-000000000001").unwrap(),
        uuid::Uuid::parse_str("99999999-1000-0000-0000-000000000002").unwrap(),
    ];
    agent_versions::Entity::delete_many()
        .filter(agent_versions::Column::Id.is_in(version_ids))
        .exec(&db)
        .await
        .unwrap();
    for (id, version_number, model, digest) in [
        (version_ids[0], 1, "anthropic/claude-sonnet", "a"),
        (version_ids[1], 2, "anthropic/claude-opus", "c"),
    ] {
        agent_versions::ActiveModel {
            id: Set(id),
            agent_id: Set(agent_id),
            version_number: Set(version_number),
            canonical_document: Set(serde_json::json!({ "model": { "reference": model } })),
            content_digest: Set(digest.repeat(64)),
            dependency_versions: Set(serde_json::json!({})),
            catalog_release_id: Set("local-2026-08-10".to_string()),
            catalog_release_digest: Set("b".repeat(64)),
            published_by: Set(
                uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            ),
            published_at: Set(chrono::Utc::now().into()),
        }
        .insert(&db)
        .await
        .unwrap();
    }

    let member = authenticated_cookie(&router).await;
    let query = format!(
        "{{ organizations(filters: {{ id: {{ eq: \"{PRODUCT}\" }} }}) {{ nodes {{ \
            projects(filters: {{ id: {{ eq: \"{CUSTOMER_FEEDBACK_COPILOT}\" }} }}) {{ nodes {{ \
              agents(filters: {{ displayName: {{ ilike: \"%triage%\" }} }}) {{ nodes {{ slug \
                agentVersions(orderBy: {{ versionNumber: DESC }}, pagination: {{ page: {{ limit: 1, page: 0 }} }}) {{ \
                  paginationInfo {{ total }} nodes {{ versionNumber canonicalDocument agents {{ slug }} }} }} }} }} }} }} }} }} }}"
    );
    let seen = generated(&router, &member, &query).await;
    let stranger = authenticated_cookie_for(&router, BEATRICE).await;
    let hidden = generated(
        &router,
        &stranger,
        &format!(
            "{{ agentVersions(filters: {{ agentId: {{ eq: \"{FEEDBACK_TRIAGE_AGENT}\" }} }}) {{ nodes {{ id }} }} }}"
        ),
    )
    .await;

    agent_versions::Entity::delete_many()
        .filter(agent_versions::Column::Id.is_in(version_ids))
        .exec(&db)
        .await
        .unwrap();

    let agent = &seen["organizations"]["nodes"][0]["projects"]["nodes"][0]["agents"]["nodes"][0];
    assert_eq!(agent["slug"], "feedback-triage-agent");
    let versions = &agent["agentVersions"];
    assert_eq!(versions["paginationInfo"]["total"], 2, "{versions:?}");
    assert_eq!(versions["nodes"].as_array().unwrap().len(), 1);
    let latest = &versions["nodes"][0];
    assert_eq!(latest["versionNumber"], 2);
    assert_eq!(
        latest["canonicalDocument"]["model"]["reference"],
        "anthropic/claude-opus"
    );
    assert_eq!(latest["agents"]["slug"], "feedback-triage-agent");
    assert_eq!(hidden["agentVersions"]["nodes"], serde_json::json!([]));
}

#[tokio::test]
#[ignore]
async fn generated_organization_projects_expose_their_agents_through_the_relation() {
    let router = build_test_router().await;
    let _guard = lock_project_agents().await;
    let _guard = lock_product_projects().await;
    let cookie = authenticated_cookie(&router).await;

    let query = format!(
        "{{ organizations(filters: {{ id: {{ eq: \"{PRODUCT}\" }} }}) {{ nodes {{ \
            projects(orderBy: {{ displayName: ASC }}, pagination: {{ page: {{ limit: 1, page: 0 }} }}) {{ \
              nodes {{ slug agents {{ paginationInfo {{ total }} }} }} }} }} }} }}"
    );
    let data = generated(&router, &cookie, &query).await;

    let node = &data["organizations"]["nodes"][0]["projects"]["nodes"][0];
    assert_eq!(node["slug"], "customer-feedback-copilot");
    assert_eq!(node["agents"]["paginationInfo"]["total"], 3);
}

// The console context is generated reads: `principals` (the requester only), `organizations` with
// their `projects`, and the computed `capabilities` field on each (the codes the requesting
// principal holds at that scope). Ada is ORGANIZATION_ADMIN on Product (10000000-...-0001), which
// owns an ACTIVE project (Customer Feedback Copilot) and an ARCHIVED one (Usage Analytics), and
// plain ORGANIZATION_MEMBER on Support and Quality Assurance. Beatrice belongs to none of them.

const ADA: &str = "00000000-0000-0000-0000-000000000001";
const USAGE_ANALYTICS: &str = "50000000-0000-0000-0000-000000000002";
const CONSOLE_CONTEXT: &str = "{ principals { nodes { id displayName capabilities } } \
    organizations(orderBy: { displayName: ASC, id: ASC }) { nodes { id capabilities \
      projects(orderBy: { displayName: ASC, id: ASC }) { nodes { id capabilities } } } } }";

fn codes(node: &serde_json::Value) -> Vec<&str> {
    node["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|code| code.as_str().unwrap())
        .collect()
}

fn node_with_id<'a>(connection: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    connection["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == id)
        .unwrap_or_else(|| panic!("{id} is not in {connection:?}"))
}

#[tokio::test]
#[ignore]
async fn computed_capabilities_report_organization_admin_rights_and_preferences_update() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let data = generated(&router, &cookie, CONSOLE_CONTEXT).await;

    let principals = data["principals"]["nodes"].as_array().unwrap();
    assert_eq!(principals.len(), 1, "a principal reads only itself");
    assert_eq!(principals[0]["id"], ADA);
    assert_eq!(principals[0]["displayName"], "Ada Lovelace");
    assert_eq!(codes(&principals[0]), ["PREFERENCES.UPDATE"]);

    let product = node_with_id(&data["organizations"], PRODUCT);
    let product_codes = codes(product);
    assert!(product_codes.contains(&"ORGANIZATION.VIEW"));
    assert!(product_codes.contains(&"ORGANIZATION.UPDATE"));
    let mut sorted = product_codes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(product_codes, sorted, "codes are sorted and distinct");

    let copilot = node_with_id(&product["projects"], CUSTOMER_FEEDBACK_COPILOT);
    let copilot_codes = codes(copilot);
    for code in ["PROJECT.VIEW", "AGENT.VIEW", "PROJECT_MEMBERSHIP.ADD"] {
        assert!(copilot_codes.contains(&code), "{code}: {copilot_codes:?}");
    }
}

#[tokio::test]
#[ignore]
async fn computed_capabilities_differ_for_a_plain_member_and_hide_other_tenants() {
    let router = build_test_router().await;
    let ada = authenticated_cookie(&router).await;
    let data = generated(&router, &ada, CONSOLE_CONTEXT).await;
    let organizations = data["organizations"]["nodes"].as_array().unwrap();
    let administered = codes(node_with_id(&data["organizations"], PRODUCT));
    let plain = organizations
        .iter()
        .find(|node| node["id"] != PRODUCT)
        .expect("Ada is a plain member of a second organization");
    let plain_codes = codes(plain);
    assert!(plain_codes.contains(&"ORGANIZATION.VIEW"));
    assert!(
        !plain_codes.contains(&"ORGANIZATION.UPDATE"),
        "a plain member does not administer the organization: {plain_codes:?}"
    );
    assert_ne!(administered, plain_codes);

    // Beatrice is not a member of Product: she sees neither it nor its projects, reads only her
    // own principal row, and holds nothing there beyond her own preferences.
    let beatrice = authenticated_cookie_for(&router, BEATRICE).await;
    let other = generated(&router, &beatrice, CONSOLE_CONTEXT).await;
    let visible: Vec<&str> = other["organizations"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["id"].as_str().unwrap())
        .collect();
    assert!(!visible.contains(&PRODUCT), "{visible:?}");
    let principals = other["principals"]["nodes"].as_array().unwrap();
    assert_eq!(principals.len(), 1);
    assert_eq!(principals[0]["id"], BEATRICE);
    assert_eq!(codes(&principals[0]), ["PREFERENCES.UPDATE"]);
    let hidden = generated(
        &router,
        &beatrice,
        &format!("{{ principals(filters: {{ id: {{ eq: \"{ADA}\" }} }}) {{ nodes {{ id }} }} }}"),
    )
    .await;
    assert_eq!(hidden["principals"]["nodes"], serde_json::json!([]));
}

#[tokio::test]
#[ignore]
async fn computed_capabilities_deny_membership_writes_on_an_archived_project_even_for_an_organization_admin(
) {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let data = generated(&router, &cookie, CONSOLE_CONTEXT).await;

    let product = node_with_id(&data["organizations"], PRODUCT);
    let archived_project_codes = codes(node_with_id(&product["projects"], USAGE_ANALYTICS));

    assert!(archived_project_codes.contains(&"PROJECT_MEMBERSHIP.VIEW"));
    assert!(
        !archived_project_codes.contains(&"PROJECT_MEMBERSHIP.ADD"),
        "PROJECT_MEMBERSHIP.ADD requires an active project even when inherited from organization admin"
    );
}

// Display preferences are the generated `principalDisplayPreferences` field, scoped to the
// requester's own row. A principal that never saved has no row; the console applies its defaults.

const DISPLAY_PREFERENCES: &str =
    "{ principalDisplayPreferences { nodes { principalId colorScheme density sidebarState } } }";

async fn delete_preferences(principal: &str) {
    use hive_persistence::entity::principal_display_preferences;
    use sea_orm::EntityTrait;

    let db = sea_orm::Database::connect(test_database_url())
        .await
        .unwrap();
    principal_display_preferences::Entity::delete_by_id(uuid::Uuid::parse_str(principal).unwrap())
        .exec(&db)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn display_preferences_have_no_row_before_any_write() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, BEATRICE).await;
    let data = generated(&router, &cookie, DISPLAY_PREFERENCES).await;
    assert_eq!(
        data["principalDisplayPreferences"]["nodes"],
        serde_json::json!([])
    );
}

#[tokio::test]
#[ignore]
async fn update_display_preferences_persists_and_is_read_back_by_its_owner_only() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let mutation = "mutation { updateDisplayPreferences(input: { colorScheme: \"DARK\", density: \"COMPACT\", sidebarState: \"COLLAPSED\" }) \
        { displayPreferences { colorScheme density sidebarState } problems { code message } } }";
    let body = graphql_as(&router, &cookie, mutation).await;
    let payload = &body["data"]["updateDisplayPreferences"];
    assert_eq!(payload["problems"].as_array().unwrap().len(), 0);
    assert_eq!(payload["displayPreferences"]["colorScheme"], "DARK");

    // A second write updates the same row in place.
    let again = "mutation { updateDisplayPreferences(input: { colorScheme: \"SYSTEM\", density: \"COMPACT\", sidebarState: \"COLLAPSED\" }) \
        { displayPreferences { colorScheme } problems { code } } }";
    let body = graphql_as(&router, &cookie, again).await;
    assert_eq!(
        body["data"]["updateDisplayPreferences"]["displayPreferences"]["colorScheme"],
        "SYSTEM"
    );

    let read_back = generated(&router, &cookie, DISPLAY_PREFERENCES).await;
    let stranger = authenticated_cookie_for(&router, BEATRICE).await;
    let hidden = generated(&router, &stranger, DISPLAY_PREFERENCES).await;
    delete_preferences(ADA).await;

    assert_eq!(
        read_back["principalDisplayPreferences"]["nodes"],
        serde_json::json!([{
            "principalId": ADA,
            "colorScheme": "SYSTEM",
            "density": "COMPACT",
            "sidebarState": "COLLAPSED"
        }])
    );
    assert_eq!(
        hidden["principalDisplayPreferences"]["nodes"],
        serde_json::json!([]),
        "preferences are visible to their own principal only"
    );
}

#[tokio::test]
#[ignore]
async fn update_display_preferences_rejects_an_unrecognized_color_scheme_without_writing() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, BEATRICE).await;

    let mutation = "mutation { updateDisplayPreferences(input: { colorScheme: \"NEON\", density: \"COMPACT\", sidebarState: \"COLLAPSED\" }) \
        { displayPreferences { colorScheme } problems { code message } } }";
    let body = graphql_as(&router, &cookie, mutation).await;
    let payload = &body["data"]["updateDisplayPreferences"];
    assert_eq!(payload["displayPreferences"], serde_json::Value::Null);
    assert_eq!(payload["problems"][0]["code"], "INVALID_PREFERENCES");

    let read_back = generated(&router, &cookie, DISPLAY_PREFERENCES).await;
    assert_eq!(
        read_back["principalDisplayPreferences"]["nodes"],
        serde_json::json!([])
    );
}

// The agent operational view is the generated `agentOperationalViewProjection` field over the
// view of the same name, scoped through the agent's organization like `agents`.

#[tokio::test]
#[ignore]
async fn generated_agent_operational_view_is_scoped_to_members_and_joins_its_agent() {
    let router = build_test_router().await;
    let query = format!(
        "{{ agentOperationalViewProjection(filters: {{ projectId: {{ eq: \"{CUSTOMER_FEEDBACK_COPILOT}\" }}, agentId: {{ eq: \"{FEEDBACK_TRIAGE_AGENT}\" }} }}) {{ nodes {{ \
            agentId projectId slug displayName lifecycleStatus draftValidationStatus draftErrorCount draftWarningCount \
            publishedVersionStatus aliasTargetCount activeAliasTargetCount deploymentStatus evaluationOutcome \
            runtimeHealth runtimeObservedAt runtimeFreshness agents {{ slug }} }} }} }}"
    );
    let member = authenticated_cookie(&router).await;
    let seen = generated(&router, &member, &query).await;
    let nodes = seen["agentOperationalViewProjection"]["nodes"]
        .as_array()
        .unwrap();
    assert_eq!(nodes.len(), 1, "{seen:?}");
    assert_eq!(nodes[0]["agentId"], FEEDBACK_TRIAGE_AGENT);
    assert_eq!(nodes[0]["slug"], "feedback-triage-agent");
    assert_eq!(nodes[0]["agents"]["slug"], "feedback-triage-agent");
    for column in [
        "draftValidationStatus",
        "publishedVersionStatus",
        "deploymentStatus",
        "evaluationOutcome",
        "runtimeHealth",
        "runtimeFreshness",
    ] {
        assert!(nodes[0][column].is_string(), "{column}: {:?}", nodes[0]);
    }

    let through_agent = generated(
        &router,
        &member,
        &format!(
            "{{ agents(filters: {{ id: {{ eq: \"{FEEDBACK_TRIAGE_AGENT}\" }} }}) {{ nodes {{ agentOperationalViewProjection {{ slug }} }} }} }}"
        ),
    )
    .await;
    assert_eq!(
        through_agent["agents"]["nodes"][0]["agentOperationalViewProjection"]["slug"],
        "feedback-triage-agent"
    );

    let stranger = authenticated_cookie_for(&router, BEATRICE).await;
    let hidden = generated(&router, &stranger, &query).await;
    assert_eq!(
        hidden["agentOperationalViewProjection"]["nodes"],
        serde_json::json!([])
    );
}

// The project dashboard is the generated `projectDashboardProjection` field over the
// `project_dashboard_projection` view. Project 50000000-...-0001 (Customer Feedback Copilot, in
// Product) has a seeded project_dashboard_metrics row with AVAILABLE cost.

const PROJECT_DASHBOARD: &str = "{ projectDashboardProjection(filters: { projectId: { eq: \"50000000-0000-0000-0000-000000000001\" } }) { nodes { \
    projectId slug lifecycleStatus activeAgents activeDeployments failedDeployments pendingApprovals unhealthyResources \
    costAvailability costCurrency currentPeriodCostCents costPeriodStart costPeriodEnd costDataAsOf \
    projects { displayName } } } }";

#[tokio::test]
#[ignore]
async fn project_dashboard_reports_the_seeded_metrics_and_available_cost() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(&router, &cookie, PROJECT_DASHBOARD).await;
    assert_eq!(body["errors"], serde_json::Value::Null);
    let nodes = body["data"]["projectDashboardProjection"]["nodes"]
        .as_array()
        .unwrap();
    assert_eq!(nodes.len(), 1);
    let dashboard = &nodes[0];
    assert_eq!(
        dashboard["projectId"],
        "50000000-0000-0000-0000-000000000001"
    );
    assert_eq!(dashboard["slug"], "customer-feedback-copilot");
    assert_eq!(dashboard["lifecycleStatus"], "ACTIVE");
    assert_eq!(dashboard["activeAgents"], 3);
    assert_eq!(dashboard["activeDeployments"], 2);
    assert_eq!(dashboard["failedDeployments"], 1);
    assert_eq!(dashboard["pendingApprovals"], 4);
    assert_eq!(dashboard["unhealthyResources"], 1);
    assert_eq!(dashboard["costAvailability"], "AVAILABLE");
    assert_eq!(dashboard["costCurrency"], "USD");
    assert_eq!(dashboard["currentPeriodCostCents"], 12345);
    // `timestamp_rfc3339` is on, so generated timestamps are RFC 3339.
    for field in ["costPeriodStart", "costPeriodEnd", "costDataAsOf"] {
        let value = dashboard[field].as_str().unwrap();
        assert!(
            chrono::DateTime::parse_from_rfc3339(value).is_ok(),
            "{field} = {value}"
        );
    }
    // The relation back to the project the row summarizes.
    assert_eq!(
        dashboard["projects"]["displayName"],
        "Customer Feedback Copilot"
    );
}

#[tokio::test]
#[ignore]
async fn project_dashboard_has_no_row_for_a_principal_outside_the_organization() {
    let router = build_test_router().await;
    // Beatrice is a member of SRE only, not of Product; the stranger is a member of none.
    for principal in [
        "00000000-0000-0000-0000-000000000002",
        "99999999-9999-9999-9999-999999999999",
    ] {
        let cookie = authenticated_cookie_for(&router, principal).await;
        let body = graphql_as(&router, &cookie, PROJECT_DASHBOARD).await;
        assert_eq!(body["errors"], serde_json::Value::Null, "{principal}");
        assert_eq!(
            body["data"]["projectDashboardProjection"]["nodes"],
            serde_json::json!([]),
            "{principal}"
        );
    }
    // Unfiltered, the stranger still reads nothing: the scope is the hook's, not the filter's.
    let stranger = authenticated_cookie_for(&router, "99999999-9999-9999-9999-999999999999").await;
    let body = graphql_as(
        &router,
        &stranger,
        "{ projectDashboardProjection { nodes { projectId } } }",
    )
    .await;
    assert_eq!(
        body["data"]["projectDashboardProjection"]["nodes"],
        serde_json::json!([])
    );
}

#[tokio::test]
#[ignore]
async fn project_dashboard_has_no_row_for_a_nonexistent_project() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(
        &router,
        &cookie,
        "{ projectDashboardProjection(filters: { projectId: { eq: \"50000000-0000-0000-0000-000000000099\" } }) { nodes { projectId } } }",
    )
    .await;
    assert_eq!(body["errors"], serde_json::Value::Null);
    assert_eq!(
        body["data"]["projectDashboardProjection"]["nodes"],
        serde_json::json!([])
    );
}

// organizationAdministration/projectAdministration: Ada (00000000-...-0001) is
// ORGANIZATION_ADMIN of 10000000-...-0001 (Product); project 50000000-...-0001
// (Customer Feedback Copilot, in Product) has a seeded budget policy and P-05
// approval policy (organization-project-administration.sql).

#[tokio::test]
#[ignore]
async fn organization_administration_reports_memberships_and_admin_capabilities_for_an_admin() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(
        &router,
        &cookie,
        "{ organizationAdministration(id: \"10000000-0000-0000-0000-000000000001\") { \
            slug displayName lifecycleStatus assignableRoles capabilities \
            memberships { displayName roleCodes projectAccessSummary } \
            availablePrincipals { displayName } } }",
    )
    .await;
    let organization = &body["data"]["organizationAdministration"];
    assert_eq!(organization["slug"], "product");
    assert_eq!(organization["lifecycleStatus"], "ACTIVE");
    let capabilities: Vec<&str> = organization["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert!(capabilities.contains(&"ORGANIZATION_MEMBERSHIP.ADD"));
    let membership = organization["memberships"]
        .as_array()
        .unwrap()
        .iter()
        .find(|membership| membership["displayName"] == "Ada Lovelace")
        .unwrap();
    assert_eq!(membership["roleCodes"][0], "ORGANIZATION_ADMIN");
    assert_eq!(
        membership["projectAccessSummary"][0],
        "All organization projects (ORGANIZATION_ADMIN)"
    );
}

#[tokio::test]
#[ignore]
async fn organization_administration_is_null_for_an_organization_the_principal_cannot_see() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;
    let body = graphql_as(
        &router,
        &cookie,
        "{ organizationAdministration(id: \"10000000-0000-0000-0000-000000000001\") { id } }",
    )
    .await;
    assert_eq!(
        body["data"]["organizationAdministration"],
        serde_json::Value::Null
    );
}

#[tokio::test]
#[ignore]
async fn project_administration_reports_budget_and_approval_policy() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(
        &router,
        &cookie,
        "{ projectAdministration(id: \"50000000-0000-0000-0000-000000000001\") { \
            slug displayName assignableRoles \
            budgetPolicy { currency monthlyLimitCents warningThresholdCents } \
            budgetStatus { state currency } \
            approvalPolicy { matrix { cell requiredEvidence requiredApprovers } } \
            connections { id } } }",
    )
    .await;
    let project = &body["data"]["projectAdministration"];
    assert_eq!(project["slug"], "customer-feedback-copilot");
    assert_eq!(project["budgetPolicy"]["currency"], "USD");
    assert_eq!(project["budgetPolicy"]["monthlyLimitCents"], 500000);
    assert_eq!(project["budgetStatus"]["state"], "NORMAL");
    let matrix = project["approvalPolicy"]["matrix"].as_array().unwrap();
    assert!(matrix
        .iter()
        .any(|rule| rule["cell"] == "PRODUCTION_HIGH" && rule["requiredApprovers"] == 2));
    assert_eq!(project["connections"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[ignore]
async fn project_administration_is_null_for_a_project_the_principal_cannot_see() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;
    let body = graphql_as(
        &router,
        &cookie,
        "{ projectAdministration(id: \"50000000-0000-0000-0000-000000000001\") { id } }",
    )
    .await;
    assert_eq!(
        body["data"]["projectAdministration"],
        serde_json::Value::Null
    );
}

// Administration mutations. Ada (00000000-...-0001) is ORGANIZATION_ADMIN of
// 10000000-...-0001 (Product) only, so a mutation test that creates state
// scopes it to a throwaway project under Product and deletes every row it
// wrote afterward — Product's project count is asserted exactly elsewhere
// (`organization_projects_defaults_to_every_lifecycle_status_ordered_by_display_name`).
// The refusal-path tests below write nothing, so they run directly against
// the shared seeded project 50000000-...-0001 (Customer Feedback Copilot).

async fn delete_administration_test_project(pool: &sqlx::PgPool, project_id: &str) {
    for statement in [
        "DELETE FROM administration_audit_events WHERE scope_id = $1::uuid",
        "DELETE FROM project_settings_connections WHERE project_id = $1::uuid",
        "DELETE FROM project_approval_policy_versions WHERE policy_id = $1::uuid",
        "DELETE FROM project_approval_policies WHERE project_id = $1::uuid",
        "DELETE FROM project_budget_policy_versions WHERE project_id = $1::uuid",
        "DELETE FROM project_budget_policies WHERE project_id = $1::uuid",
        "DELETE FROM project_membership_roles WHERE membership_id IN (SELECT id FROM project_memberships WHERE project_id = $1::uuid)",
        "DELETE FROM deployment_approval_principal_project_scopes WHERE project_id = $1::uuid",
        "DELETE FROM project_memberships WHERE project_id = $1::uuid",
        "DELETE FROM projects WHERE id = $1::uuid",
    ] {
        sqlx::query(statement)
            .bind(project_id)
            .execute(pool)
            .await
            .expect("clean up an administration test project");
    }
}

#[tokio::test]
#[ignore]
async fn create_project_add_membership_budget_general_and_connection_round_trip() {
    let _guard = lock_product_projects().await;
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { createProject(input: { organizationId: \"10000000-0000-0000-0000-000000000001\", expectedRevision: 1, \
            slug: \"http-integration-round-trip\", displayName: \"HTTP Integration Round Trip\" }) \
            { project { id revision } problems { code } } }",
    )
    .await;
    assert_eq!(
        create_body["data"]["createProject"]["problems"],
        serde_json::json!([])
    );
    let project_id = create_body["data"]["createProject"]["project"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let add_membership_query = format!(
        "mutation {{ addAdministrationMembership(input: {{ scope: \"PROJECT\", scopeId: \"{project_id}\", \
            principalId: \"00000000-0000-0000-0000-000000000001\", roleCodes: [\"PROJECT_ADMIN\"], expectedScopeRevision: 1 }}) \
            {{ project {{ memberships {{ roleCodes }} }} problems {{ code }} }} }}"
    );
    let add_membership_body = graphql_as(&router, &cookie, &add_membership_query).await;
    assert_eq!(
        add_membership_body["data"]["addAdministrationMembership"]["problems"],
        serde_json::json!([])
    );

    let budget_query = format!(
        "mutation {{ updateProjectBudgetPolicy(input: {{ projectId: \"{project_id}\", expectedRevision: 0, currency: \"USD\", \
            monthlyLimitCents: 100000, warningThresholdCents: 80000, reason: \"round trip\" }}) \
            {{ project {{ budgetPolicy {{ currency monthlyLimitCents }} }} problems {{ code }} }} }}"
    );
    let budget_body = graphql_as(&router, &cookie, &budget_query).await;
    assert_eq!(
        budget_body["data"]["updateProjectBudgetPolicy"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        budget_body["data"]["updateProjectBudgetPolicy"]["project"]["budgetPolicy"]
            ["monthlyLimitCents"],
        100000
    );

    let general_query = format!(
        "mutation {{ updateProjectGeneral(input: {{ projectId: \"{project_id}\", expectedRevision: 1, \
            displayName: \"Renamed\", description: \"updated\" }}) {{ project {{ displayName description }} problems {{ code }} }} }}"
    );
    let general_body = graphql_as(&router, &cookie, &general_query).await;
    assert_eq!(
        general_body["data"]["updateProjectGeneral"]["project"]["displayName"],
        "Renamed"
    );

    let connection_query = format!(
        "mutation {{ saveProjectSettingsConnection(input: {{ projectId: \"{project_id}\", expectedRevision: 0, \
            displayName: \"Primary\", definitionVersion: \"v1\", environment: \"DEVELOPMENT\", credentialStatus: \"UNBOUND\", \
            lifecycleStatus: \"ACTIVE\" }}) {{ project {{ connections {{ displayName revision }} }} problems {{ code }} }} }}"
    );
    let connection_body = graphql_as(&router, &cookie, &connection_query).await;
    assert_eq!(
        connection_body["data"]["saveProjectSettingsConnection"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        connection_body["data"]["saveProjectSettingsConnection"]["project"]["connections"][0]
            ["displayName"],
        "Primary"
    );

    delete_administration_test_project(&pool, &project_id).await;
}

#[tokio::test]
#[ignore]
async fn create_project_is_forbidden_for_a_non_administrator() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;
    let body = graphql_as(
        &router,
        &cookie,
        "mutation { createProject(input: { organizationId: \"10000000-0000-0000-0000-000000000001\", expectedRevision: 1, \
            slug: \"should-not-be-created\", displayName: \"Should Not Be Created\" }) { project { id } problems { code } } }",
    )
    .await;
    assert_eq!(
        body["data"]["createProject"]["project"],
        serde_json::Value::Null
    );
    assert_eq!(
        body["data"]["createProject"]["problems"][0]["code"],
        "FORBIDDEN"
    );
}

#[tokio::test]
#[ignore]
async fn create_project_reports_a_revision_conflict_for_a_stale_expected_revision() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(
        &router,
        &cookie,
        "mutation { createProject(input: { organizationId: \"10000000-0000-0000-0000-000000000001\", expectedRevision: 99, \
            slug: \"stale-revision-attempt\", displayName: \"Stale Revision Attempt\" }) \
            { project { id } problems { code ... on AdministrationRevisionConflict { expectedRevision actualRevision } } } }",
    )
    .await;
    let problem = &body["data"]["createProject"]["problems"][0];
    assert_eq!(problem["code"], "REVISION_CONFLICT");
    assert_eq!(problem["expectedRevision"], 99);
    assert_eq!(problem["actualRevision"], 1);
}

#[tokio::test]
#[ignore]
async fn archive_administration_scope_requires_the_organization_slug_as_confirmation() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(
        &router,
        &cookie,
        "mutation { archiveAdministrationScope(input: { scope: \"ORGANIZATION\", scopeId: \"10000000-0000-0000-0000-000000000001\", \
            expectedRevision: 1, reason: \"should be refused\", confirmation: \"not-the-slug\" }) \
            { organization { lifecycleStatus } problems { code } } }",
    )
    .await;
    assert_eq!(
        body["data"]["archiveAdministrationScope"]["organization"],
        serde_json::Value::Null
    );
    assert_eq!(
        body["data"]["archiveAdministrationScope"]["problems"][0]["code"],
        "PROTECTED_LIFECYCLE"
    );

    let readback = graphql_as(
        &router,
        &cookie,
        "{ organizationAdministration(id: \"10000000-0000-0000-0000-000000000001\") { lifecycleStatus revision } }",
    )
    .await;
    assert_eq!(
        readback["data"]["organizationAdministration"]["lifecycleStatus"],
        "ACTIVE"
    );
    assert_eq!(
        readback["data"]["organizationAdministration"]["revision"],
        1
    );
}

// PROJECT_APPROVAL_POLICY.UPDATE (like PROJECT_BUDGET.UPDATE) is absent from
// INHERITED_ORGANIZATION_ADMIN, so even Ada's ORGANIZATION_ADMIN role on
// Product does not reach it on a project — an explicit PROJECT_ADMIN
// membership is required, hence the same create-plus-cleanup shape as the
// round trip test above rather than reusing a shared seeded project.
#[tokio::test]
#[ignore]
async fn update_project_approval_policy_rejects_a_weakening_change_and_writes_nothing() {
    let _guard = lock_product_projects().await;
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { createProject(input: { organizationId: \"10000000-0000-0000-0000-000000000001\", expectedRevision: 1, \
            slug: \"http-integration-weakening-check\", displayName: \"HTTP Integration Weakening Check\" }) \
            { project { id } problems { code } } }",
    )
    .await;
    assert_eq!(
        create_body["data"]["createProject"]["problems"],
        serde_json::json!([])
    );
    let project_id = create_body["data"]["createProject"]["project"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let add_membership_query = format!(
        "mutation {{ addAdministrationMembership(input: {{ scope: \"PROJECT\", scopeId: \"{project_id}\", \
            principalId: \"00000000-0000-0000-0000-000000000001\", roleCodes: [\"PROJECT_ADMIN\"], expectedScopeRevision: 1 }}) \
            {{ project {{ id }} problems {{ code }} }} }}"
    );
    let add_membership_body = graphql_as(&router, &cookie, &add_membership_query).await;
    assert_eq!(
        add_membership_body["data"]["addAdministrationMembership"]["problems"],
        serde_json::json!([])
    );

    // Weakens DEVELOPMENT_HIGH from the seeded default's 1 required approver to 0.
    let weaker_cell = "{ requiredEvidence: [\"PLAN_VALIDATED\", \"CHANGE_SUMMARY_READY\", \"EVALUATION_PASSED\"], requiredApprovers: 0 }";
    let default_low = "{ requiredEvidence: [\"PLAN_VALIDATED\"], requiredApprovers: 0 }";
    let default_medium = "{ requiredEvidence: [\"PLAN_VALIDATED\", \"CHANGE_SUMMARY_READY\"], requiredApprovers: 0 }";
    let default_high = "{ requiredEvidence: [\"PLAN_VALIDATED\", \"CHANGE_SUMMARY_READY\", \"EVALUATION_PASSED\"], requiredApprovers: 1 }";
    let query = format!(
        "mutation {{ updateProjectApprovalPolicy(input: {{ projectId: \"{project_id}\", expectedRevision: 1, \
            reason: \"weaken test\", matrix: {{ DEVELOPMENT_LOW: {default_low}, DEVELOPMENT_MEDIUM: {default_medium}, DEVELOPMENT_HIGH: {weaker_cell}, \
            STAGING_LOW: {default_medium}, STAGING_MEDIUM: {default_high}, STAGING_HIGH: {default_high}, \
            PRODUCTION_LOW: {default_high}, PRODUCTION_MEDIUM: {default_high}, PRODUCTION_HIGH: {default_high} }} }}) \
            {{ project {{ id }} problems {{ code }} }} }}"
    );
    let body = graphql_as(&router, &cookie, &query).await;
    assert_eq!(
        body["data"]["updateProjectApprovalPolicy"]["project"],
        serde_json::Value::Null
    );
    assert_eq!(
        body["data"]["updateProjectApprovalPolicy"]["problems"][0]["code"],
        "POLICY_WEAKENING"
    );

    let readback_query = format!(
        "{{ projectAdministration(id: \"{project_id}\") {{ approvalPolicy {{ revision }} }} }}"
    );
    let readback = graphql_as(&router, &cookie, &readback_query).await;
    assert_eq!(
        readback["data"]["projectAdministration"]["approvalPolicy"]["revision"],
        1
    );

    delete_administration_test_project(&pool, &project_id).await;
}

// Agent drafts. Ada (00000000-...-0001) holds a console_role_assignments
// AGENT_DEVELOPER grant on project 50000000-...-0001 (Customer Feedback
// Copilot), which AGENT_DRAFT.UPDATE/CREATE/PUBLISH route through
// (`legacy_or_developer`) independently of her ORGANIZATION_ADMIN role on
// Product — neither ORGANIZATION_ADMIN nor plain project visibility grants
// those three capabilities on its own. No agent_drafts/agent_versions rows
// are seeded for any of the four seeded agents, so a read never needs
// cleanup; a write against the shared seeded agent 60000000-...-0001
// (Feedback Triage Agent, ACTIVE) does, and a write that creates a new agent
// additionally needs `lock_project_agents` (this project's agent count and
// slug list are asserted exactly elsewhere).

/// Defensive pre-cleanup: a prior run of the round trip test that panicked
/// before reaching its own cleanup call leaves its fixed slug taken, which
/// would otherwise fail every subsequent run at `createAgentDraft` with
/// `INVALID_DOCUMENT` forever.
async fn delete_agent_draft_test_agent_by_slug(pool: &sqlx::PgPool, project_id: &str, slug: &str) {
    let existing: Option<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM agents WHERE project_id = $1::uuid AND slug = $2")
            .bind(project_id)
            .bind(slug)
            .fetch_optional(pool)
            .await
            .expect("look up a leftover agent draft test agent by slug");
    if let Some((id,)) = existing {
        delete_agent_draft_test_agent(pool, &id.to_string()).await;
    }
}

async fn delete_agent_draft_test_agent(pool: &sqlx::PgPool, agent_id: &str) {
    for statement in [
        "DELETE FROM evaluation_target_projections WHERE target_kind = 'AGENT_VERSION' AND target_id IN (SELECT id FROM agent_versions WHERE agent_id = $1::uuid)",
        "DELETE FROM agent_versions WHERE agent_id = $1::uuid",
        "DELETE FROM agent_authoring_audit_events WHERE agent_id = $1::uuid",
        "DELETE FROM agent_draft_audit_events WHERE agent_id = $1::uuid",
        "DELETE FROM agent_drafts WHERE agent_id = $1::uuid",
        "DELETE FROM agents WHERE id = $1::uuid",
    ] {
        sqlx::query(statement)
            .bind(agent_id)
            .execute(pool)
            .await
            .expect("clean up an agent draft test agent");
    }
}

fn agent_draft_query(project_id: &str, agent_id: &str, fields: &str) -> String {
    format!(
        "{{ agents(filters: {{ id: {{ eq: \"{agent_id}\" }}, projectId: {{ eq: \"{project_id}\" }} }}) \
            {{ nodes {{ slug agentVersions {{ nodes {{ versionNumber }} }} draft {{ {fields} }} }} }} }}"
    )
}

#[tokio::test]
#[ignore]
async fn agent_draft_reports_the_default_document_for_an_agent_with_no_draft_row() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(
        &router,
        &cookie,
        &agent_draft_query(
            "50000000-0000-0000-0000-000000000001",
            "60000000-0000-0000-0000-000000000002",
            "agentId revision validationStatus validationDiagnostics canUpdate canPublish document \
             agents { slug } review { changedSections catalogReleaseId }",
        ),
    )
    .await;
    let agent = &body["data"]["agents"]["nodes"][0];
    assert_eq!(agent["slug"], "sentiment-analyst", "{body:?}");
    assert_eq!(agent["agentVersions"]["nodes"], serde_json::json!([]));
    let draft = &agent["draft"];
    assert_eq!(draft["agentId"], "60000000-0000-0000-0000-000000000002");
    assert_eq!(draft["agents"]["slug"], "sentiment-analyst");
    assert_eq!(draft["revision"], 1);
    assert_eq!(draft["validationStatus"], "NOT_VALIDATED");
    assert_eq!(draft["validationDiagnostics"], serde_json::json!([]));
    // Sentiment Analyst is DEPRECATED, not ACTIVE, so neither write capability applies.
    assert_eq!(draft["canUpdate"], false);
    assert_eq!(draft["canPublish"], false);
    assert_eq!(
        draft["document"]["general"]["displayName"],
        "Sentiment Analyst"
    );
    // The default draft differs from nothing: no version exists to compare it with.
    assert_eq!(draft["review"]["changedSections"], serde_json::json!([]));
    // No stored row is created by reading.
    let stored = graphql_as(
        &router,
        &cookie,
        "{ agentDrafts(filters: { agentId: { eq: \"60000000-0000-0000-0000-000000000002\" } }) { nodes { agentId } } }",
    )
    .await;
    assert_eq!(
        stored["data"]["agentDrafts"]["nodes"],
        serde_json::json!([])
    );
}

#[tokio::test]
#[ignore]
async fn agent_draft_is_absent_for_a_principal_without_project_access() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;
    let body = graphql_as(
        &router,
        &cookie,
        &agent_draft_query(
            "50000000-0000-0000-0000-000000000001",
            "60000000-0000-0000-0000-000000000001",
            "agentId",
        ),
    )
    .await;
    assert_eq!(body["data"]["agents"]["nodes"], serde_json::json!([]));
}

#[tokio::test]
#[ignore]
async fn update_agent_draft_is_forbidden_on_an_archived_agent() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(
        &router,
        &cookie,
        "mutation { updateAgentDraft(input: { projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"60000000-0000-0000-0000-000000000003\", \
            expectedRevision: 1, document: {} }) { agentDraft { agentId } problems { code } } }",
    )
    .await;
    assert_eq!(
        body["data"]["updateAgentDraft"]["agentDraft"],
        serde_json::Value::Null
    );
    assert_eq!(
        body["data"]["updateAgentDraft"]["problems"][0]["code"],
        "FORBIDDEN"
    );
}

#[tokio::test]
#[ignore]
async fn update_agent_draft_rejects_a_non_object_document() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(
        &router,
        &cookie,
        "mutation { updateAgentDraft(input: { projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"60000000-0000-0000-0000-000000000001\", \
            expectedRevision: 1, document: [1, 2, 3] }) { agentDraft { agentId } problems { code } } }",
    )
    .await;
    assert_eq!(
        body["data"]["updateAgentDraft"]["agentDraft"],
        serde_json::Value::Null
    );
    assert_eq!(
        body["data"]["updateAgentDraft"]["problems"][0]["code"],
        "INVALID_DOCUMENT"
    );
}

#[tokio::test]
#[ignore]
async fn create_update_validate_and_publish_agent_draft_round_trip() {
    let _guard = lock_project_agents().await;
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_agent_draft_test_agent_by_slug(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-agent",
    )
    .await;

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { createAgentDraft(input: { projectId: \"50000000-0000-0000-0000-000000000001\", displayName: \"HTTP Integration Agent\" }) \
            { agentDraft { agentId revision canUpdate canPublish agents { slug } } problems { code } } }",
    )
    .await;
    assert_eq!(
        create_body["data"]["createAgentDraft"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        create_body["data"]["createAgentDraft"]["agentDraft"]["canPublish"],
        true
    );
    let agent_id = create_body["data"]["createAgentDraft"]["agentDraft"]["agentId"]
        .as_str()
        .unwrap()
        .to_string();

    let duplicate_body = graphql_as(
        &router,
        &cookie,
        "mutation { createAgentDraft(input: { projectId: \"50000000-0000-0000-0000-000000000001\", displayName: \"Duplicate\", slug: \"http-integration-agent\" }) \
            { agentDraft { agentId } problems { code } } }",
    )
    .await;
    assert_eq!(
        duplicate_body["data"]["createAgentDraft"]["problems"][0]["code"],
        "INVALID_DOCUMENT"
    );

    let update_query = format!(
        "mutation {{ updateAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: 1, \
            document: {{ general: {{ displayName: \"HTTP Integration Agent\" }}, instructions: {{ source: \"Do the thing.\" }}, limits: {{ maxTokens: 4096 }} }} }}) \
            {{ agentDraft {{ revision }} problems {{ code }} }} }}"
    );
    let update_body = graphql_as(&router, &cookie, &update_query).await;
    assert_eq!(
        update_body["data"]["updateAgentDraft"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        update_body["data"]["updateAgentDraft"]["agentDraft"]["revision"],
        2
    );

    let validate_query = format!(
        "mutation {{ validateAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: 2 }}) \
            {{ agentDraft {{ revision validationStatus validationDiagnostics }} problems {{ code }} }} }}"
    );
    let validate_body = graphql_as(&router, &cookie, &validate_query).await;
    assert_eq!(
        validate_body["data"]["validateAgentDraft"]["agentDraft"]["revision"],
        3
    );
    assert_eq!(
        validate_body["data"]["validateAgentDraft"]["agentDraft"]["validationStatus"],
        "VALID"
    );

    let publish_query = format!(
        "mutation {{ publishAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: 3, warningsAcknowledged: true }}) \
            {{ agentDraft {{ revision review {{ changedSections }} }} agentVersion {{ id versionNumber agents {{ slug }} }} problems {{ code }} }} }}"
    );
    let publish_body = graphql_as(&router, &cookie, &publish_query).await;
    assert_eq!(
        publish_body["data"]["publishAgentDraft"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        publish_body["data"]["publishAgentDraft"]["agentVersion"]["versionNumber"],
        1
    );
    // The payload is the generated `AgentVersions` / `AgentDrafts` object: relations and
    // computed fields resolve on it.
    assert_eq!(
        publish_body["data"]["publishAgentDraft"]["agentVersion"]["agents"]["slug"],
        "http-integration-agent"
    );
    assert_eq!(
        publish_body["data"]["publishAgentDraft"]["agentDraft"]["review"]["changedSections"],
        serde_json::json!([])
    );
    let version_id = publish_body["data"]["publishAgentDraft"]["agentVersion"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Idempotent republish: same revision, unchanged content, returns the same version.
    let republish_body = graphql_as(&router, &cookie, &publish_query).await;
    assert_eq!(
        republish_body["data"]["publishAgentDraft"]["agentVersion"]["id"],
        version_id
    );

    let versions_query = format!(
        "{{ agentVersions(filters: {{ agentId: {{ eq: \"{agent_id}\" }} }}) {{ nodes {{ id versionNumber agents {{ projectId }} }} }} }}"
    );
    let versions_body = graphql_as(&router, &cookie, &versions_query).await;
    let versions = versions_body["data"]["agentVersions"]["nodes"]
        .as_array()
        .unwrap();
    assert_eq!(versions.len(), 1, "{versions_body:?}");
    assert_eq!(versions[0]["id"], version_id);
    assert_eq!(
        versions[0]["agents"]["projectId"],
        "50000000-0000-0000-0000-000000000001"
    );

    delete_agent_draft_test_agent(&pool, &agent_id).await;
}

async fn delete_configuration_test_resource_by_identity(
    pool: &sqlx::PgPool,
    project_id: &str,
    identity: &str,
) {
    let existing: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM reusable_resources WHERE project_id = $1::uuid AND identity = $2",
    )
    .bind(project_id)
    .bind(identity)
    .fetch_optional(pool)
    .await
    .expect("look up a leftover configuration test resource by identity");
    if let Some((id,)) = existing {
        for statement in [
            "DELETE FROM configuration_audit_events WHERE subject_id = $1::uuid",
            "DELETE FROM reusable_resource_versions WHERE resource_id = $1::uuid",
            "DELETE FROM reusable_resource_drafts WHERE resource_id = $1::uuid",
            "DELETE FROM reusable_resources WHERE id = $1::uuid",
        ] {
            sqlx::query(statement)
                .bind(id)
                .execute(pool)
                .await
                .expect("clean up a configuration test resource");
        }
    }
}

async fn delete_configuration_test_tool_by_server_id(
    pool: &sqlx::PgPool,
    project_id: &str,
    server_id: &str,
) {
    let existing: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM project_tool_connections WHERE project_id = $1::uuid AND server_id = $2",
    )
    .bind(project_id)
    .bind(server_id)
    .fetch_optional(pool)
    .await
    .expect("look up a leftover configuration test tool by server id");
    if let Some((id,)) = existing {
        for statement in [
            "DELETE FROM configuration_audit_events WHERE subject_id = $1::uuid",
            "DELETE FROM project_tool_connections WHERE id = $1::uuid",
        ] {
            sqlx::query(statement)
                .bind(id)
                .execute(pool)
                .await
                .expect("clean up a configuration test tool");
        }
    }
}

#[tokio::test]
#[ignore]
async fn create_update_validate_and_publish_reusable_resource_round_trip() {
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_configuration_test_resource_by_identity(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-resource",
    )
    .await;

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { createReusableResource(input: { projectId: \"50000000-0000-0000-0000-000000000001\", kind: \"PROMPT\", \
            name: \"HTTP Integration Resource\", content: \"Hello {{name}}\", dependencies: [] }) \
            { resource { id draftRevision validationStatus } problems { code } } }",
    )
    .await;
    assert_eq!(
        create_body["data"]["createReusableResource"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        create_body["data"]["createReusableResource"]["resource"]["draftRevision"],
        1
    );
    let resource_id = create_body["data"]["createReusableResource"]["resource"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let update_query = format!(
        "mutation {{ updateReusableResourceDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", resourceId: \"{resource_id}\", \
            expectedRevision: 1, content: \"Hello {{{{name}}}}, welcome.\", dependencies: [] }}) {{ resource {{ draftRevision }} problems {{ code }} }} }}"
    );
    let update_body = graphql_as(&router, &cookie, &update_query).await;
    assert_eq!(
        update_body["data"]["updateReusableResourceDraft"]["resource"]["draftRevision"],
        2
    );

    let validate_query = format!(
        "mutation {{ validateReusableResource(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", resourceId: \"{resource_id}\", expectedRevision: 2 }}) \
            {{ resource {{ validationStatus diagnostics }} problems {{ code }} }} }}"
    );
    let validate_body = graphql_as(&router, &cookie, &validate_query).await;
    assert_eq!(
        validate_body["data"]["validateReusableResource"]["resource"]["validationStatus"],
        "VALID"
    );

    let publish_query = format!(
        "mutation {{ publishReusableResource(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", resourceId: \"{resource_id}\", expectedRevision: 2 }}) \
            {{ resource {{ publishedVersion }} problems {{ code }} }} }}"
    );
    let publish_body = graphql_as(&router, &cookie, &publish_query).await;
    assert_eq!(
        publish_body["data"]["publishReusableResource"]["resource"]["publishedVersion"],
        1
    );

    // Idempotent republish: same revision, unchanged digest, still version 1.
    let republish_body = graphql_as(&router, &cookie, &publish_query).await;
    assert_eq!(
        republish_body["data"]["publishReusableResource"]["resource"]["publishedVersion"],
        1
    );

    let read_query = format!("{{ reusableResource(projectId: \"50000000-0000-0000-0000-000000000001\", resourceId: \"{resource_id}\") {{ versions {{ version }} }} }}");
    let read_body = graphql_as(&router, &cookie, &read_query).await;
    assert_eq!(
        read_body["data"]["reusableResource"]["versions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    delete_configuration_test_resource_by_identity(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-resource",
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn update_reusable_resource_draft_reports_a_revision_conflict_for_a_stale_expected_revision()
{
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_configuration_test_resource_by_identity(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-stale-resource",
    )
    .await;

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { createReusableResource(input: { projectId: \"50000000-0000-0000-0000-000000000001\", kind: \"PROMPT\", \
            name: \"HTTP Integration Stale Resource\", content: \"hello\", dependencies: [] }) { resource { id } problems { code } } }",
    )
    .await;
    let resource_id = create_body["data"]["createReusableResource"]["resource"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let stale_update_query = format!(
        "mutation {{ updateReusableResourceDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", resourceId: \"{resource_id}\", \
            expectedRevision: 99, content: \"stale\", dependencies: [] }}) {{ resource {{ id }} problems {{ code }} }} }}"
    );
    let stale_body = graphql_as(&router, &cookie, &stale_update_query).await;
    assert_eq!(
        stale_body["data"]["updateReusableResourceDraft"]["resource"],
        serde_json::Value::Null
    );
    assert_eq!(
        stale_body["data"]["updateReusableResourceDraft"]["problems"][0]["code"],
        "REVISION_CONFLICT"
    );

    delete_configuration_test_resource_by_identity(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-stale-resource",
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn create_reusable_resource_rejects_an_unresolvable_dependency() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let body = graphql_as(
        &router,
        &cookie,
        "mutation { createReusableResource(input: { projectId: \"50000000-0000-0000-0000-000000000001\", kind: \"PROMPT\", \
            name: \"HTTP Integration Bad Dependency\", content: \"hello\", dependencies: [\"model:nonexistent@v1\"] }) \
            { resource { id } problems { code } } }",
    )
    .await;
    assert_eq!(
        body["data"]["createReusableResource"]["resource"],
        serde_json::Value::Null
    );
    assert_eq!(
        body["data"]["createReusableResource"]["problems"][0]["code"],
        "INVALID_DRAFT"
    );
}

#[tokio::test]
#[ignore]
async fn update_reusable_resource_draft_is_forbidden_for_a_principal_without_project_access() {
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let owner_cookie = authenticated_cookie(&router).await;
    delete_configuration_test_resource_by_identity(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-forbidden-resource",
    )
    .await;

    let create_body = graphql_as(
        &router,
        &owner_cookie,
        "mutation { createReusableResource(input: { projectId: \"50000000-0000-0000-0000-000000000001\", kind: \"PROMPT\", \
            name: \"HTTP Integration Forbidden Resource\", content: \"hello\", dependencies: [] }) { resource { id } problems { code } } }",
    )
    .await;
    let resource_id = create_body["data"]["createReusableResource"]["resource"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let outsider_cookie =
        authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;
    let update_query = format!(
        "mutation {{ updateReusableResourceDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", resourceId: \"{resource_id}\", \
            expectedRevision: 1, content: \"x\", dependencies: [] }}) {{ resource {{ id }} problems {{ code }} }} }}"
    );
    let outsider_body = graphql_as(&router, &outsider_cookie, &update_query).await;
    assert_eq!(
        outsider_body["data"]["updateReusableResourceDraft"]["resource"],
        serde_json::Value::Null
    );
    assert_eq!(
        outsider_body["data"]["updateReusableResourceDraft"]["problems"][0]["code"],
        "FORBIDDEN"
    );

    delete_configuration_test_resource_by_identity(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-forbidden-resource",
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn create_and_update_project_mcp_server_round_trip() {
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_configuration_test_tool_by_server_id(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-mcp-server",
    )
    .await;

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { createProjectMcpServer(input: { projectId: \"50000000-0000-0000-0000-000000000001\", serverId: \"http-integration-mcp-server\", \
            name: \"HTTP Integration MCP Server\", definition: \"tool:http-metadata@v1\", environment: \"DEVELOPMENT\", enabled: true, \
            transportType: \"STDIO\", command: \"/usr/bin/http-metadata\", arguments: [], remoteUrl: null, \
            redactedBindings: [\"redacted://local/http-integration-token\"], tools: [], resources: [], prompts: [] }) \
            { mcpServer { id revision status } tool { id redactedSecretReference } problems { code } } }",
    )
    .await;
    assert_eq!(
        create_body["data"]["createProjectMcpServer"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        create_body["data"]["createProjectMcpServer"]["mcpServer"]["revision"],
        1
    );
    assert_eq!(
        create_body["data"]["createProjectMcpServer"]["tool"]["redactedSecretReference"],
        "redacted://local/http-integration-token"
    );
    let server_id = create_body["data"]["createProjectMcpServer"]["mcpServer"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let update_query = format!(
        "mutation {{ updateProjectMcpServer(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", id: \"{server_id}\", expectedRevision: 1, \
            name: \"HTTP Integration MCP Server Renamed\", definition: \"tool:http-metadata@v1\", environment: \"DEVELOPMENT\", enabled: true, \
            transportType: \"STDIO\", command: \"/usr/bin/http-metadata\", arguments: [], remoteUrl: null, \
            redactedBindings: [\"redacted://local/http-integration-token\"], tools: [], resources: [], prompts: [], lifecycleStatus: \"ACTIVE\" }}) \
            {{ mcpServer {{ name revision }} problems {{ code }} }} }}"
    );
    let update_body = graphql_as(&router, &cookie, &update_query).await;
    assert_eq!(
        update_body["data"]["updateProjectMcpServer"]["mcpServer"]["name"],
        "HTTP Integration MCP Server Renamed"
    );
    assert_eq!(
        update_body["data"]["updateProjectMcpServer"]["mcpServer"]["revision"],
        2
    );

    let create_remote_rejected = graphql_as(
        &router,
        &cookie,
        "mutation { createProjectMcpServer(input: { projectId: \"50000000-0000-0000-0000-000000000001\", serverId: \"http-integration-mcp-server-rejected\", \
            name: \"HTTP Integration MCP Server Rejected\", definition: \"tool:http-metadata@v1\", environment: \"DEVELOPMENT\", enabled: true, \
            transportType: \"REMOTE\", command: null, arguments: [], remoteUrl: \"https://example.com/mcp?api_key=shhh\", \
            redactedBindings: [\"redacted://local/http-integration-token\"], tools: [], resources: [], prompts: [] }) \
            { mcpServer { id } problems { code } } }",
    )
    .await;
    assert_eq!(
        create_remote_rejected["data"]["createProjectMcpServer"]["mcpServer"],
        serde_json::Value::Null
    );
    assert_eq!(
        create_remote_rejected["data"]["createProjectMcpServer"]["problems"][0]["code"],
        "INVALID_INPUT"
    );

    delete_configuration_test_tool_by_server_id(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-mcp-server",
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn save_project_tool_connection_metadata_creates_and_updates_a_legacy_tool() {
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_configuration_test_tool_by_server_id(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "legacy-http-integration-tool",
    )
    .await;

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { saveProjectToolConnectionMetadata(input: { projectId: \"50000000-0000-0000-0000-000000000001\", toolId: null, expectedRevision: 0, \
            name: \"Legacy HTTP Integration Tool\", definition: \"tool:http-metadata@v1\", environment: \"PRODUCTION\", \
            redactedSecretReference: \"redacted://local/legacy-key\", lifecycleStatus: \"ACTIVE\", rotationSummary: \"Rotated quarterly.\" }) \
            { tool { id name revision lifecycleStatus } problems { code } } }",
    )
    .await;
    assert_eq!(
        create_body["data"]["saveProjectToolConnectionMetadata"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        create_body["data"]["saveProjectToolConnectionMetadata"]["tool"]["revision"],
        1
    );
    let tool_id = create_body["data"]["saveProjectToolConnectionMetadata"]["tool"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let update_query = format!(
        "mutation {{ saveProjectToolConnectionMetadata(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", toolId: \"{tool_id}\", expectedRevision: 1, \
            name: \"Legacy HTTP Integration Tool Renamed\", definition: \"tool:http-metadata@v1\", environment: \"PRODUCTION\", \
            redactedSecretReference: \"redacted://local/legacy-key-2\", lifecycleStatus: \"ARCHIVED\", rotationSummary: \"Rotated again.\" }}) \
            {{ tool {{ name revision lifecycleStatus }} problems {{ code }} }} }}"
    );
    let update_body = graphql_as(&router, &cookie, &update_query).await;
    assert_eq!(
        update_body["data"]["saveProjectToolConnectionMetadata"]["tool"]["name"],
        "Legacy HTTP Integration Tool Renamed"
    );
    assert_eq!(
        update_body["data"]["saveProjectToolConnectionMetadata"]["tool"]["revision"],
        2
    );
    assert_eq!(
        update_body["data"]["saveProjectToolConnectionMetadata"]["tool"]["lifecycleStatus"],
        "ARCHIVED"
    );

    let stale_update_body = graphql_as(&router, &cookie, &update_query).await;
    assert_eq!(
        stale_update_body["data"]["saveProjectToolConnectionMetadata"]["tool"],
        serde_json::Value::Null
    );
    assert_eq!(
        stale_update_body["data"]["saveProjectToolConnectionMetadata"]["problems"][0]["code"],
        "REVISION_CONFLICT"
    );

    delete_configuration_test_tool_by_server_id(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "legacy-http-integration-tool",
    )
    .await;
}

/// Deployment write capabilities (`DEPLOYMENT.REQUEST`/`CANCEL`/`PROMOTE`/`ROLLBACK`) require a
/// project-level `PROJECT_ADMIN`/`AGENT_DEVELOPER`/`OPERATOR` role, unlike agent authoring which an
/// organization admin can already reach: see `deployment_capabilities` in
/// `hive-persistence/src/capability/tx.rs`. Tests below grant this explicitly rather than relying on
/// principal 1's seeded `ORGANIZATION_ADMIN` role, which only grants `DEPLOYMENT.VIEW`.
async fn grant_project_role(
    pool: &sqlx::PgPool,
    project_id: &str,
    principal_id: &str,
    role_code: &str,
) -> uuid::Uuid {
    let membership_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO project_memberships (id, project_id, principal_id, started_at, revision, active_marker) \
         VALUES ($1, $2::uuid, $3::uuid, CURRENT_TIMESTAMP, 1, true)",
    )
    .bind(membership_id)
    .bind(project_id)
    .bind(principal_id)
    .execute(pool)
    .await
    .expect("grant a project membership");
    sqlx::query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, $2)")
        .bind(membership_id)
        .bind(role_code)
        .execute(pool)
        .await
        .expect("grant a project membership role");
    membership_id
}

async fn revoke_project_membership(pool: &sqlx::PgPool, membership_id: uuid::Uuid) {
    sqlx::query("DELETE FROM project_membership_roles WHERE membership_id = $1")
        .bind(membership_id)
        .execute(pool)
        .await
        .expect("revoke a project membership role");
    sqlx::query("DELETE FROM project_memberships WHERE id = $1")
        .bind(membership_id)
        .execute(pool)
        .await
        .expect("revoke a project membership");
}

async fn delete_deployment_test_fixtures(pool: &sqlx::PgPool, agent_id: &str) {
    let deployment_ids: Vec<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM deployments WHERE agent_id = $1::uuid")
            .bind(agent_id)
            .fetch_all(pool)
            .await
            .expect("look up deployment test fixtures");
    for (deployment_id,) in &deployment_ids {
        for statement in [
            "DELETE FROM deployment_stage_events WHERE deployment_attempt_id IN (SELECT id FROM deployment_attempts WHERE deployment_id = $1)",
            "DELETE FROM deployment_attempts WHERE deployment_id = $1",
            "DELETE FROM deployment_audit_events WHERE deployment_id = $1",
            "DELETE FROM deployment_outbox_events WHERE deployment_id = $1",
            "DELETE FROM deployment_evidence_invalidations WHERE evidence_snapshot_id IN (SELECT id FROM deployment_evidence_snapshots WHERE deployment_id = $1)",
            "DELETE FROM deployment_evidence_snapshots WHERE deployment_id = $1",
            "DELETE FROM deployment_approval_decisions WHERE approval_requirement_id IN (SELECT id FROM deployment_approval_requirements WHERE deployment_id = $1)",
            "DELETE FROM deployment_approval_handoff_releases WHERE deployment_id = $1",
            "DELETE FROM deployment_approval_requirements WHERE deployment_id = $1",
            "DELETE FROM deployment_plan_review_facts WHERE plan_id IN (SELECT id FROM deployment_plan_versions WHERE deployment_id = $1)",
            "DELETE FROM deployment_plan_versions WHERE deployment_id = $1",
            "DELETE FROM deployment_policy_snapshots WHERE deployment_id = $1",
            "DELETE FROM deployment_promotion_facts WHERE deployment_id = $1",
            "DELETE FROM deployment_recovery_action_receipts WHERE source_deployment_id = $1 OR result_deployment_id = $1",
            "DELETE FROM deployment_runtime_health WHERE deployment_id = $1",
            "DELETE FROM deployment_timeline_counters WHERE deployment_id = $1",
        ] {
            sqlx::query(statement)
                .bind(deployment_id)
                .execute(pool)
                .await
                .expect("clean up a deployment test fixture table");
        }
    }
    sqlx::query("DELETE FROM deployments WHERE agent_id = $1::uuid")
        .bind(agent_id)
        .execute(pool)
        .await
        .expect("clean up deployment test fixture deployments");
}

#[tokio::test]
#[ignore]
async fn deploy_cancel_and_read_deployment_round_trip() {
    let _guard = lock_project_agents().await;
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_agent_draft_test_agent_by_slug(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-deployment-agent",
    )
    .await;

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { createAgentDraft(input: { projectId: \"50000000-0000-0000-0000-000000000001\", displayName: \"HTTP Integration Deployment Agent\" }) \
            { agentDraft { agentId } problems { code } } }",
    )
    .await;
    let agent_id = create_body["data"]["createAgentDraft"]["agentDraft"]["agentId"]
        .as_str()
        .unwrap()
        .to_string();

    let update_query = format!(
        "mutation {{ updateAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: 1, \
            document: {{ general: {{ displayName: \"HTTP Integration Deployment Agent\" }}, instructions: {{ source: \"Do the thing.\" }}, limits: {{ maxTokens: 4096 }} }} }}) \
            {{ agentDraft {{ revision }} problems {{ code }} }} }}"
    );
    graphql_as(&router, &cookie, &update_query).await;

    let validate_query = format!(
        "mutation {{ validateAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: 2 }}) \
            {{ agentDraft {{ revision }} problems {{ code }} }} }}"
    );
    let validate_body = graphql_as(&router, &cookie, &validate_query).await;
    let validated_revision = validate_body["data"]["validateAgentDraft"]["agentDraft"]["revision"]
        .as_i64()
        .unwrap();

    let publish_query = format!(
        "mutation {{ publishAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: {validated_revision}, warningsAcknowledged: true }}) \
            {{ agentVersion {{ id versionNumber }} problems {{ code }} }} }}"
    );
    let publish_body = graphql_as(&router, &cookie, &publish_query).await;
    assert_eq!(
        publish_body["data"]["publishAgentDraft"]["problems"],
        serde_json::json!([])
    );
    let agent_version_id = publish_body["data"]["publishAgentDraft"]["agentVersion"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let environment_id = "e1300000-0000-0000-0000-000000000001";

    let preview_query = format!(
        "query {{ deploymentPreview(agentVersionId: \"{agent_version_id}\", environmentDefinitionVersionId: \"{environment_id}\", strategy: REPLACE) \
            {{ strategy risk }} }}"
    );
    let preview_body = graphql_as(&router, &cookie, &preview_query).await;
    assert_eq!(
        preview_body["data"]["deploymentPreview"]["strategy"],
        "REPLACE"
    );

    let deploy_query = format!(
        "mutation {{ deployAgentVersion(input: {{ agentVersionId: \"{agent_version_id}\", environmentDefinitionVersionId: \"{environment_id}\", strategy: REPLACE, \
            idempotencyKey: \"http-integration-deploy-round-trip\" }}) {{ deployment {{ id revision lifecycleStatus }} problems {{ code message }} }} }}"
    );

    // Principal 1's seeded ORGANIZATION_ADMIN role on the owning organization grants
    // DEPLOYMENT.VIEW (the preview above succeeds) but not DEPLOYMENT.REQUEST, so a deploy
    // attempt without an explicit project role is refused as FORBIDDEN.
    let forbidden_body = graphql_as(&router, &cookie, &deploy_query).await;
    assert_eq!(
        forbidden_body["data"]["deployAgentVersion"]["deployment"],
        serde_json::Value::Null
    );
    assert_eq!(
        forbidden_body["data"]["deployAgentVersion"]["problems"][0]["code"],
        "FORBIDDEN"
    );

    let membership_id = grant_project_role(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "00000000-0000-0000-0000-000000000001",
        "PROJECT_ADMIN",
    )
    .await;

    let deploy_body = graphql_as(&router, &cookie, &deploy_query).await;
    assert_eq!(
        deploy_body["data"]["deployAgentVersion"]["problems"],
        serde_json::json!([])
    );
    // The compiled policy's required-approver count (driven by the computed risk tier, not
    // controlled by this test) decides whether the deployment lands in REQUESTED (0 required
    // approvers, auto-satisfied inline) or AWAITING_APPROVAL (1+ required approvers) — both are
    // valid outcomes here; only cancel's lifecycle handling below depends on which one occurred.
    let initial_status = deploy_body["data"]["deployAgentVersion"]["deployment"]["lifecycleStatus"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(matches!(
        initial_status.as_str(),
        "REQUESTED" | "AWAITING_APPROVAL"
    ));
    let deployment_id = deploy_body["data"]["deployAgentVersion"]["deployment"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let deployments_query = format!(
        "query {{ deployments(projectId: \"50000000-0000-0000-0000-000000000001\", filter: {{ agentId: \"{agent_id}\" }}, first: 10) \
            {{ edges {{ node {{ id lifecycleStatus }} }} }} }}"
    );
    let deployments_body = graphql_as(&router, &cookie, &deployments_query).await;
    let listed = deployments_body["data"]["deployments"]["edges"]
        .as_array()
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["node"]["id"], deployment_id);

    let detail_query = format!(
        "query {{ deploymentProjection(deploymentId: \"{deployment_id}\") \
            {{ deployment {{ id lifecycleStatus }} timeline {{ edges {{ node {{ stage status }} }} }} }} }}"
    );
    let detail_body = graphql_as(&router, &cookie, &detail_query).await;
    assert_eq!(
        detail_body["data"]["deploymentProjection"]["deployment"]["lifecycleStatus"],
        initial_status
    );
    assert_eq!(
        detail_body["data"]["deploymentProjection"]["timeline"]["edges"][0]["node"]["stage"],
        "REQUESTED"
    );

    let environments_query = format!(
        "query {{ deploymentEnvironmentDefinitionVersions(agentVersionId: \"{agent_version_id}\", first: 10) \
            {{ edges {{ node {{ id logicalEnvironmentClass }} }} }} }}"
    );
    let environments_body = graphql_as(&router, &cookie, &environments_query).await;
    let environments = environments_body["data"]["deploymentEnvironmentDefinitionVersions"]
        ["edges"]
        .as_array()
        .unwrap();
    assert!(environments
        .iter()
        .any(|edge| edge["node"]["id"] == environment_id));

    let cancel_query = format!(
        "mutation {{ cancelDeployment(input: {{ deploymentId: \"{deployment_id}\", expectedRevision: 1, reason: \"http integration cleanup\" }}) \
            {{ deployment {{ id revision lifecycleStatus }} problems {{ code message }} }} }}"
    );
    let cancel_body = graphql_as(&router, &cookie, &cancel_query).await;
    assert_eq!(
        cancel_body["data"]["cancelDeployment"]["deployment"]["lifecycleStatus"],
        "CANCELED"
    );

    // The revision just advanced by the cancel above, so replaying the same stale
    // expectedRevision is refused as a revision conflict rather than canceling twice.
    let stale_cancel_body = graphql_as(&router, &cookie, &cancel_query).await;
    assert_eq!(
        stale_cancel_body["data"]["cancelDeployment"]["deployment"],
        serde_json::Value::Null
    );
    assert_eq!(
        stale_cancel_body["data"]["cancelDeployment"]["problems"][0]["code"],
        "REVISION_CONFLICT"
    );

    revoke_project_membership(&pool, membership_id).await;
    delete_deployment_test_fixtures(&pool, &agent_id).await;
    delete_agent_draft_test_agent(&pool, &agent_id).await;
}

async fn grant_organization_membership(
    pool: &sqlx::PgPool,
    organization_id: &str,
    principal_id: &str,
) -> uuid::Uuid {
    let membership_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, revision, active_marker) \
         VALUES ($1, $2::uuid, $3::uuid, CURRENT_TIMESTAMP, 1, true)",
    )
    .bind(membership_id)
    .bind(organization_id)
    .bind(principal_id)
    .execute(pool)
    .await
    .expect("grant an organization membership");
    sqlx::query(
        "INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_MEMBER')",
    )
    .bind(membership_id)
    .execute(pool)
    .await
    .expect("grant an organization membership role");
    membership_id
}

async fn revoke_organization_membership(pool: &sqlx::PgPool, membership_id: uuid::Uuid) {
    sqlx::query("DELETE FROM organization_membership_roles WHERE membership_id = $1")
        .bind(membership_id)
        .execute(pool)
        .await
        .expect("revoke an organization membership role");
    sqlx::query("DELETE FROM organization_memberships WHERE id = $1")
        .bind(membership_id)
        .execute(pool)
        .await
        .expect("revoke an organization membership");
}

/// Covers the approval inbox/decision GraphQL surface RTP-APPROVAL adds on top of the already-
/// verified deploy/cancel round trip above: `approvalInbox`, `approvalRequirement`,
/// `decideDeploymentApproval`'s self-approval refusal, a real APPROVE decision, idempotent replay,
/// and a stale-revision conflict.
#[tokio::test]
#[ignore]
async fn approval_inbox_and_decide_round_trip() {
    let _guard = lock_project_agents().await;
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_agent_draft_test_agent_by_slug(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-approval-agent",
    )
    .await;

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { createAgentDraft(input: { projectId: \"50000000-0000-0000-0000-000000000001\", displayName: \"HTTP Integration Approval Agent\" }) \
            { agentDraft { agentId } problems { code } } }",
    )
    .await;
    let agent_id = create_body["data"]["createAgentDraft"]["agentDraft"]["agentId"]
        .as_str()
        .unwrap()
        .to_string();

    let update_query = format!(
        "mutation {{ updateAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: 1, \
            document: {{ general: {{ displayName: \"HTTP Integration Approval Agent\" }}, instructions: {{ source: \"Do the thing.\" }}, limits: {{ maxTokens: 4096 }} }} }}) \
            {{ agentDraft {{ revision }} problems {{ code }} }} }}"
    );
    graphql_as(&router, &cookie, &update_query).await;

    let validate_query = format!(
        "mutation {{ validateAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: 2 }}) \
            {{ agentDraft {{ revision }} problems {{ code }} }} }}"
    );
    let validate_body = graphql_as(&router, &cookie, &validate_query).await;
    let validated_revision = validate_body["data"]["validateAgentDraft"]["agentDraft"]["revision"]
        .as_i64()
        .unwrap();

    let publish_query = format!(
        "mutation {{ publishAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: {validated_revision}, warningsAcknowledged: true }}) \
            {{ agentVersion {{ id }} problems {{ code }} }} }}"
    );
    let publish_body = graphql_as(&router, &cookie, &publish_query).await;
    let agent_version_id = publish_body["data"]["publishAgentDraft"]["agentVersion"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let environment_id = "e1300000-0000-0000-0000-000000000001";
    let membership_id = grant_project_role(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "00000000-0000-0000-0000-000000000001",
        "PROJECT_ADMIN",
    )
    .await;

    let deploy_query = format!(
        "mutation {{ deployAgentVersion(input: {{ agentVersionId: \"{agent_version_id}\", environmentDefinitionVersionId: \"{environment_id}\", strategy: REPLACE, \
            idempotencyKey: \"http-integration-approval-round-trip\" }}) {{ deployment {{ id revision lifecycleStatus }} problems {{ code message }} }} }}"
    );
    let deploy_body = graphql_as(&router, &cookie, &deploy_query).await;
    assert_eq!(
        deploy_body["data"]["deployAgentVersion"]["problems"],
        serde_json::json!([])
    );
    let deployment_id = deploy_body["data"]["deployAgentVersion"]["deployment"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let initial_status = deploy_body["data"]["deployAgentVersion"]["deployment"]["lifecycleStatus"]
        .as_str()
        .unwrap()
        .to_string();

    if initial_status == "AWAITING_APPROVAL" {
        let inbox_query = "query { approvalInbox(projectId: \"50000000-0000-0000-0000-000000000001\", first: 20) \
            { edges { node { requirement { id status } deployment { id } eligible decisionAvailable } } } }";
        let inbox_body = graphql_as(&router, &cookie, inbox_query).await;
        let edges = inbox_body["data"]["approvalInbox"]["edges"]
            .as_array()
            .unwrap();
        let entry = edges
            .iter()
            .find(|edge| edge["node"]["deployment"]["id"] == deployment_id)
            .expect("the new deployment appears in the approval inbox");
        assert_eq!(entry["node"]["eligible"], false);
        let requirement_id = entry["node"]["requirement"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        let detail_query = format!(
            "query {{ approvalRequirement(approvalRequirementId: \"{requirement_id}\") {{ eligible decisionAvailable requirement {{ status }} }} }}"
        );
        let detail_body = graphql_as(&router, &cookie, &detail_query).await;
        assert_eq!(
            detail_body["data"]["approvalRequirement"]["eligible"],
            false
        );

        // decideDeploymentApproval's idempotencyKey is parsed as a UUID (unlike the other
        // deployment mutations' free-form idempotency keys), so every key below must be one.
        let self_decide_query = format!(
            "mutation {{ decideDeploymentApproval(input: {{ approvalRequirementId: \"{requirement_id}\", expectedRevision: 1, decision: APPROVE, \
                idempotencyKey: \"{}\" }}) {{ decision {{ id }} problems {{ code }} }} }}",
            uuid::Uuid::new_v4()
        );
        let self_decide_body = graphql_as(&router, &cookie, &self_decide_query).await;
        assert_eq!(
            self_decide_body["data"]["decideDeploymentApproval"]["decision"],
            serde_json::Value::Null
        );
        assert_eq!(
            self_decide_body["data"]["decideDeploymentApproval"]["problems"][0]["code"],
            "APPROVER_INELIGIBLE"
        );

        let approver_project_membership = grant_project_role(
            &pool,
            "50000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
            "DEPLOYMENT_APPROVER",
        )
        .await;
        let approver_organization_membership = grant_organization_membership(
            &pool,
            "10000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
        )
        .await;
        let approver_cookie =
            authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;

        let decide_query = format!(
            "mutation {{ decideDeploymentApproval(input: {{ approvalRequirementId: \"{requirement_id}\", expectedRevision: 1, decision: APPROVE, \
                idempotencyKey: \"{}\" }}) {{ decision {{ id decision }} requirement {{ status }} deployment {{ lifecycleStatus }} problems {{ code }} }} }}",
            uuid::Uuid::new_v4()
        );
        let decide_body = graphql_as(&router, &approver_cookie, &decide_query).await;
        assert_eq!(
            decide_body["data"]["decideDeploymentApproval"]["problems"],
            serde_json::json!([])
        );
        assert_eq!(
            decide_body["data"]["decideDeploymentApproval"]["decision"]["decision"],
            "APPROVE"
        );
        let decision_id = decide_body["data"]["decideDeploymentApproval"]["decision"]["id"].clone();

        let replay_body = graphql_as(&router, &approver_cookie, &decide_query).await;
        assert_eq!(
            replay_body["data"]["decideDeploymentApproval"]["decision"]["id"],
            decision_id
        );

        let stale_query = format!(
            "mutation {{ decideDeploymentApproval(input: {{ approvalRequirementId: \"{requirement_id}\", expectedRevision: 1, decision: REJECT, \
                idempotencyKey: \"{}\" }}) {{ problems {{ code }} }} }}",
            uuid::Uuid::new_v4()
        );
        let stale_body = graphql_as(&router, &approver_cookie, &stale_query).await;
        assert_eq!(
            stale_body["data"]["decideDeploymentApproval"]["problems"][0]["code"],
            "REVISION_CONFLICT"
        );

        revoke_organization_membership(&pool, approver_organization_membership).await;
        revoke_project_membership(&pool, approver_project_membership).await;
    }

    revoke_project_membership(&pool, membership_id).await;
    delete_deployment_test_fixtures(&pool, &agent_id).await;
    delete_agent_draft_test_agent(&pool, &agent_id).await;
}

async fn delete_evaluation_test_fixtures(pool: &sqlx::PgPool, definition_id: &str, agent_id: &str) {
    let run_ids: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT run.id FROM evaluation_runs run \
         JOIN evaluation_definition_versions version ON version.id = run.definition_version_id \
         WHERE version.definition_id = $1::uuid",
    )
    .bind(definition_id)
    .fetch_all(pool)
    .await
    .expect("look up evaluation test runs");
    for (run_id,) in &run_ids {
        for statement in [
            "DELETE FROM evaluation_case_runs WHERE run_id = $1",
            "DELETE FROM evaluation_metric_results WHERE run_id = $1",
            "DELETE FROM evaluation_artifact_metadata WHERE run_id = $1",
            "DELETE FROM evaluation_results WHERE run_id = $1",
            "DELETE FROM evaluation_audit_events WHERE run_id = $1",
            "DELETE FROM evaluation_outbox_events WHERE run_id = $1",
            "DELETE FROM evaluation_target_snapshots WHERE run_id = $1",
            "DELETE FROM evaluation_command_receipts WHERE run_id = $1",
        ] {
            sqlx::query(statement)
                .bind(run_id)
                .execute(pool)
                .await
                .expect("clean up an evaluation test run table");
        }
    }
    sqlx::query("DELETE FROM evaluation_runs WHERE definition_version_id IN (SELECT id FROM evaluation_definition_versions WHERE definition_id = $1::uuid)")
        .bind(definition_id)
        .execute(pool)
        .await
        .expect("clean up evaluation test runs");
    sqlx::query("DELETE FROM evaluation_command_receipts WHERE definition_id = $1::uuid OR definition_version_id IN (SELECT id FROM evaluation_definition_versions WHERE definition_id = $1::uuid)")
        .bind(definition_id)
        .execute(pool)
        .await
        .expect("clean up evaluation test command receipts");
    sqlx::query("DELETE FROM evaluation_audit_events WHERE definition_id = $1::uuid")
        .bind(definition_id)
        .execute(pool)
        .await
        .expect("clean up evaluation test definition audit events");
    sqlx::query("DELETE FROM evaluation_definition_versions WHERE definition_id = $1::uuid")
        .bind(definition_id)
        .execute(pool)
        .await
        .expect("clean up evaluation test definition versions");
    sqlx::query("DELETE FROM evaluation_definition_drafts WHERE definition_id = $1::uuid")
        .bind(definition_id)
        .execute(pool)
        .await
        .expect("clean up evaluation test definition draft");
    sqlx::query("DELETE FROM evaluation_definitions WHERE id = $1::uuid")
        .bind(definition_id)
        .execute(pool)
        .await
        .expect("clean up evaluation test definition");
    sqlx::query("DELETE FROM evaluation_target_projections WHERE target_kind = 'AGENT_VERSION' AND target_id IN (SELECT id FROM agent_versions WHERE agent_id = $1::uuid)")
        .bind(agent_id)
        .execute(pool)
        .await
        .expect("clean up evaluation test target projections");
}

/// Covers the evaluation GraphQL surface end to end against a real published agent version:
/// create/validate/publish a definition, `evaluationTargets`, `runEvaluation`, driving the local
/// outbox worker in-process (mirroring the `evaluation-worker` subcommand's own delivery path) to
/// completion, then `evaluationRun`'s nested connections, a lifecycle-conflict refusal, and rerun.
#[tokio::test]
#[ignore]
async fn evaluation_definition_and_run_round_trip() {
    let _guard = lock_project_agents().await;
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_agent_draft_test_agent_by_slug(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-evaluation-agent",
    )
    .await;

    let membership_id = grant_project_role(
        &pool,
        "50000000-0000-0000-0000-000000000001",
        "00000000-0000-0000-0000-000000000001",
        "PROJECT_ADMIN",
    )
    .await;

    let create_agent_body = graphql_as(
        &router,
        &cookie,
        "mutation { createAgentDraft(input: { projectId: \"50000000-0000-0000-0000-000000000001\", displayName: \"HTTP Integration Evaluation Agent\" }) \
            { agentDraft { agentId } problems { code } } }",
    )
    .await;
    let agent_id = create_agent_body["data"]["createAgentDraft"]["agentDraft"]["agentId"]
        .as_str()
        .unwrap()
        .to_string();
    let update_query = format!(
        "mutation {{ updateAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: 1, \
            document: {{ general: {{ displayName: \"HTTP Integration Evaluation Agent\" }}, instructions: {{ source: \"Do the thing.\" }}, limits: {{ maxTokens: 4096 }} }} }}) \
            {{ agentDraft {{ revision }} problems {{ code }} }} }}"
    );
    graphql_as(&router, &cookie, &update_query).await;
    let validate_agent_query = format!(
        "mutation {{ validateAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: 2 }}) \
            {{ agentDraft {{ revision }} problems {{ code }} }} }}"
    );
    let validate_agent_body = graphql_as(&router, &cookie, &validate_agent_query).await;
    let agent_revision = validate_agent_body["data"]["validateAgentDraft"]["agentDraft"]
        ["revision"]
        .as_i64()
        .unwrap();
    let publish_agent_query = format!(
        "mutation {{ publishAgentDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", agentId: \"{agent_id}\", expectedRevision: {agent_revision}, warningsAcknowledged: true }}) \
            {{ agentVersion {{ id }} problems {{ code }} }} }}"
    );
    let publish_agent_body = graphql_as(&router, &cookie, &publish_agent_query).await;
    let agent_version_id = publish_agent_body["data"]["publishAgentDraft"]["agentVersion"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { createEvaluationDefinition(input: { projectId: \"50000000-0000-0000-0000-000000000001\", slug: \"http-integration-eval\", \
            idempotencyKey: \"http-integration-eval-create\" }) { definition { id draft { revision } } problems { code } } }",
    )
    .await;
    assert_eq!(
        create_body["data"]["createEvaluationDefinition"]["problems"],
        serde_json::json!([])
    );
    let definition_id = create_body["data"]["createEvaluationDefinition"]["definition"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let validate_query = format!(
        "mutation {{ validateEvaluationDefinitionDraft(input: {{ definitionId: \"{definition_id}\", expectedRevision: 1, idempotencyKey: \"http-integration-eval-validate\" }}) \
            {{ definition {{ draft {{ revision validationStatus }} }} problems {{ code }} }} }}"
    );
    let validate_body = graphql_as(&router, &cookie, &validate_query).await;
    assert_eq!(
        validate_body["data"]["validateEvaluationDefinitionDraft"]["definition"]["draft"]
            ["validationStatus"],
        "VALID"
    );
    let validated_revision = validate_body["data"]["validateEvaluationDefinitionDraft"]
        ["definition"]["draft"]["revision"]
        .as_i64()
        .unwrap();

    let publish_query = format!(
        "mutation {{ publishEvaluationDefinitionDraft(input: {{ definitionId: \"{definition_id}\", expectedRevision: {validated_revision}, idempotencyKey: \"http-integration-eval-publish\" }}) \
            {{ version {{ id number }} problems {{ code }} }} }}"
    );
    let publish_body = graphql_as(&router, &cookie, &publish_query).await;
    assert_eq!(
        publish_body["data"]["publishEvaluationDefinitionDraft"]["problems"],
        serde_json::json!([])
    );
    let version_id = publish_body["data"]["publishEvaluationDefinitionDraft"]["version"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let targets_query = format!(
        "query {{ evaluationTargets(projectId: \"50000000-0000-0000-0000-000000000001\", definitionVersionId: \"{version_id}\", first: 20) \
            {{ edges {{ node {{ kind id }} }} }} }}"
    );
    let targets_body = graphql_as(&router, &cookie, &targets_query).await;
    let targets = targets_body["data"]["evaluationTargets"]["edges"]
        .as_array()
        .unwrap();
    assert!(targets
        .iter()
        .any(|edge| edge["node"]["kind"] == "AGENT_VERSION"
            && edge["node"]["id"] == agent_version_id));

    let environment_id = "e1300000-0000-0000-0000-000000000001";
    let run_query = format!(
        "mutation {{ runEvaluation(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", definitionVersionId: \"{version_id}\", \
            targetKind: AGENT_VERSION, targetId: \"{agent_version_id}\", environmentDefinitionVersionId: \"{environment_id}\", \
            idempotencyKey: \"http-integration-eval-run\" }}) {{ run {{ id lifecycleStatus generation }} problems {{ code message }} }} }}"
    );
    let run_body = graphql_as(&router, &cookie, &run_query).await;
    assert_eq!(
        run_body["data"]["runEvaluation"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        run_body["data"]["runEvaluation"]["run"]["lifecycleStatus"],
        "QUEUED"
    );
    let run_id = run_body["data"]["runEvaluation"]["run"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Drives the same claim -> decide -> commit -> delivered cycle the `evaluation-worker`
    // subcommand's outer loop calls, in-process, mirroring how the deployment domain's existing
    // integration tests never spawn a second process for worker-delivered state either.
    let worker_db = sea_orm::Database::connect(test_database_url())
        .await
        .expect("connect the evaluation worker to the test database");
    let worker = hive_application::evaluation::LocalEvaluationWorker::new(
        hive_persistence::evaluation::PgEvaluationWorkStore::new(worker_db),
        Box::new(hive_application::evaluation::LocalPromptCaseFixtureAdapter),
        "http-integration-evaluation-worker".to_string(),
    );
    worker
        .run_batch(10)
        .await
        .expect("the evaluation worker batch completes");

    let run_detail_query = format!(
        "query {{ evaluationRun(runId: \"{run_id}\") {{ id lifecycleStatus outcomeCategory generation \
            cases(first: 10) {{ edges {{ node {{ key passed }} }} }} \
            metrics(first: 10) {{ edges {{ node {{ code value passed }} }} }} \
            artifacts(first: 10) {{ edges {{ node {{ kind }} }} }} \
            audit(first: 10) {{ edges {{ node {{ action }} }} }} }} }}"
    );
    let run_detail_body = graphql_as(&router, &cookie, &run_detail_query).await;
    let run_detail = &run_detail_body["data"]["evaluationRun"];
    assert_eq!(run_detail["lifecycleStatus"], "COMPLETED");
    assert_eq!(run_detail["outcomeCategory"], "PASSED");
    assert_eq!(run_detail["cases"]["edges"][0]["node"]["passed"], true);
    assert_eq!(run_detail["metrics"]["edges"][0]["node"]["passed"], true);
    assert_eq!(
        run_detail["artifacts"]["edges"][0]["node"]["kind"],
        "LOCAL_SUMMARY"
    );
    assert!(!run_detail["audit"]["edges"].as_array().unwrap().is_empty());
    let completed_generation = run_detail["generation"].as_i64().unwrap();

    // The run is terminal (COMPLETED), so cancel is refused as a lifecycle conflict regardless of
    // whether the supplied generation happens to still be current.
    let cancel_query = format!(
        "mutation {{ cancelEvaluation(input: {{ runId: \"{run_id}\", expectedGeneration: {completed_generation}, idempotencyKey: \"http-integration-eval-cancel\" }}) \
            {{ run {{ id }} problems {{ code }} }} }}"
    );
    let cancel_body = graphql_as(&router, &cookie, &cancel_query).await;
    assert_eq!(
        cancel_body["data"]["cancelEvaluation"]["run"],
        serde_json::Value::Null
    );
    assert_eq!(
        cancel_body["data"]["cancelEvaluation"]["problems"][0]["code"],
        "LIFECYCLE_CONFLICT"
    );

    let rerun_query = format!(
        "mutation {{ rerunEvaluation(input: {{ runId: \"{run_id}\", idempotencyKey: \"http-integration-eval-rerun\" }}) \
            {{ run {{ id sourceRunId lifecycleStatus }} problems {{ code }} }} }}"
    );
    let rerun_body = graphql_as(&router, &cookie, &rerun_query).await;
    assert_eq!(
        rerun_body["data"]["rerunEvaluation"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        rerun_body["data"]["rerunEvaluation"]["run"]["sourceRunId"],
        run_id
    );

    sqlx::query("DELETE FROM evaluation_worker_heartbeats WHERE worker_id = 'http-integration-evaluation-worker'")
        .execute(&pool)
        .await
        .expect("clean up the in-process evaluation worker's heartbeat row");
    revoke_project_membership(&pool, membership_id).await;
    delete_evaluation_test_fixtures(&pool, &definition_id, &agent_id).await;
    delete_deployment_test_fixtures(&pool, &agent_id).await;
    delete_agent_draft_test_agent(&pool, &agent_id).await;
}

#[tokio::test]
#[ignore]
async fn audit_events_requires_a_narrowing_filter() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(
        &router,
        &cookie,
        "query { auditEvents(filter: { organizationId: \"10000000-0000-0000-0000-000000000001\" }) { totalCount } }",
    )
    .await;
    assert_eq!(
        body["errors"][0]["message"],
        "An audit query requires a narrowing filter."
    );
}

/// Ports the exact asymmetry `AuditGraphql.Resolver.events`/`.event` carry: a principal with no
/// visible audit scope makes `auditEvents` a GraphQL error, while `auditEvent` resolves quietly
/// to `null`. Bea (00000000-...-0002) holds no role on organization 10000000-...-0001.
#[tokio::test]
#[ignore]
async fn audit_events_refuses_an_unauthorized_principal_while_audit_event_resolves_to_null() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;

    let events_body = graphql_as(
        &router,
        &cookie,
        "query { auditEvents(filter: { organizationId: \"10000000-0000-0000-0000-000000000001\", occurredAfter: \"2020-01-01T00:00:00Z\" }) { totalCount } }",
    )
    .await;
    assert_eq!(
        events_body["errors"][0]["message"],
        "Audit history is unavailable."
    );

    let event_body = graphql_as(
        &router,
        &cookie,
        "query { auditEvent(filter: { organizationId: \"10000000-0000-0000-0000-000000000001\" }, eventId: \"deployment:00000000-0000-0000-0000-000000000099\") { id } }",
    )
    .await;
    assert_eq!(event_body["data"]["auditEvent"], serde_json::Value::Null);
    assert!(event_body.get("errors").is_none());
}

/// Drives a real mutation through the full `/graphql` handler (not `graphql_as`, so this can set
/// a `User-Agent` header and a loopback `ConnectInfo`) and confirms the resulting audit row binds
/// `request_id`/`correlation_id`/`graphql_operation`/`source_ip`/`user_agent` from the tokio
/// task-local (`hive_persistence::audit::context`) instead of leaving them `NULL`, and that
/// `AUDIT_SENSITIVE.VIEW` gates `sourceIp`/`userAgent` (redacted for Ada's plain
/// `ORGANIZATION_ADMIN` grant, visible once she also holds `PLATFORM_ADMIN`).
#[tokio::test]
#[ignore]
async fn audit_events_bind_request_metadata_and_redact_sensitive_fields_by_capability() {
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to test database");
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let mut request = Request::post("/graphql")
        .header("content-type", "application/json")
        .header("cookie", &cookie)
        .header("user-agent", "hive-http-integration/1.0")
        .body(Body::from(
            serde_json::json!({
                "query": "mutation TouchProjectGeneralForAudit { updateProjectGeneral(input: { \
                    projectId: \"50000000-0000-0000-0000-000000000001\", expectedRevision: 1, \
                    displayName: \"Customer Feedback Copilot\", description: \"\" }) { problems { code } } }"
            })
            .to_string(),
        ))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 54321))));
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(
        body["data"]["updateProjectGeneral"]["problems"],
        serde_json::json!([])
    );

    let (event_id, request_id): (uuid::Uuid, uuid::Uuid) = sqlx::query_as(
        "SELECT id, request_id FROM administration_audit_events \
         WHERE scope_id = $1::uuid AND action = 'PROJECT_GENERAL_UPDATED' \
         ORDER BY occurred_at DESC LIMIT 1",
    )
    .bind("50000000-0000-0000-0000-000000000001")
    .fetch_one(&pool)
    .await
    .expect("find the newly written administration audit event");

    let event_query = format!(
        "query {{ auditEvent(filter: {{ organizationId: \"10000000-0000-0000-0000-000000000001\" }}, eventId: \"administration:{event_id}\") \
            {{ id requestId correlationId graphqlOperation sourceIp userAgent sensitiveFieldsRedacted }} }}"
    );
    let unprivileged = graphql_as(&router, &cookie, &event_query).await;
    let node = &unprivileged["data"]["auditEvent"];
    assert_eq!(node["requestId"], request_id.to_string());
    assert_eq!(node["correlationId"], request_id.to_string());
    assert_eq!(node["graphqlOperation"], "TouchProjectGeneralForAudit");
    assert_eq!(node["sourceIp"], serde_json::Value::Null);
    assert_eq!(node["userAgent"], serde_json::Value::Null);
    assert_eq!(node["sensitiveFieldsRedacted"], true);

    sqlx::query(
        "INSERT INTO platform_role_assignments (principal_id, role_code) VALUES ($1::uuid, 'PLATFORM_ADMIN')",
    )
    .bind("00000000-0000-0000-0000-000000000001")
    .execute(&pool)
    .await
    .expect("grant platform admin");

    let privileged = graphql_as(&router, &cookie, &event_query).await;
    let node = &privileged["data"]["auditEvent"];
    assert_eq!(node["sourceIp"], "127.0.0.1");
    assert_eq!(node["userAgent"], "hive-http-integration/1.0");

    sqlx::query(
        "DELETE FROM platform_role_assignments WHERE principal_id = $1::uuid AND role_code = 'PLATFORM_ADMIN'",
    )
    .bind("00000000-0000-0000-0000-000000000001")
    .execute(&pool)
    .await
    .expect("revoke platform admin");
    sqlx::query("DELETE FROM administration_audit_events WHERE id = $1")
        .bind(event_id)
        .execute(&pool)
        .await
        .expect("clean up the test audit event");
    sqlx::query("UPDATE projects SET revision = 1 WHERE id = $1::uuid AND revision = 2")
        .bind("50000000-0000-0000-0000-000000000001")
        .execute(&pool)
        .await
        .expect("restore the project revision");
}
