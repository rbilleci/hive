//! Requires a live, empty PostgreSQL database named by `HIVE_TEST_DATABASE_URL`
//! (falls back to `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test http_integration -- --ignored

use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::net::{Ipv4Addr, SocketAddr};
use tower::ServiceExt;

fn test_database_url() -> String {
    std::env::var("HIVE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hive:hive@127.0.0.1:15432/hive".to_string())
}

/// The connection this file's fixture helpers below use to set up and tear down rows, separate
/// from the router's own pool. They read and write through the same SeaORM entity modules the
/// service does (`hive_persistence::entity`), so a schema change breaks them at compile time.
async fn fixture_database() -> sea_orm::DatabaseConnection {
    sea_orm::Database::connect(test_database_url())
        .await
        .expect("connect to test database")
}

/// The identifiers in this file are written as literal strings, as they are inside the GraphQL
/// documents next to them; the entity columns are typed `Uuid`.
fn uuid(value: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(value).expect("a fixture identifier is a UUID")
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
                    r#"{"query":"{ principals { nodes { id subject } } }"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    // The deleted `currentPrincipal` query is the generated `principals` read: Ada administers no
    // organization and views no project's memberships, so the tenant rule answers her own row and
    // nothing else — which is exactly "who is this cookie". `subject` is the stored column now,
    // where the deleted resolver echoed the identifier back.
    assert_eq!(
        body["data"]["principals"]["nodes"],
        serde_json::json!([{ "id": ADA, "subject": "ada.fixture" }])
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

// Administration reads are generated: the scope row with its membership, budget policy and
// approval policy relations. Ada (00000000-...-0001) is ORGANIZATION_ADMIN of 10000000-...-0001
// (Product); project 50000000-...-0001 (Customer Feedback Copilot, in Product) has a seeded
// budget policy and P-05 approval policy (organization-project-administration.sql).

#[tokio::test]
#[ignore]
async fn organization_administration_reports_memberships_and_admin_capabilities_for_an_admin() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(
        &router,
        &cookie,
        "{ organizations(filters: { id: { eq: \"10000000-0000-0000-0000-000000000001\" } }) { nodes { \
            slug displayName lifecycleStatus assignableRoles capabilities \
            organizationMemberships { nodes { roleCodes projectAccessSummary principals { displayName } } } \
            availablePrincipals { displayName } } } }",
    )
    .await;
    let organization = &body["data"]["organizations"]["nodes"][0];
    assert_eq!(organization["slug"], "product");
    assert_eq!(organization["lifecycleStatus"], "ACTIVE");
    let capabilities: Vec<&str> = organization["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert!(capabilities.contains(&"ORGANIZATION_MEMBERSHIP.ADD"));
    let membership = organization["organizationMemberships"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|membership| membership["principals"]["displayName"] == "Ada Lovelace")
        .unwrap();
    assert!(membership["roleCodes"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("ORGANIZATION_ADMIN")));
    assert_eq!(
        membership["projectAccessSummary"][0],
        "All organization projects (ORGANIZATION_ADMIN)"
    );
    assert!(organization["availablePrincipals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|principal| principal["displayName"] == "Ada Lovelace"));
}

#[tokio::test]
#[ignore]
async fn organization_memberships_are_hidden_from_a_principal_outside_the_organization() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;
    let body = graphql_as(
        &router,
        &cookie,
        "{ organizations(filters: { id: { eq: \"10000000-0000-0000-0000-000000000001\" } }) { nodes { id } } \
           organizationMemberships(filters: { organizationId: { eq: \"10000000-0000-0000-0000-000000000001\" } }) { nodes { id } } }",
    )
    .await;
    assert_eq!(
        body["data"]["organizations"]["nodes"],
        serde_json::json!([])
    );
    assert_eq!(
        body["data"]["organizationMemberships"]["nodes"],
        serde_json::json!([])
    );
}

// Ada is a plain ORGANIZATION_MEMBER of Support (10000000-...-0002): she reads the organization
// row, but membership rows and member principals need ORGANIZATION_MEMBERSHIP.VIEW.
#[tokio::test]
#[ignore]
async fn a_plain_member_cannot_list_an_organizations_members() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    let body = graphql_as(
        &router,
        &cookie,
        "{ organizations(filters: { id: { eq: \"10000000-0000-0000-0000-000000000002\" } }) { nodes { \
             slug availablePrincipals { id } organizationMemberships { nodes { id } } } } \
           organizationMemberships(filters: { organizationId: { eq: \"10000000-0000-0000-0000-000000000002\" } }) { nodes { id } } }",
    )
    .await;
    let organization = &body["data"]["organizations"]["nodes"][0];
    assert_eq!(organization["slug"], "support");
    assert_eq!(organization["availablePrincipals"], serde_json::json!([]));
    assert_eq!(
        organization["organizationMemberships"]["nodes"],
        serde_json::json!([])
    );
    assert_eq!(
        body["data"]["organizationMemberships"]["nodes"],
        serde_json::json!([])
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
        "{ projects(filters: { id: { eq: \"50000000-0000-0000-0000-000000000001\" } }) { nodes { \
            slug displayName assignableRoles \
            projectBudgetPolicies { currentVersion { currency monthlyLimitCents warningThresholdCents } \
              status { state currency } } \
            projectApprovalPolicies { currentVersion { rules { cell requiredEvidence requiredApprovers } } } } } }",
    )
    .await;
    let project = &body["data"]["projects"]["nodes"][0];
    assert_eq!(project["slug"], "customer-feedback-copilot");
    let budget = &project["projectBudgetPolicies"];
    assert_eq!(budget["currentVersion"]["currency"], "USD");
    assert_eq!(budget["currentVersion"]["monthlyLimitCents"], 500000);
    assert_eq!(budget["status"]["state"], "NORMAL");
    let rules = project["projectApprovalPolicies"]["currentVersion"]["rules"]
        .as_array()
        .unwrap();
    assert!(rules
        .iter()
        .any(|rule| rule["cell"] == "PRODUCTION_HIGH" && rule["requiredApprovers"] == 2));
}

#[tokio::test]
#[ignore]
async fn project_policies_are_hidden_from_a_principal_outside_the_organization() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;
    let body = graphql_as(
        &router,
        &cookie,
        "{ projectBudgetPolicies(filters: { projectId: { eq: \"50000000-0000-0000-0000-000000000001\" } }) { nodes { projectId } } \
           projectApprovalPolicies(filters: { projectId: { eq: \"50000000-0000-0000-0000-000000000001\" } }) { nodes { id } } \
           projectMemberships(filters: { projectId: { eq: \"50000000-0000-0000-0000-000000000001\" } }) { nodes { id } } }",
    )
    .await;
    for root in [
        "projectBudgetPolicies",
        "projectApprovalPolicies",
        "projectMemberships",
    ] {
        assert_eq!(body["data"][root]["nodes"], serde_json::json!([]), "{root}");
    }
}

// Administration mutations. Ada (00000000-...-0001) is ORGANIZATION_ADMIN of
// 10000000-...-0001 (Product) only, so a mutation test that creates state
// scopes it to a throwaway project under Product and deletes every row it
// wrote afterward — Product's project count is asserted exactly elsewhere
// (`organization_projects_defaults_to_every_lifecycle_status_ordered_by_display_name`).
// The refusal-path tests below write nothing, so they run directly against
// the shared seeded project 50000000-...-0001 (Customer Feedback Copilot).

async fn delete_administration_test_project(db: &sea_orm::DatabaseConnection, project_id: &str) {
    use hive_persistence::entity::{
        administration_audit_events, deployment_approval_principal_project_scopes,
        project_approval_policies, project_approval_policy_versions, project_budget_policies,
        project_budget_policy_versions, project_membership_roles, project_memberships,
        project_settings_connections, projects,
    };
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, QueryTrait};

    let project = uuid(project_id);
    let expect = "clean up an administration test project";
    administration_audit_events::Entity::delete_many()
        .filter(administration_audit_events::Column::ScopeId.eq(project))
        .exec(db)
        .await
        .expect(expect);
    project_settings_connections::Entity::delete_many()
        .filter(project_settings_connections::Column::ProjectId.eq(project))
        .exec(db)
        .await
        .expect(expect);
    project_approval_policy_versions::Entity::delete_many()
        .filter(project_approval_policy_versions::Column::PolicyId.eq(project))
        .exec(db)
        .await
        .expect(expect);
    project_approval_policies::Entity::delete_many()
        .filter(project_approval_policies::Column::ProjectId.eq(project))
        .exec(db)
        .await
        .expect(expect);
    project_budget_policy_versions::Entity::delete_many()
        .filter(project_budget_policy_versions::Column::ProjectId.eq(project))
        .exec(db)
        .await
        .expect(expect);
    project_budget_policies::Entity::delete_many()
        .filter(project_budget_policies::Column::ProjectId.eq(project))
        .exec(db)
        .await
        .expect(expect);
    project_membership_roles::Entity::delete_many()
        .filter(
            project_membership_roles::Column::MembershipId.in_subquery(
                project_memberships::Entity::find()
                    .select_only()
                    .column(project_memberships::Column::Id)
                    .filter(project_memberships::Column::ProjectId.eq(project))
                    .into_query(),
            ),
        )
        .exec(db)
        .await
        .expect(expect);
    deployment_approval_principal_project_scopes::Entity::delete_many()
        .filter(deployment_approval_principal_project_scopes::Column::ProjectId.eq(project))
        .exec(db)
        .await
        .expect(expect);
    project_memberships::Entity::delete_many()
        .filter(project_memberships::Column::ProjectId.eq(project))
        .exec(db)
        .await
        .expect(expect);
    projects::Entity::delete_by_id(project)
        .exec(db)
        .await
        .expect(expect);
}

