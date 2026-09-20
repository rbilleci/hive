//! Requires a live, empty PostgreSQL database named by `HIVE_TEST_DATABASE_URL`
//! (falls back to `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-persistence --test capability_integration -- --ignored
//!
//! Exercises `hive_persistence::capability` against the real seeded fixtures in
//! `db/seed/organization-directory.sql` and `db/seed/organization-project-administration.sql`:
//! Ada Lovelace (00000000-...-0001) holds ORGANIZATION_ADMIN + ORGANIZATION_MEMBER
//! on organization 10000000-...-0001 (Product), ORGANIZATION_MEMBER only on
//! 10000000-...-0002 (Support) and 10000000-...-0003 (Quality Assurance, archived),
//! and an AGENT_DEVELOPER console_role_assignments row (the legacy table, not
//! project_membership_roles) for project 50000000-...-0001, which belongs to
//! organization 0001. Beatrice Hopper (00000000-...-0002) holds ORGANIZATION_MEMBER
//! only on 10000000-...-0004 (SRE). No principal is seeded as PLATFORM_ADMIN.

use hive_persistence::capability;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

fn ada() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
}

fn beatrice() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap()
}

fn product_org() -> Uuid {
    Uuid::parse_str("10000000-0000-0000-0000-000000000001").unwrap()
}

fn support_org() -> Uuid {
    Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap()
}

fn quality_assurance_org() -> Uuid {
    Uuid::parse_str("10000000-0000-0000-0000-000000000003").unwrap()
}

fn feedback_copilot_project() -> Uuid {
    Uuid::parse_str("50000000-0000-0000-0000-000000000001").unwrap()
}

async fn migrated_pool() -> PgPool {
    let url = std::env::var("HIVE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hive:hive@127.0.0.1:15432/hive".to_string());
    let pool = PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("connect to the test database");
    hive_persistence::migrate_and_seed(&pool)
        .await
        .expect("migrate the test database");
    pool
}

#[tokio::test]
#[ignore]
async fn organization_admin_grants_administration_capabilities_but_only_on_that_organization() {
    let pool = migrated_pool().await;

    assert!(
        capability::has_capability(
            &pool,
            ada(),
            "ORGANIZATION.UPDATE",
            capability::Scope::Organization(product_org()),
            false
        )
        .await
        .unwrap(),
        "Ada is ORGANIZATION_ADMIN on the Product organization"
    );
    assert!(
        !capability::has_capability(
            &pool,
            ada(),
            "ORGANIZATION.UPDATE",
            capability::Scope::Organization(support_org()),
            false
        )
        .await
        .unwrap(),
        "Ada is only ORGANIZATION_MEMBER on Support, not an admin"
    );
}

#[tokio::test]
#[ignore]
async fn plain_membership_grants_view_but_not_update() {
    let pool = migrated_pool().await;

    assert!(
        capability::has_capability(
            &pool,
            ada(),
            "ORGANIZATION.VIEW",
            capability::Scope::Organization(support_org()),
            false
        )
        .await
        .unwrap(),
        "any active membership grants ORGANIZATION.VIEW"
    );
    assert!(
        capability::has_capability(
            &pool,
            ada(),
            "ORGANIZATION.VIEW",
            capability::Scope::Organization(quality_assurance_org()),
            false
        )
        .await
        .unwrap(),
        "membership grants ORGANIZATION.VIEW regardless of the organization's own lifecycle status"
    );
}

#[tokio::test]
#[ignore]
async fn a_principal_with_no_membership_sees_nothing() {
    let pool = migrated_pool().await;

    assert!(
        !capability::has_capability(
            &pool,
            beatrice(),
            "ORGANIZATION.VIEW",
            capability::Scope::Organization(product_org()),
            false
        )
        .await
        .unwrap(),
        "Beatrice has no membership in Product"
    );
}

#[tokio::test]
#[ignore]
async fn no_seeded_principal_is_a_platform_administrator() {
    let pool = migrated_pool().await;
    assert!(!capability::is_platform_administrator(&pool, ada())
        .await
        .unwrap());
    assert!(!capability::is_platform_administrator(&pool, beatrice())
        .await
        .unwrap());
}

#[tokio::test]
#[ignore]
async fn console_role_assignments_grant_agent_draft_update_through_legacy_or_developer() {
    let pool = migrated_pool().await;

    assert!(
        capability::has_capability(
            &pool,
            ada(),
            "AGENT_DRAFT.UPDATE",
            capability::Scope::Project(feedback_copilot_project()),
            false
        )
        .await
        .unwrap(),
        "Ada holds a legacy console_role_assignments AGENT_DEVELOPER row for this project"
    );
    assert!(
        !capability::has_capability(
            &pool,
            beatrice(),
            "AGENT_DRAFT.UPDATE",
            capability::Scope::Project(feedback_copilot_project()),
            false
        )
        .await
        .unwrap(),
        "Beatrice has no membership in the Product organization this project belongs to"
    );
}

