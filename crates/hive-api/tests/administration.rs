//! The administration domain: the generated scope reads with their membership, budget and
//! approval-policy relations, and the mutations that create a project, add a membership, set a
//! budget and change an approval policy.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test administration -- --ignored

mod common;

use common::*;

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