#[tokio::test]
#[ignore]
async fn create_project_add_membership_budget_general_and_connection_round_trip() {
    let _guard = lock_product_projects().await;
    let db = fixture_database().await;
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
            {{ project {{ projectMemberships {{ nodes {{ roleCodes }} }} }} problems {{ code }} }} }}"
    );
    let add_membership_body = graphql_as(&router, &cookie, &add_membership_query).await;
    assert_eq!(
        add_membership_body["data"]["addAdministrationMembership"]["problems"],
        serde_json::json!([])
    );

    let budget_query = format!(
        "mutation {{ updateProjectBudgetPolicy(input: {{ projectId: \"{project_id}\", expectedRevision: 0, currency: \"USD\", \
            monthlyLimitCents: 100000, warningThresholdCents: 80000, reason: \"round trip\" }}) \
            {{ project {{ projectBudgetPolicies {{ currentVersion {{ currency monthlyLimitCents }} }} }} problems {{ code }} }} }}"
    );
    let budget_body = graphql_as(&router, &cookie, &budget_query).await;
    assert_eq!(
        budget_body["data"]["updateProjectBudgetPolicy"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        budget_body["data"]["updateProjectBudgetPolicy"]["project"]["projectBudgetPolicies"]
            ["currentVersion"]["monthlyLimitCents"],
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
            lifecycleStatus: \"ACTIVE\" }}) {{ project {{ projectSettingsConnections {{ nodes {{ displayName revision }} }} }} problems {{ code }} }} }}"
    );
    let connection_body = graphql_as(&router, &cookie, &connection_query).await;
    assert_eq!(
        connection_body["data"]["saveProjectSettingsConnection"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        connection_body["data"]["saveProjectSettingsConnection"]["project"]
            ["projectSettingsConnections"]["nodes"][0]["displayName"],
        "Primary"
    );

    delete_administration_test_project(&db, &project_id).await;
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
            { project { id } problems { code expectedRevision actualRevision } } }",
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
        "{ organizations(filters: { id: { eq: \"10000000-0000-0000-0000-000000000001\" } }) { nodes { lifecycleStatus revision } } }",
    )
    .await;
    assert_eq!(
        readback["data"]["organizations"]["nodes"][0]["lifecycleStatus"],
        "ACTIVE"
    );
    assert_eq!(readback["data"]["organizations"]["nodes"][0]["revision"], 1);
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
    let db = fixture_database().await;
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

    // Weakens DEVELOPMENT_HIGH from the default's 1 required approver to 0.
    let low = "requiredEvidence: [\"PLAN_VALIDATED\"], requiredApprovers: 0";
    let medium =
        "requiredEvidence: [\"PLAN_VALIDATED\", \"CHANGE_SUMMARY_READY\"], requiredApprovers: 0";
    let high = "requiredEvidence: [\"PLAN_VALIDATED\", \"CHANGE_SUMMARY_READY\", \"EVALUATION_PASSED\"], requiredApprovers: 1";
    let weaker = "requiredEvidence: [\"PLAN_VALIDATED\", \"CHANGE_SUMMARY_READY\", \"EVALUATION_PASSED\"], requiredApprovers: 0";
    let matrix = [
        ("DEVELOPMENT_LOW", low),
        ("DEVELOPMENT_MEDIUM", medium),
        ("DEVELOPMENT_HIGH", weaker),
        ("STAGING_LOW", medium),
        ("STAGING_MEDIUM", high),
        ("STAGING_HIGH", high),
        ("PRODUCTION_LOW", high),
        ("PRODUCTION_MEDIUM", high),
        ("PRODUCTION_HIGH", high),
    ]
    .iter()
    .map(|(cell, rule)| format!("{{ cell: \"{cell}\", {rule} }}"))
    .collect::<Vec<_>>()
    .join(", ");
    let query = format!(
        "mutation {{ updateProjectApprovalPolicy(input: {{ projectId: \"{project_id}\", expectedRevision: 1, \
            reason: \"weaken test\", matrix: [{matrix}] }}) {{ project {{ id }} problems {{ code }} }} }}"
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
        "{{ projectApprovalPolicies(filters: {{ projectId: {{ eq: \"{project_id}\" }} }}) {{ nodes {{ currentRevision }} }} }}"
    );
    let readback = graphql_as(&router, &cookie, &readback_query).await;
    assert_eq!(
        readback["data"]["projectApprovalPolicies"]["nodes"][0]["currentRevision"],
        1
    );

    delete_administration_test_project(&db, &project_id).await;
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
async fn delete_agent_draft_test_agent_by_slug(
    db: &sea_orm::DatabaseConnection,
    project_id: &str,
    slug: &str,
) {
    use hive_persistence::entity::agents;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let existing = agents::Entity::find()
        .filter(agents::Column::ProjectId.eq(uuid(project_id)))
        .filter(agents::Column::Slug.eq(slug))
        .one(db)
        .await
        .expect("look up a leftover agent draft test agent by slug");
    if let Some(agent) = existing {
        delete_agent_draft_test_agent(db, &agent.id.to_string()).await;
    }
}

