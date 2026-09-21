//! What the console reads to render itself: the computed `capabilities` field on principals,
//! organizations and projects, the requester's display preferences, and the project dashboard
//! projection.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test console -- --ignored

mod common;

use common::*;

// The console context is generated reads: `principals` (the requester only), `organizations` with
// their `projects`, and the computed `capabilities` field on each (the codes the requesting
// principal holds at that scope). Ada is ORGANIZATION_ADMIN on Product (10000000-...-0001), which
// owns an ACTIVE project (Customer Feedback Copilot) and an ARCHIVED one (Usage Analytics), and
// plain ORGANIZATION_MEMBER on Support and Quality Assurance. Beatrice belongs to none of them.

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
