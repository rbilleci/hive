//! The Seaography-generated read tier: relation traversal, filters, ordering, page pagination and
//! the tenant rule that decides which rows a principal sees at all. Every test here reads; none
//! writes, apart from the published-version fixture the agent-versions test sets up and removes.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test generated_reads -- --ignored

mod common;

use common::*;

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