async fn delete_agent_draft_test_agent(db: &sea_orm::DatabaseConnection, agent_id: &str) {
    use hive_persistence::entity::{
        agent_authoring_audit_events, agent_draft_audit_events, agent_drafts, agent_versions,
        agents, enums, evaluation_target_projections,
    };
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, QueryTrait};

    let agent = uuid(agent_id);
    let expect = "clean up an agent draft test agent";
    evaluation_target_projections::Entity::delete_many()
        .filter(
            evaluation_target_projections::Column::TargetKind
                .eq(enums::EvaluationTargetKind::AgentVersion),
        )
        .filter(
            evaluation_target_projections::Column::TargetId.in_subquery(
                agent_versions::Entity::find()
                    .select_only()
                    .column(agent_versions::Column::Id)
                    .filter(agent_versions::Column::AgentId.eq(agent))
                    .into_query(),
            ),
        )
        .exec(db)
        .await
        .expect(expect);
    agent_versions::Entity::delete_many()
        .filter(agent_versions::Column::AgentId.eq(agent))
        .exec(db)
        .await
        .expect(expect);
    agent_authoring_audit_events::Entity::delete_many()
        .filter(agent_authoring_audit_events::Column::AgentId.eq(agent))
        .exec(db)
        .await
        .expect(expect);
    agent_draft_audit_events::Entity::delete_many()
        .filter(agent_draft_audit_events::Column::AgentId.eq(agent))
        .exec(db)
        .await
        .expect(expect);
    agent_drafts::Entity::delete_many()
        .filter(agent_drafts::Column::AgentId.eq(agent))
        .exec(db)
        .await
        .expect(expect);
    agents::Entity::delete_by_id(agent)
        .exec(db)
        .await
        .expect(expect);
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
    let db = fixture_database().await;
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_agent_draft_test_agent_by_slug(
        &db,
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

    delete_agent_draft_test_agent(&db, &agent_id).await;
}

async fn delete_configuration_test_resource_by_identity(
    db: &sea_orm::DatabaseConnection,
    project_id: &str,
    identity: &str,
) {
    use hive_persistence::entity::{
        configuration_audit_events, reusable_resource_drafts, reusable_resource_versions,
        reusable_resources,
    };
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let existing = reusable_resources::Entity::find()
        .filter(reusable_resources::Column::ProjectId.eq(uuid(project_id)))
        .filter(reusable_resources::Column::Identity.eq(identity))
        .one(db)
        .await
        .expect("look up a leftover configuration test resource by identity");
    if let Some(resource) = existing {
        let id = resource.id;
        let expect = "clean up a configuration test resource";
        configuration_audit_events::Entity::delete_many()
            .filter(configuration_audit_events::Column::SubjectId.eq(id))
            .exec(db)
            .await
            .expect(expect);
        reusable_resource_versions::Entity::delete_many()
            .filter(reusable_resource_versions::Column::ResourceId.eq(id))
            .exec(db)
            .await
            .expect(expect);
        reusable_resource_drafts::Entity::delete_many()
            .filter(reusable_resource_drafts::Column::ResourceId.eq(id))
            .exec(db)
            .await
            .expect(expect);
        reusable_resources::Entity::delete_by_id(id)
            .exec(db)
            .await
            .expect(expect);
    }
}

async fn delete_configuration_test_tool_by_server_id(
    db: &sea_orm::DatabaseConnection,
    project_id: &str,
    server_id: &str,
) {
    use hive_persistence::entity::{configuration_audit_events, project_tool_connections};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let existing = project_tool_connections::Entity::find()
        .filter(project_tool_connections::Column::ProjectId.eq(uuid(project_id)))
        .filter(project_tool_connections::Column::ServerId.eq(server_id))
        .one(db)
        .await
        .expect("look up a leftover configuration test tool by server id");
    if let Some(connection) = existing {
        let id = connection.id;
        let expect = "clean up a configuration test tool";
        configuration_audit_events::Entity::delete_many()
            .filter(configuration_audit_events::Column::SubjectId.eq(id))
            .exec(db)
            .await
            .expect(expect);
        project_tool_connections::Entity::delete_by_id(id)
            .exec(db)
            .await
            .expect(expect);
    }
}