#[tokio::test]
#[ignore]
async fn organization_admin_inherits_project_view_on_that_organizations_projects() {
    let pool = migrated_pool().await;
    assert!(
        capability::has_capability(
            &pool,
            ada(),
            "PROJECT.VIEW",
            capability::Scope::Project(feedback_copilot_project()),
            false
        )
        .await
        .unwrap(),
        "Ada is ORGANIZATION_ADMIN on Product, which owns this project"
    );
}

#[tokio::test]
#[ignore]
async fn preferences_update_is_scoped_to_the_principals_own_id_only() {
    let pool = migrated_pool().await;
    assert!(
        capability::has_capability(
            &pool,
            ada(),
            "PREFERENCES.UPDATE",
            capability::Scope::Principal(ada()),
            false
        )
        .await
        .unwrap(),
        "a principal may always update their own preferences"
    );
    assert!(
        !capability::has_capability(
            &pool,
            ada(),
            "PREFERENCES.UPDATE",
            capability::Scope::Principal(beatrice()),
            false
        )
        .await
        .unwrap(),
        "a principal may never update someone else's preferences"
    );
}

#[tokio::test]
#[ignore]
async fn an_unrecognized_capability_is_always_denied() {
    let pool = migrated_pool().await;
    assert!(!capability::has_capability(
        &pool,
        ada(),
        "NOT_A_REAL_CAPABILITY",
        capability::Scope::Organization(product_org()),
        false
    )
    .await
    .unwrap());
}

#[tokio::test]
#[ignore]
async fn evaluation_capabilities_are_empty_for_a_nonexistent_project() {
    let pool = migrated_pool().await;
    let capabilities = capability::evaluation_capabilities(&pool, ada(), Uuid::new_v4(), false)
        .await
        .unwrap();
    assert!(capabilities.is_empty());
}

#[tokio::test]
#[ignore]
async fn deployment_capabilities_many_returns_one_entry_per_project() {
    let pool = migrated_pool().await;
    let projects = vec![feedback_copilot_project()];
    let result = capability::deployment_capabilities_many(&pool, ada(), &projects)
        .await
        .unwrap();
    assert_eq!(result.len(), 1);
    assert!(result.contains_key(&feedback_copilot_project()));
}

// The lock=true path (FOR UPDATE / FOR KEY SHARE) is otherwise untested: every
// query field resolver built so far reads with lock=false. These smoke-test that
// every lock query is valid SQL and still produces the correct boolean answer.
// A lock taken against a pool (autocommit) releases immediately after its own
// statement, same as every unlocked call here; the point of `lock=true` is to
// hold the row across the *caller's* later write in the same transaction, which
// has no caller yet (no mutation exists), so there is nothing to hold it against.

#[tokio::test]
#[ignore]
async fn locked_has_capability_matches_the_unlocked_answer() {
    let pool = migrated_pool().await;
    let locked = capability::has_capability(
        &pool,
        ada(),
        "ORGANIZATION.UPDATE",
        capability::Scope::Organization(product_org()),
        true,
    )
    .await
    .unwrap();
    let unlocked = capability::has_capability(
        &pool,
        ada(),
        "ORGANIZATION.UPDATE",
        capability::Scope::Organization(product_org()),
        false,
    )
    .await
    .unwrap();
    assert_eq!(locked, unlocked);
    assert!(locked);
}

#[tokio::test]
#[ignore]
async fn locked_evaluation_capabilities_matches_the_unlocked_answer() {
    let pool = migrated_pool().await;
    let locked =
        capability::evaluation_capabilities(&pool, ada(), feedback_copilot_project(), true)
            .await
            .unwrap();
    let unlocked =
        capability::evaluation_capabilities(&pool, ada(), feedback_copilot_project(), false)
            .await
            .unwrap();
    assert_eq!(locked, unlocked);
}

#[tokio::test]
#[ignore]
async fn locked_deployment_approval_capabilities_many_matches_the_unlocked_answer() {
    let pool = migrated_pool().await;
    let projects = vec![feedback_copilot_project()];
    let locked = capability::deployment_approval_capabilities_many(&pool, ada(), &projects, true)
        .await
        .unwrap();
    let unlocked =
        capability::deployment_approval_capabilities_many(&pool, ada(), &projects, false)
            .await
            .unwrap();
    assert_eq!(locked, unlocked);
}
