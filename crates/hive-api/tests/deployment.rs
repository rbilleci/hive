//! The deployment domain: the preview, deploy and cancel mutations with the project role they
//! require, and the generated deployment reads that follow them.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test deployment -- --ignored

mod common;

use common::*;

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
