//! The approval domain: the requirement inbox and single-row read, and `decideDeploymentApproval`
//! with its self-approval refusal, idempotent replay and stale-revision conflict.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test approval -- --ignored

mod common;

use common::*;

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
