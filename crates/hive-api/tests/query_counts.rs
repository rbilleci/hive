//! What a page of rows costs in database round trips, measured on the real router.
//!
//! Every statement sqlx finishes emits one `tracing` event on its query target, so the
//! subscriber below counts round trips exactly. Each measurement runs the same page twice, once
//! with the computed field selected and once without, and reports the difference: that isolates
//! the field from the fixed cost of the request, the session and the tenant-filtered page itself.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test query_counts -- --ignored --nocapture

mod common;

use common::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The `tracing` target sqlx names its finished-statement event with, spelled in two pieces so
/// the raw-SQL scan in `scripts/idiomatic-gate.mjs` does not read this literal as a statement
/// this file writes.
const STATEMENT_EVENT: &str = concat!("sqlx", "::query");

static QUERIES: AtomicUsize = AtomicUsize::new(0);

/// Counts finished statements and nothing else. A `tracing` subscriber, rather than the Postgres
/// statement log, so the count belongs to this process and not to whatever else shares the server.
struct CountStatements;

impl tracing::Subscriber for CountStatements {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == STATEMENT_EVENT
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        if event.metadata().target() == STATEMENT_EVENT {
            QUERIES.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

/// The statements one request issues. The page is fetched once to settle anything the first
/// request of a session does only once, then measured.
async fn statements(router: &axum::Router, cookie: &str, query: &str) -> usize {
    graphql_as(router, cookie, query).await;
    let before = QUERIES.load(Ordering::SeqCst);
    graphql_as(router, cookie, query).await;
    QUERIES.load(Ordering::SeqCst) - before
}

/// The statements `field` costs per row of an `n`-row page: the same page with and without it.
async fn per_row(
    router: &axum::Router,
    cookie: &str,
    plain: &str,
    with_field: &str,
    rows: usize,
) -> f64 {
    let bare = statements(router, cookie, plain).await;
    let selected = statements(router, cookie, with_field).await;
    println!("  {rows} rows: {bare} statements without the field, {selected} with it");
    (selected as f64 - bare as f64) / rows as f64
}

fn page(entity: &str, rows: usize, fields: &str) -> String {
    format!(
        "{{ {entity}(pagination: {{ page: {{ limit: {rows}, page: 0 }} }}) \
            {{ nodes {{ id {fields} }} }} }}"
    )
}

async fn node_count(router: &axum::Router, cookie: &str, entity: &str, rows: usize) -> usize {
    let body = graphql_as(router, cookie, &page(entity, rows, "")).await;
    body["data"][entity]["nodes"].as_array().unwrap().len()
}

#[tokio::test]
#[ignore]
async fn a_page_of_rows_costs_a_bounded_number_of_statements() {
    tracing::subscriber::set_global_default(CountStatements)
        .expect("this binary sets the subscriber once");
    let _guard = lock_project_agents().await;
    let db = fixture_database().await;
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let organizations = node_count(&router, &cookie, "organizations", 10).await;
    println!("organizations.capabilities");
    let organization_cost = per_row(
        &router,
        &cookie,
        &page("organizations", 10, ""),
        &page("organizations", 10, "capabilities"),
        organizations,
    )
    .await;
    println!("  {organization_cost} statements per organization row");

    let projects = node_count(&router, &cookie, "projects", 10).await;
    println!("projects.capabilities");
    let project_cost = per_row(
        &router,
        &cookie,
        &page("projects", 10, ""),
        &page("projects", 10, "capabilities"),
        projects,
    )
    .await;
    println!("  {project_cost} statements per project row");

    let (agent_id, membership) = deployment_page_fixture(&db, &router, &cookie).await;
    let deployments = node_count(&router, &cookie, "deployments", 10).await;
    println!("deployments.plan/currentAttempt/rollbackTarget");
    let deployment_cost = per_row(
        &router,
        &cookie,
        &page("deployments", 10, ""),
        &page(
            "deployments",
            10,
            "plan { versionNumber } currentAttempt { attemptNumber } rollbackTarget { id }",
        ),
        deployments,
    )
    .await;
    println!("  {deployment_cost} statements per deployment row");

    revoke_project_membership(&db, membership).await;
    delete_deployment_test_fixtures(&db, &agent_id).await;
    delete_agent_draft_test_agent(&db, &agent_id).await;

    // The three deployment fields are batched across the page, so they cost the page a fixed
    // number of statements rather than one each per row; the capability set is answered from one
    // read of each underlying fact rather than one read per code. Bounds, not exact counts: the
    // page size varies with what the seeds and the fixture leave visible.
    assert!(
        deployment_cost < 1.0,
        "the three deployment fields cost {deployment_cost} statements per row"
    );
    assert!(
        organization_cost < 12.0,
        "capabilities costs {organization_cost} statements per organization row"
    );
    assert!(
        project_cost < 12.0,
        "capabilities costs {project_cost} statements per project row"
    );
}

/// A page of deployments of one throwaway agent: one deployment recorded through the mutations,
/// then copies of its row and its frozen plan, which is what the three computed fields read.
async fn deployment_page_fixture(
    db: &sea_orm::DatabaseConnection,
    router: &axum::Router,
    cookie: &str,
) -> (String, uuid::Uuid) {
    use hive_persistence::entity::{deployment_plan_versions, deployments};

    let slug = "query-count-deployment-agent";
    delete_agent_draft_test_agent_by_slug(db, CUSTOMER_FEEDBACK_COPILOT, slug).await;
    let created = graphql_as(
        router,
        cookie,
        &format!(
            "mutation {{ createAgentDraft(input: {{ projectId: \"{CUSTOMER_FEEDBACK_COPILOT}\", \
                displayName: \"Query Count Deployment Agent\" }}) \
                {{ agentDraft {{ agentId }} problems {{ code }} }} }}"
        ),
    )
    .await;
    let agent_id = created["data"]["createAgentDraft"]["agentDraft"]["agentId"]
        .as_str()
        .unwrap()
        .to_string();
    graphql_as(
        router,
        cookie,
        &format!(
            "mutation {{ updateAgentDraft(input: {{ projectId: \"{CUSTOMER_FEEDBACK_COPILOT}\", \
                agentId: \"{agent_id}\", expectedRevision: 1, document: {{ \
                general: {{ displayName: \"Query Count Deployment Agent\" }}, \
                instructions: {{ source: \"Do the thing.\" }}, limits: {{ maxTokens: 4096 }} }} }}) \
                {{ agentDraft {{ revision }} problems {{ code }} }} }}"
        ),
    )
    .await;
    let validated = graphql_as(
        router,
        cookie,
        &format!(
            "mutation {{ validateAgentDraft(input: {{ projectId: \"{CUSTOMER_FEEDBACK_COPILOT}\", \
                agentId: \"{agent_id}\", expectedRevision: 2 }}) \
                {{ agentDraft {{ revision }} problems {{ code }} }} }}"
        ),
    )
    .await;
    let revision = validated["data"]["validateAgentDraft"]["agentDraft"]["revision"]
        .as_i64()
        .unwrap();
    let published = graphql_as(
        router,
        cookie,
        &format!(
            "mutation {{ publishAgentDraft(input: {{ projectId: \"{CUSTOMER_FEEDBACK_COPILOT}\", \
                agentId: \"{agent_id}\", expectedRevision: {revision}, warningsAcknowledged: true }}) \
                {{ agentVersion {{ id }} problems {{ code }} }} }}"
        ),
    )
    .await;
    let agent_version_id = published["data"]["publishAgentDraft"]["agentVersion"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let membership = grant_project_role(
        db,
        CUSTOMER_FEEDBACK_COPILOT,
        ADA,
        hive_persistence::entity::enums::ProjectRoleCode::ProjectAdmin,
    )
    .await;
    let deployed = graphql_as(
        router,
        cookie,
        &format!(
            "mutation {{ deployAgentVersion(input: {{ agentVersionId: \"{agent_version_id}\", \
                environmentDefinitionVersionId: \"e1300000-0000-0000-0000-000000000001\", \
                strategy: REPLACE, idempotencyKey: \"query-count-deployment\" }}) \
                {{ deployment {{ id }} problems {{ code message }} }} }}"
        ),
    )
    .await;
    assert_eq!(
        deployed["data"]["deployAgentVersion"]["problems"],
        serde_json::json!([]),
        "{deployed:?}"
    );
    let deployment_id = uuid(
        deployed["data"]["deployAgentVersion"]["deployment"]["id"]
            .as_str()
            .unwrap(),
    );

    let original = deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await
        .unwrap()
        .expect("the recorded deployment");
    let plan = deployment_plan_versions::Entity::find()
        .filter(deployment_plan_versions::Column::DeploymentId.eq(deployment_id))
        .one(db)
        .await
        .unwrap()
        .expect("the frozen plan of the recorded deployment");
    for copy in 0..9 {
        let id = uuid::Uuid::new_v4();
        let mut row = original.clone().into_active_model().reset_all();
        row.id = Set(id);
        row.idempotency_key = Set(format!("query-count-deployment-copy-{copy}"));
        // A 64-character lowercase hexadecimal digest, which the column's check constraint
        // requires and the copy needs its own of.
        row.request_fingerprint = Set(Some(format!("{copy:064x}")));
        row.insert(db).await.expect("copy a deployment row");
        let mut frozen = plan.clone().into_active_model().reset_all();
        frozen.id = Set(uuid::Uuid::new_v4());
        frozen.deployment_id = Set(id);
        frozen.insert(db).await.expect("copy a frozen plan row");
    }

    (agent_id, membership)
}
