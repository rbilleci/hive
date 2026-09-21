//! The harness the per-domain test binaries in this directory share: the router under test,
//! the session cookie, the two GraphQL senders, the seeded identifiers and the fixture helpers
//! that more than one binary needs.
//!
//! Cargo compiles each file in `tests/` as its own binary and runs those binaries one at a time,
//! so the whole suite still meets one live database sequentially. Within a binary tests run
//! concurrently, which is what the two locks below and `build_test_router`'s migration queue
//! serialise.

#![allow(dead_code)] // Each binary uses a different part of this module.

use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::net::{Ipv4Addr, SocketAddr};
use tower::ServiceExt;

pub fn test_database_url() -> String {
    std::env::var("HIVE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hive:hive@127.0.0.1:15432/hive".to_string())
}

/// The connection the fixture helpers use to set up and tear down rows, separate from the
/// router's own pool. They read and write through the same SeaORM entity modules the service
/// does (`hive_persistence::entity`), so a schema change breaks them at compile time.
pub async fn fixture_database() -> sea_orm::DatabaseConnection {
    sea_orm::Database::connect(test_database_url())
        .await
        .expect("connect to test database")
}

/// The identifiers in these tests are written as literal strings, as they are inside the GraphQL
/// documents next to them; the entity columns are typed `Uuid`.
pub fn uuid(value: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(value).expect("a fixture identifier is a UUID")
}

/// Tests within one binary run concurrently and share one live database. Every test that lists
/// Product's (10000000-...-0001) projects asserts an exact slug/count set, so a test that
/// creates (even temporarily) a project under Product must serialize against them — otherwise a
/// listing test can observe the extra row mid-run. Acquired by both sides: the project-listing
/// tests in `generated_reads.rs` and the two tests in `administration.rs` that create a
/// throwaway project. Those sit in different binaries, which cargo runs one after another, so
/// the two sides never overlap; the lock is what holds inside each binary.
pub static PRODUCT_PROJECTS_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

pub async fn lock_product_projects() -> tokio::sync::MutexGuard<'static, ()> {
    PRODUCT_PROJECTS_LOCK.lock().await
}

/// Same concern, scoped to project 50000000-...-0001's (Customer Feedback Copilot) exact agent
/// list/count and to Beatrice's momentary Product membership: acquired by the agent-listing and
/// organization-by-id tests in `generated_reads.rs`, and by every test that temporarily creates
/// a throwaway agent under that project or grants a throwaway membership — in `agent.rs`,
/// `deployment.rs`, `approval.rs`, `evaluation.rs` and `audit.rs`.
pub static PROJECT_AGENTS_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

pub async fn lock_project_agents() -> tokio::sync::MutexGuard<'static, ()> {
    PROJECT_AGENTS_LOCK.lock().await
}

pub async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

pub async fn authenticated_cookie(router: &axum::Router) -> String {
    authenticated_cookie_for(router, "00000000-0000-0000-0000-000000000001").await
}

pub async fn authenticated_cookie_for(router: &axum::Router, principal: &str) -> String {
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

pub async fn build_test_router() -> axum::Router {
    // A plain eager connection here, separate from `connect_test_dynamic`'s deliberately lazy
    // one, since migrating issues real DDL immediately.
    // `max_connections(1)`: this handle only ever migrates, sequentially, then is dropped. Both
    // bounds below were measured while every test lived in one binary, so the pressure they
    // answer is at most what a binary still applies: the default pool size multiplied by the
    // whole suite's 65 concurrent tests exhausted Postgres's connection limit the first time this
    // was eager and unbounded (`ConnectionAcquire(Timeout)` from every one of them).
    // Tests in a binary start together and each re-runs the migrator and seeds (which is also
    // what resets seed state between them). Queue them here, in-process, where waiting has no
    // deadline; queued instead on the migrator's own database lock, the last of those 65 hit its
    // 30 s timeout.
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

pub async fn generated(router: &axum::Router, cookie: &str, query: &str) -> serde_json::Value {
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

pub async fn graphql_as(router: &axum::Router, cookie: &str, query: &str) -> serde_json::Value {
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

pub const PRODUCT: &str = "10000000-0000-0000-0000-000000000001";
pub const CUSTOMER_FEEDBACK_COPILOT: &str = "50000000-0000-0000-0000-000000000001";
pub const FEEDBACK_TRIAGE_AGENT: &str = "60000000-0000-0000-0000-000000000001";
pub const BEATRICE: &str = "00000000-0000-0000-0000-000000000002";

pub const ADA: &str = "00000000-0000-0000-0000-000000000001";

/// Defensive pre-cleanup: a prior run of the round trip test that panicked
/// before reaching its own cleanup call leaves its fixed slug taken, which
/// would otherwise fail every subsequent run at `createAgentDraft` with
/// `INVALID_DOCUMENT` forever.
pub async fn delete_agent_draft_test_agent_by_slug(
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

pub async fn delete_agent_draft_test_agent(db: &sea_orm::DatabaseConnection, agent_id: &str) {
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

/// Deployment write capabilities (`DEPLOYMENT.REQUEST`/`CANCEL`/`PROMOTE`/`ROLLBACK`) require a
/// project-level `PROJECT_ADMIN`/`AGENT_DEVELOPER`/`OPERATOR` role, unlike agent authoring which an
/// organization admin can already reach: see `deployment_capabilities` in
/// `hive-persistence/src/capability/tx.rs`. Its callers grant this explicitly rather than relying
/// on principal 1's seeded `ORGANIZATION_ADMIN` role, which only grants `DEPLOYMENT.VIEW`.
pub async fn grant_project_role(
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
        // An `ActiveValue::Set` takes a value, so this is the service clock rather than
        // `CURRENT_TIMESTAMP`. Nothing asserts this instant, only that the membership is open.
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

pub async fn revoke_project_membership(
    db: &sea_orm::DatabaseConnection,
    membership_id: uuid::Uuid,
) {
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

pub async fn delete_deployment_test_fixtures(db: &sea_orm::DatabaseConnection, agent_id: &str) {
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