#[tokio::test]
#[ignore]
async fn create_update_validate_and_publish_reusable_resource_round_trip() {
    let db = fixture_database().await;
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_configuration_test_resource_by_identity(
        &db,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-resource",
    )
    .await;

    let create_body = graphql_as(
        &router,
        &cookie,
        "mutation { createReusableResource(input: { projectId: \"50000000-0000-0000-0000-000000000001\", kind: \"PROMPT\", \
            name: \"HTTP Integration Resource\", content: \"Hello {{name}}\", dependencies: [] }) \
            { resource { id currentDraftRevision draft { validationStatus } } problems { code } } }",
    )
    .await;
    assert_eq!(
        create_body["data"]["createReusableResource"]["problems"],
        serde_json::json!([])
    );
    assert_eq!(
        create_body["data"]["createReusableResource"]["resource"]["currentDraftRevision"],
        1
    );
    let resource_id = create_body["data"]["createReusableResource"]["resource"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let update_query = format!(
        "mutation {{ updateReusableResourceDraft(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", resourceId: \"{resource_id}\", \
            expectedRevision: 1, content: \"Hello {{{{name}}}}, welcome.\", dependencies: [] }}) {{ resource {{ currentDraftRevision }} problems {{ code }} }} }}"
    );
    let update_body = graphql_as(&router, &cookie, &update_query).await;
    assert_eq!(
        update_body["data"]["updateReusableResourceDraft"]["resource"]["currentDraftRevision"],
        2
    );

    let validate_query = format!(
        "mutation {{ validateReusableResource(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", resourceId: \"{resource_id}\", expectedRevision: 2 }}) \
            {{ resource {{ draft {{ validationStatus diagnostics }} }} problems {{ code }} }} }}"
    );
    let validate_body = graphql_as(&router, &cookie, &validate_query).await;
    assert_eq!(
        validate_body["data"]["validateReusableResource"]["resource"]["draft"]["validationStatus"],
        "VALID"
    );

    let publish_query = format!(
        "mutation {{ publishReusableResource(input: {{ projectId: \"50000000-0000-0000-0000-000000000001\", resourceId: \"{resource_id}\", expectedRevision: 2 }}) \
            {{ resource {{ currentPublishedVersion }} problems {{ code }} }} }}"
    );
    let publish_body = graphql_as(&router, &cookie, &publish_query).await;
    assert_eq!(
        publish_body["data"]["publishReusableResource"]["resource"]["currentPublishedVersion"],
        1
    );

    // Idempotent republish: same revision, unchanged digest, still version 1.
    let republish_body = graphql_as(&router, &cookie, &publish_query).await;
    assert_eq!(
        republish_body["data"]["publishReusableResource"]["resource"]["currentPublishedVersion"],
        1
    );

    // The generated read: the resource with its current draft and its versions.
    let read_query = format!(
        "{{ reusableResources(filters: {{ id: {{ eq: \"{resource_id}\" }} }}) {{ nodes {{ \
            draft {{ revision validationStatus }} reusableResourceVersions {{ nodes {{ version }} }} }} }} }}"
    );
    let read_body = graphql_as(&router, &cookie, &read_query).await;
    let read = &read_body["data"]["reusableResources"]["nodes"][0];
    assert_eq!(
        read["draft"],
        serde_json::json!({ "revision": 2, "validationStatus": "VALID" })
    );
    assert_eq!(
        read["reusableResourceVersions"]["nodes"],
        serde_json::json!([{ "version": 1 }])
    );

    delete_configuration_test_resource_by_identity(
        &db,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-resource",
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn update_reusable_resource_draft_reports_a_revision_conflict_for_a_stale_expected_revision()
{
    let db = fixture_database().await;
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_configuration_test_resource_by_identity(
        &db,
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
        &db,
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
    let db = fixture_database().await;
    let router = build_test_router().await;
    let owner_cookie = authenticated_cookie(&router).await;
    delete_configuration_test_resource_by_identity(
        &db,
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
        &db,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-forbidden-resource",
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn create_and_update_project_mcp_server_round_trip() {
    let db = fixture_database().await;
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_configuration_test_tool_by_server_id(
        &db,
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
        &db,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-mcp-server",
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn save_project_tool_connection_metadata_creates_and_updates_a_legacy_tool() {
    let db = fixture_database().await;
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_configuration_test_tool_by_server_id(
        &db,
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
        &db,
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
    db: &sea_orm::DatabaseConnection,
    project_id: &str,
    principal_id: &str,
    role_code: hive_persistence::entity::enums::ProjectRoleCode,
) -> uuid::Uuid {
    use hive_persistence::entity::{project_membership_roles, project_memberships};
    use sea_orm::{ActiveModelTrait, Set};

    let membership_id = uuid::Uuid::new_v4();
    project_memberships::ActiveModel {
        id: Set(membership_id),
        project_id: Set(uuid(project_id)),
        principal_id: Set(uuid(principal_id)),
        // `CURRENT_TIMESTAMP` in the deleted statement; an `ActiveValue::Set` takes a value, so
        // this is the service clock, as the ported commands do (plan, "the two service-clock
        // items"). Nothing asserts this instant, only that the membership is open.
        started_at: Set(chrono::Utc::now().into()),
        ended_at: Set(None),
        revision: Set(1),
        active_marker: Set(Some(true)),
    }
    .insert(db)
    .await
    .expect("grant a project membership");
    project_membership_roles::ActiveModel {
        membership_id: Set(membership_id),
        role_code: Set(role_code),
    }
    .insert(db)
    .await
    .expect("grant a project membership role");
    membership_id
}

async fn revoke_project_membership(db: &sea_orm::DatabaseConnection, membership_id: uuid::Uuid) {
    use hive_persistence::entity::{project_membership_roles, project_memberships};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    project_membership_roles::Entity::delete_many()
        .filter(project_membership_roles::Column::MembershipId.eq(membership_id))
        .exec(db)
        .await
        .expect("revoke a project membership role");
    project_memberships::Entity::delete_by_id(membership_id)
        .exec(db)
        .await
        .expect("revoke a project membership");
}

async fn delete_deployment_test_fixtures(db: &sea_orm::DatabaseConnection, agent_id: &str) {
    use hive_persistence::entity::{
        deployment_approval_decisions, deployment_approval_handoff_releases,
        deployment_approval_requirements, deployment_attempts, deployment_audit_events,
        deployment_evidence_invalidations, deployment_evidence_snapshots, deployment_outbox_events,
        deployment_plan_review_facts, deployment_plan_versions, deployment_policy_snapshots,
        deployment_promotion_facts, deployment_recovery_action_receipts, deployment_runtime_health,
        deployment_stage_events, deployment_timeline_counters, deployments,
    };
    use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QuerySelect, QueryTrait};

    let agent = uuid(agent_id);
    let deployment_ids: Vec<uuid::Uuid> = deployments::Entity::find()
        .filter(deployments::Column::AgentId.eq(agent))
        .all(db)
        .await
        .expect("look up deployment test fixtures")
        .into_iter()
        .map(|deployment| deployment.id)
        .collect();
    let expect = "clean up a deployment test fixture table";
    for deployment_id in &deployment_ids {
        let deployment_id = *deployment_id;
        deployment_stage_events::Entity::delete_many()
            .filter(
                deployment_stage_events::Column::DeploymentAttemptId.in_subquery(
                    deployment_attempts::Entity::find()
                        .select_only()
                        .column(deployment_attempts::Column::Id)
                        .filter(deployment_attempts::Column::DeploymentId.eq(deployment_id))
                        .into_query(),
                ),
            )
            .exec(db)
            .await
            .expect(expect);
        deployment_attempts::Entity::delete_many()
            .filter(deployment_attempts::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await
            .expect(expect);
        deployment_audit_events::Entity::delete_many()
            .filter(deployment_audit_events::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await
            .expect(expect);
        deployment_outbox_events::Entity::delete_many()
            .filter(deployment_outbox_events::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await
            .expect(expect);
        deployment_evidence_invalidations::Entity::delete_many()
            .filter(
                deployment_evidence_invalidations::Column::EvidenceSnapshotId.in_subquery(
                    deployment_evidence_snapshots::Entity::find()
                        .select_only()
                        .column(deployment_evidence_snapshots::Column::Id)
                        .filter(
                            deployment_evidence_snapshots::Column::DeploymentId.eq(deployment_id),
                        )
                        .into_query(),
                ),
            )
            .exec(db)
            .await
            .expect(expect);
        deployment_evidence_snapshots::Entity::delete_many()
            .filter(deployment_evidence_snapshots::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await
            .expect(expect);
        deployment_approval_decisions::Entity::delete_many()
            .filter(
                deployment_approval_decisions::Column::ApprovalRequirementId.in_subquery(
                    deployment_approval_requirements::Entity::find()
                        .select_only()
                        .column(deployment_approval_requirements::Column::Id)
                        .filter(
                            deployment_approval_requirements::Column::DeploymentId
                                .eq(deployment_id),
                        )
                        .into_query(),
                ),
            )
            .exec(db)
            .await
            .expect(expect);
        deployment_approval_handoff_releases::Entity::delete_many()
            .filter(deployment_approval_handoff_releases::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await
            .expect(expect);
        deployment_approval_requirements::Entity::delete_many()
            .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await
            .expect(expect);
        deployment_plan_review_facts::Entity::delete_many()
            .filter(
                deployment_plan_review_facts::Column::PlanId.in_subquery(
                    deployment_plan_versions::Entity::find()
                        .select_only()
                        .column(deployment_plan_versions::Column::Id)
                        .filter(deployment_plan_versions::Column::DeploymentId.eq(deployment_id))
                        .into_query(),
                ),
            )
            .exec(db)
            .await
            .expect(expect);
        deployment_plan_versions::Entity::delete_many()
            .filter(deployment_plan_versions::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await
            .expect(expect);
        deployment_policy_snapshots::Entity::delete_many()
            .filter(deployment_policy_snapshots::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await
            .expect(expect);
        deployment_promotion_facts::Entity::delete_many()
            .filter(deployment_promotion_facts::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await
            .expect(expect);
        deployment_recovery_action_receipts::Entity::delete_many()
            .filter(
                Condition::any()
                    .add(
                        deployment_recovery_action_receipts::Column::SourceDeploymentId
                            .eq(deployment_id),
                    )
                    .add(
                        deployment_recovery_action_receipts::Column::ResultDeploymentId
                            .eq(deployment_id),
                    ),
            )
            .exec(db)
            .await
            .expect(expect);
        deployment_runtime_health::Entity::delete_many()
            .filter(deployment_runtime_health::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await
            .expect(expect);
        deployment_timeline_counters::Entity::delete_many()
            .filter(deployment_timeline_counters::Column::DeploymentId.eq(deployment_id))
            .exec(db)
            .await
            .expect(expect);
    }
    deployments::Entity::delete_many()
        .filter(deployments::Column::AgentId.eq(agent))
        .exec(db)
        .await
        .expect("clean up deployment test fixture deployments");
}

#[tokio::test]
#[ignore]
async fn deploy_cancel_and_read_deployment_round_trip() {
    let _guard = lock_project_agents().await;
    let db = fixture_database().await;
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_agent_draft_test_agent_by_slug(
        &db,
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

    // The frozen-inputs preview is the computed field on the generated `AgentVersions` row.
    let preview_query = format!(
        "query {{ agentVersions(filters: {{ id: {{ eq: \"{agent_version_id}\" }} }}) {{ nodes {{ \
            deploymentPreview(environmentDefinitionVersionId: \"{environment_id}\", strategy: \"REPLACE\") \
            {{ strategy risk }} }} }} }}"
    );
    let preview_body = graphql_as(&router, &cookie, &preview_query).await;
    assert_eq!(
        preview_body["data"]["agentVersions"]["nodes"][0]["deploymentPreview"]["strategy"],
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
        &db,
        "50000000-0000-0000-0000-000000000001",
        "00000000-0000-0000-0000-000000000001",
        hive_persistence::entity::enums::ProjectRoleCode::ProjectAdmin,
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

    // The list is the generated `deployments` connection, ordered newest first with the key as the
    // final tie-break.
    let deployments_query = format!(
        "query {{ deployments(filters: {{ projectId: {{ eq: \"50000000-0000-0000-0000-000000000001\" }}, agentId: {{ eq: \"{agent_id}\" }} }}, \
            orderBy: {{ requestedAt: DESC, id: DESC }}, pagination: {{ page: {{ limit: 10, page: 0 }} }}) \
            {{ nodes {{ id lifecycleStatus }} }} }}"
    );
    let deployments_body = graphql_as(&router, &cookie, &deployments_query).await;
    let listed = deployments_body["data"]["deployments"]["nodes"]
        .as_array()
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], deployment_id);

    // The detail is the same generated field filtered by key, with the nested plan, policy,
    // attempt, health and timeline answered by relations and computed fields.
    let detail_query = format!(
        "query {{ deployments(filters: {{ id: {{ eq: \"{deployment_id}\" }} }}, pagination: {{ page: {{ limit: 1, page: 0 }} }}) \
            {{ nodes {{ id lifecycleStatus agents {{ displayName }} agentVersions {{ versionNumber }} \
              environmentDefinitionVersions {{ id stableDefinitionId }} \
              plan {{ planDigest packageDigest review {{ changeSummary }} }} \
              deploymentPolicySnapshots {{ risk requiredEvidence }} \
              deploymentEvidenceSnapshots {{ nodes {{ evidenceKind state }} }} \
              deploymentRuntimeHealth {{ status }} currentAttempt {{ attemptNumber }} rollbackTarget {{ id }} \
              timeline(first: 10) {{ stage status source }} }} }} }}"
    );
    let detail_body = graphql_as(&router, &cookie, &detail_query).await;
    let detail = &detail_body["data"]["deployments"]["nodes"][0];
    assert_eq!(detail["lifecycleStatus"], initial_status);
    assert_eq!(
        detail["environmentDefinitionVersions"]["id"],
        environment_id
    );
    assert_eq!(detail["timeline"][0]["stage"], "REQUESTED");
    assert_eq!(detail["timeline"][0]["source"], "USER");
    assert_eq!(detail["rollbackTarget"], serde_json::Value::Null);
    assert!(detail["plan"]["review"]["changeSummary"].is_string());

    // The environments a version may deploy to are its catalog release's, through the generated
    // relation.
    let environments_query = format!(
        "query {{ agentVersions(filters: {{ id: {{ eq: \"{agent_version_id}\" }} }}, pagination: {{ page: {{ limit: 1, page: 0 }} }}) \
            {{ nodes {{ catalogReleases {{ environmentDefinitionVersions(orderBy: {{ stableDefinitionId: ASC, version: ASC, id: ASC }}, \
              pagination: {{ page: {{ limit: 50, page: 0 }} }}) {{ nodes {{ id logicalEnvironmentClass }} }} }} }} }} }}"
    );
    let environments_body = graphql_as(&router, &cookie, &environments_query).await;
    let environments = environments_body["data"]["agentVersions"]["nodes"][0]["catalogReleases"]
        ["environmentDefinitionVersions"]["nodes"]
        .as_array()
        .unwrap();
    assert!(environments.iter().any(|node| node["id"] == environment_id));

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

    revoke_project_membership(&db, membership_id).await;
    delete_deployment_test_fixtures(&db, &agent_id).await;
    delete_agent_draft_test_agent(&db, &agent_id).await;
}

async fn grant_organization_membership(
    db: &sea_orm::DatabaseConnection,
    organization_id: &str,
    principal_id: &str,
) -> uuid::Uuid {
    use hive_persistence::entity::{
        enums, organization_membership_roles, organization_memberships,
    };
    use sea_orm::{ActiveModelTrait, Set};

    let membership_id = uuid::Uuid::new_v4();
    organization_memberships::ActiveModel {
        id: Set(membership_id),
        organization_id: Set(uuid(organization_id)),
        principal_id: Set(uuid(principal_id)),
        started_at: Set(chrono::Utc::now().into()),
        ended_at: Set(None),
        revision: Set(Some(1)),
        active_marker: Set(Some(true)),
    }
    .insert(db)
    .await
    .expect("grant an organization membership");
    organization_membership_roles::ActiveModel {
        membership_id: Set(membership_id),
        role_code: Set(enums::OrganizationRoleCode::OrganizationMember),
    }
    .insert(db)
    .await
    .expect("grant an organization membership role");
    membership_id
}

async fn revoke_organization_membership(
    db: &sea_orm::DatabaseConnection,
    membership_id: uuid::Uuid,
) {
    use hive_persistence::entity::{organization_membership_roles, organization_memberships};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    organization_membership_roles::Entity::delete_many()
        .filter(organization_membership_roles::Column::MembershipId.eq(membership_id))
        .exec(db)
        .await
        .expect("revoke an organization membership role");
    organization_memberships::Entity::delete_by_id(membership_id)
        .exec(db)
        .await
        .expect("revoke an organization membership");
}

/// Covers the approval read/decision GraphQL surface on top of the already-verified deploy/cancel
/// round trip above: the generated `deploymentApprovalRequirements` list and single-row read,
/// `decideDeploymentApproval`'s self-approval refusal, a real APPROVE decision, idempotent replay,
/// and a stale-revision conflict.
#[tokio::test]
#[ignore]
async fn approval_inbox_and_decide_round_trip() {
    let _guard = lock_project_agents().await;
    let db = fixture_database().await;
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_agent_draft_test_agent_by_slug(
        &db,
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
        &db,
        "50000000-0000-0000-0000-000000000001",
        "00000000-0000-0000-0000-000000000001",
        hive_persistence::entity::enums::ProjectRoleCode::ProjectAdmin,
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
        let inbox_query = "query { deploymentApprovalRequirements(filters: { projectId: { eq: \"50000000-0000-0000-0000-000000000001\" } }, \
            orderBy: { requestedAt: DESC, id: DESC }, pagination: { page: { limit: 20, page: 0 } }) \
            { nodes { id status eligible decisionAvailable deployments { id } } } }";
        let inbox_body = graphql_as(&router, &cookie, inbox_query).await;
        let nodes = inbox_body["data"]["deploymentApprovalRequirements"]["nodes"]
            .as_array()
            .unwrap();
        let entry = nodes
            .iter()
            .find(|node| node["deployments"]["id"] == deployment_id)
            .expect("the new deployment appears in the approval inbox");
        assert_eq!(entry["eligible"], false);
        let requirement_id = entry["id"].as_str().unwrap().to_string();

        let detail_query = format!(
            "query {{ deploymentApprovalRequirements(filters: {{ id: {{ eq: \"{requirement_id}\" }} }}, \
                pagination: {{ page: {{ limit: 1, page: 0 }} }}) {{ nodes {{ eligible decisionAvailable status }} }} }}"
        );
        let detail_body = graphql_as(&router, &cookie, &detail_query).await;
        assert_eq!(
            detail_body["data"]["deploymentApprovalRequirements"]["nodes"][0]["eligible"],
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
            &db,
            "50000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
            hive_persistence::entity::enums::ProjectRoleCode::DeploymentApprover,
        )
        .await;
        let approver_organization_membership = grant_organization_membership(
            &db,
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

        revoke_organization_membership(&db, approver_organization_membership).await;
        revoke_project_membership(&db, approver_project_membership).await;
    }

    revoke_project_membership(&db, membership_id).await;
    delete_deployment_test_fixtures(&db, &agent_id).await;
    delete_agent_draft_test_agent(&db, &agent_id).await;
}

async fn delete_evaluation_test_fixtures(
    db: &sea_orm::DatabaseConnection,
    definition_id: &str,
    agent_id: &str,
) {
    use hive_persistence::entity::{
        agent_versions, enums, evaluation_artifact_metadata, evaluation_audit_events,
        evaluation_case_runs, evaluation_command_receipts, evaluation_definition_drafts,
        evaluation_definition_versions, evaluation_definitions, evaluation_metric_results,
        evaluation_outbox_events, evaluation_results, evaluation_runs,
        evaluation_target_projections, evaluation_target_snapshots,
    };
    use sea_orm::sea_query::JoinType;
    use sea_orm::{
        ColumnTrait, Condition, EntityTrait, QueryFilter, QuerySelect, QueryTrait, RelationTrait,
    };

    let definition = uuid(definition_id);
    // The versions of this definition, the subquery the deleted statements all narrowed by.
    let versions_of_definition = || {
        evaluation_definition_versions::Entity::find()
            .select_only()
            .column(evaluation_definition_versions::Column::Id)
            .filter(evaluation_definition_versions::Column::DefinitionId.eq(definition))
            .into_query()
    };
    let run_ids: Vec<uuid::Uuid> = evaluation_runs::Entity::find()
        .select_only()
        .column(evaluation_runs::Column::Id)
        .join(
            JoinType::InnerJoin,
            evaluation_runs::Relation::EvaluationDefinitionVersions.def(),
        )
        .filter(evaluation_definition_versions::Column::DefinitionId.eq(definition))
        .into_tuple()
        .all(db)
        .await
        .expect("look up evaluation test runs");
    let expect = "clean up an evaluation test run table";
    for run_id in &run_ids {
        let run_id = *run_id;
        evaluation_case_runs::Entity::delete_many()
            .filter(evaluation_case_runs::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_metric_results::Entity::delete_many()
            .filter(evaluation_metric_results::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_artifact_metadata::Entity::delete_many()
            .filter(evaluation_artifact_metadata::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_results::Entity::delete_many()
            .filter(evaluation_results::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_audit_events::Entity::delete_many()
            .filter(evaluation_audit_events::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_outbox_events::Entity::delete_many()
            .filter(evaluation_outbox_events::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_target_snapshots::Entity::delete_many()
            .filter(evaluation_target_snapshots::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
        evaluation_command_receipts::Entity::delete_many()
            .filter(evaluation_command_receipts::Column::RunId.eq(run_id))
            .exec(db)
            .await
            .expect(expect);
    }
    evaluation_runs::Entity::delete_many()
        .filter(evaluation_runs::Column::DefinitionVersionId.in_subquery(versions_of_definition()))
        .exec(db)
        .await
        .expect("clean up evaluation test runs");
    evaluation_command_receipts::Entity::delete_many()
        .filter(
            Condition::any()
                .add(evaluation_command_receipts::Column::DefinitionId.eq(definition))
                .add(
                    evaluation_command_receipts::Column::DefinitionVersionId
                        .in_subquery(versions_of_definition()),
                ),
        )
        .exec(db)
        .await
        .expect("clean up evaluation test command receipts");
    evaluation_audit_events::Entity::delete_many()
        .filter(evaluation_audit_events::Column::DefinitionId.eq(definition))
        .exec(db)
        .await
        .expect("clean up evaluation test definition audit events");
    evaluation_definition_versions::Entity::delete_many()
        .filter(evaluation_definition_versions::Column::DefinitionId.eq(definition))
        .exec(db)
        .await
        .expect("clean up evaluation test definition versions");
    evaluation_definition_drafts::Entity::delete_many()
        .filter(evaluation_definition_drafts::Column::DefinitionId.eq(definition))
        .exec(db)
        .await
        .expect("clean up evaluation test definition draft");
    evaluation_definitions::Entity::delete_by_id(definition)
        .exec(db)
        .await
        .expect("clean up evaluation test definition");
    evaluation_target_projections::Entity::delete_many()
        .filter(
            evaluation_target_projections::Column::TargetKind
                .eq(enums::EvaluationTargetKind::AgentVersion),
        )
        .filter(
            evaluation_target_projections::Column::TargetId.in_subquery(
                agent_versions::Entity::find()
                    .select_only()
                    .column(agent_versions::Column::Id)
                    .filter(agent_versions::Column::AgentId.eq(uuid(agent_id)))
                    .into_query(),
            ),
        )
        .exec(db)
        .await
        .expect("clean up evaluation test target projections");
}

/// Covers the evaluation GraphQL surface end to end against a real published agent version:
/// create/validate/publish a definition, the project's computed `compatibleEvaluationTargets`,
/// `runEvaluation`, driving the local outbox worker in-process (mirroring the `evaluation-worker`
/// subcommand's own delivery path) to completion, then the generated `evaluationRuns` read with
/// its four fact lists, a lifecycle-conflict refusal, and rerun.
#[tokio::test]
#[ignore]
async fn evaluation_definition_and_run_round_trip() {
    let _guard = lock_project_agents().await;
    let db = fixture_database().await;
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    delete_agent_draft_test_agent_by_slug(
        &db,
        "50000000-0000-0000-0000-000000000001",
        "http-integration-evaluation-agent",
    )
    .await;

    let membership_id = grant_project_role(
        &db,
        "50000000-0000-0000-0000-000000000001",
        "00000000-0000-0000-0000-000000000001",
        hive_persistence::entity::enums::ProjectRoleCode::ProjectAdmin,
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
            {{ version {{ id versionNumber }} problems {{ code }} }} }}"
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

    // The candidate targets of a published version are the computed field on the project, over
    // the generated `evaluationTargetProjections` rows.
    let targets_query = format!(
        "query {{ projects(filters: {{ id: {{ eq: \"50000000-0000-0000-0000-000000000001\" }} }}) \
            {{ nodes {{ compatibleEvaluationTargets(definitionVersionId: \"{version_id}\") {{ targetKind targetId }} }} }} }}"
    );
    let targets_body = graphql_as(&router, &cookie, &targets_query).await;
    let targets = targets_body["data"]["projects"]["nodes"][0]["compatibleEvaluationTargets"]
        .as_array()
        .unwrap();
    assert!(targets
        .iter()
        .any(|target| target["targetKind"] == "AGENT_VERSION"
            && target["targetId"] == agent_version_id));

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

    // The run and its four fact lists, every one a generated entity read ordered by its own key.
    let run_detail_query = format!(
        "query {{ evaluationRuns(filters: {{ id: {{ eq: \"{run_id}\" }} }}) \
            {{ nodes {{ id lifecycleStatus outcomeCategory generation durationMillis deploymentEvidenceDisposition \
                target {{ agentContentDigest }} }} }} \
          evaluationCaseRuns(filters: {{ runId: {{ eq: \"{run_id}\" }} }}, orderBy: {{ ordinal: ASC, id: ASC }}) {{ nodes {{ caseKey passed }} }} \
          evaluationMetricResults(filters: {{ runId: {{ eq: \"{run_id}\" }} }}, orderBy: {{ metricCode: ASC, id: ASC }}) {{ nodes {{ metricCode value passed }} }} \
          evaluationArtifactMetadata(filters: {{ runId: {{ eq: \"{run_id}\" }} }}, orderBy: {{ artifactKind: ASC, id: ASC }}) {{ nodes {{ artifactKind }} }} \
          evaluationAuditEvents(filters: {{ runId: {{ eq: \"{run_id}\" }} }}, orderBy: {{ occurredAt: DESC, id: DESC }}) {{ nodes {{ action summary }} }} }}"
    );
    let run_detail_body = graphql_as(&router, &cookie, &run_detail_query).await;
    let data = &run_detail_body["data"];
    let run_detail = &data["evaluationRuns"]["nodes"][0];
    assert_eq!(run_detail["lifecycleStatus"], "COMPLETED");
    assert_eq!(run_detail["outcomeCategory"], "PASSED");
    assert_eq!(
        run_detail["deploymentEvidenceDisposition"],
        "NOT_A_DEPLOYMENT"
    );
    assert!(run_detail["target"]["agentContentDigest"].is_string());
    assert_eq!(data["evaluationCaseRuns"]["nodes"][0]["passed"], true);
    assert_eq!(data["evaluationMetricResults"]["nodes"][0]["passed"], true);
    assert_eq!(
        data["evaluationArtifactMetadata"]["nodes"][0]["artifactKind"],
        "LOCAL_SUMMARY"
    );
    assert!(!data["evaluationAuditEvents"]["nodes"]
        .as_array()
        .unwrap()
        .is_empty());
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

    {
        use sea_orm::EntityTrait;
        hive_persistence::entity::evaluation_worker_heartbeats::Entity::delete_by_id(
            "http-integration-evaluation-worker".to_string(),
        )
        .exec(&db)
        .await
        .expect("clean up the in-process evaluation worker's heartbeat row");
    }
    revoke_project_membership(&db, membership_id).await;
    delete_evaluation_test_fixtures(&db, &definition_id, &agent_id).await;
    delete_deployment_test_fixtures(&db, &agent_id).await;
    delete_agent_draft_test_agent(&db, &agent_id).await;
}

/// The stored `source_ip`, `user_agent` and `required_capability` columns are not part of the
/// generated API: no principal can select, filter or order on them. `sourceIp` and `userAgent`
/// exist only as the computed fields that `AUDIT_SENSITIVE.VIEW` gates.
#[tokio::test]
#[ignore]
async fn audit_events_do_not_expose_the_raw_sensitive_columns() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    for query in [
        "query { auditEventProjection(filters: { sourceIp: { eq: \"127.0.0.1\" } }) { nodes { projectionId } } }",
        "query { auditEventProjection(filters: { userAgent: { eq: \"x\" } }) { nodes { projectionId } } }",
        "query { auditEventProjection(filters: { requiredCapability: { eq: \"AGENT.VIEW\" } }) { nodes { projectionId } } }",
        "query { auditEventProjection(orderBy: { sourceIp: ASC }) { nodes { projectionId } } }",
        "query { auditEventProjection(orderBy: { userAgent: ASC }) { nodes { projectionId } } }",
        "query { auditEventProjection { nodes { requiredCapability } } }",
    ] {
        let body = graphql_as(&router, &cookie, query).await;
        assert!(
            body["errors"][0]["message"].is_string(),
            "{query} must be refused: {body}"
        );
        assert!(body.get("data").is_none_or(serde_json::Value::is_null));
    }
}

/// The stored evaluation document columns and an evaluation audit event's raw material are not
/// part of the generated API: no principal can select, filter or order on them. `canonicalDocument`
/// and `diagnostics` exist only as the computed fields `EVALUATION_DEFINITION.AUTHOR` gates, and
/// `summary` as the one fact a run's audit list ever showed.
#[tokio::test]
#[ignore]
async fn evaluation_reads_do_not_expose_the_raw_document_or_fact_columns() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    for query in [
        "query { evaluationDefinitionDrafts(filters: { canonicalDocument: { eq: \"{}\" } }) { nodes { revision } } }",
        "query { evaluationDefinitionDrafts(orderBy: { canonicalDocument: ASC }) { nodes { revision } } }",
        "query { evaluationDefinitionVersions(orderBy: { canonicalDocument: ASC }) { nodes { id } } }",
        "query { evaluationAuditEvents { nodes { facts } } }",
        "query { evaluationAuditEvents { nodes { sourceIp } } }",
        "query { evaluationAuditEvents { nodes { userAgent } } }",
    ] {
        let body = graphql_as(&router, &cookie, query).await;
        assert!(
            body["errors"][0]["message"].is_string(),
            "{query} must be refused: {body}"
        );
        assert!(body.get("data").is_none_or(serde_json::Value::is_null));
    }
}

/// A principal with no evaluation grant reads no definition, run or fact row, and is offered no
/// candidate target. Bea (00000000-...-0002) holds no role on organization 10000000-...-0001.
#[tokio::test]
#[ignore]
async fn evaluation_reads_are_empty_for_a_principal_without_an_evaluation_grant() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;
    for field in [
        "evaluationDefinitions",
        "evaluationDefinitionDrafts",
        "evaluationDefinitionVersions",
        "evaluationRuns",
        "evaluationCaseRuns",
        "evaluationMetricResults",
        "evaluationArtifactMetadata",
        "evaluationAuditEvents",
        "evaluationTargetSnapshots",
        "evaluationTargetProjections",
    ] {
        let body = graphql_as(
            &router,
            &cookie,
            &format!("query {{ {field}(pagination: {{ page: {{ limit: 10, page: 0 }} }}) {{ nodes {{ __typename }} }} }}"),
        )
        .await;
        assert!(body.get("errors").is_none(), "{body}");
        assert_eq!(
            body["data"][field]["nodes"],
            serde_json::json!([]),
            "{body}"
        );
    }
    let targets = graphql_as(
        &router,
        &cookie,
        "query { projects(filters: { id: { eq: \"50000000-0000-0000-0000-000000000001\" } }) \
            { nodes { compatibleEvaluationTargets(definitionVersionId: \"00000000-0000-0000-0000-000000000000\") { targetId } } } }",
    )
    .await;
    assert!(targets.get("errors").is_none(), "{targets}");
    assert_eq!(
        targets["data"]["projects"]["nodes"],
        serde_json::json!([]),
        "{targets}"
    );
}

/// A principal with no audit grant reads no event, whatever it filters by. Bea
/// (00000000-...-0002) holds no role on organization 10000000-...-0001.
#[tokio::test]
#[ignore]
async fn audit_events_are_empty_for_a_principal_without_an_audit_grant() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;

    for filters in [
        "{ organizationId: { eq: \"10000000-0000-0000-0000-000000000001\" }, occurredAt: { gte: \"2020-01-01T00:00:00Z\" } }",
        "{ projectId: { eq: \"50000000-0000-0000-0000-000000000001\" } }",
        "{}",
    ] {
        let body = graphql_as(
            &router,
            &cookie,
            &format!(
                "query {{ auditEventProjection(filters: {filters}, pagination: {{ page: {{ limit: 10, page: 0 }} }}) \
                    {{ nodes {{ projectionId }} paginationInfo {{ total }} }} }}"
            ),
        )
        .await;
        assert!(body.get("errors").is_none(), "{body}");
        assert_eq!(
            body["data"]["auditEventProjection"]["nodes"],
            serde_json::json!([])
        );
        assert_eq!(
            body["data"]["auditEventProjection"]["paginationInfo"]["total"],
            0
        );
    }
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
    use hive_persistence::entity::{
        administration_audit_events, enums, platform_role_assignments, projects,
    };
    use sea_orm::sea_query::Expr;
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

    let db = fixture_database().await;
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

    let event = administration_audit_events::Entity::find()
        .filter(
            administration_audit_events::Column::ScopeId
                .eq(uuid("50000000-0000-0000-0000-000000000001")),
        )
        .filter(administration_audit_events::Column::Action.eq("PROJECT_GENERAL_UPDATED"))
        .order_by_desc(administration_audit_events::Column::OccurredAt)
        .one(&db)
        .await
        .expect("read the administration audit events")
        .expect("find the newly written administration audit event");
    let event_id = event.id;
    let request_id = event
        .request_id
        .expect("a command's audit row binds its request metadata");

    let event_query = format!(
        "query {{ auditEventProjection(filters: {{ organizationId: {{ eq: \"10000000-0000-0000-0000-000000000001\" }}, \
            sourceKind: {{ eq: \"ADMINISTRATION\" }}, sourceEventId: {{ eq: \"{event_id}\" }}, \
            projectionId: {{ eq: \"administration:{event_id}\" }} }}) \
            {{ nodes {{ projectionId requestId correlationId graphqlOperation sourceIp userAgent sensitiveFieldsRedacted }} }} }}"
    );
    let unprivileged = graphql_as(&router, &cookie, &event_query).await;
    let node = &unprivileged["data"]["auditEventProjection"]["nodes"][0];
    assert_eq!(node["projectionId"], format!("administration:{event_id}"));
    assert_eq!(node["requestId"], request_id.to_string());
    assert_eq!(node["correlationId"], request_id.to_string());
    assert_eq!(node["graphqlOperation"], "TouchProjectGeneralForAudit");
    assert_eq!(node["sourceIp"], serde_json::Value::Null);
    assert_eq!(node["userAgent"], serde_json::Value::Null);
    assert_eq!(node["sensitiveFieldsRedacted"], true);

    // Ada is a platform administrator for the next few statements. The deployment, approval and
    // evaluation round trips assert what she may do on this project without that role and hold
    // this lock while they do, so the grant waits for them and they wait for the revoke.
    let authority_guard = lock_project_agents().await;
    platform_role_assignments::ActiveModel {
        principal_id: Set(uuid("00000000-0000-0000-0000-000000000001")),
        role_code: Set(enums::PlatformRoleCode::PlatformAdmin),
    }
    .insert(&db)
    .await
    .expect("grant platform admin");

    let privileged = graphql_as(&router, &cookie, &event_query).await;
    let node = &privileged["data"]["auditEventProjection"]["nodes"][0];
    assert_eq!(node["sourceIp"], "127.0.0.1");
    assert_eq!(node["userAgent"], "hive-http-integration/1.0");
    assert_eq!(node["sensitiveFieldsRedacted"], false);

    platform_role_assignments::Entity::delete_by_id((
        uuid("00000000-0000-0000-0000-000000000001"),
        enums::PlatformRoleCode::PlatformAdmin,
    ))
    .exec(&db)
    .await
    .expect("revoke platform admin");
    drop(authority_guard);
    administration_audit_events::Entity::delete_by_id(event_id)
        .exec(&db)
        .await
        .expect("clean up the test audit event");
    projects::Entity::update_many()
        .col_expr(projects::Column::Revision, Expr::value(1_i64))
        .filter(projects::Column::Id.eq(uuid("50000000-0000-0000-0000-000000000001")))
        .filter(projects::Column::Revision.eq(2_i64))
        .exec(&db)
        .await
        .expect("restore the project revision");
}
