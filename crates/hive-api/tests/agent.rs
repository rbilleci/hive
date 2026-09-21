//! The agent-authoring domain: reading an agent's draft document, and the create, update,
//! validate and publish mutations over it.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test agent -- --ignored

mod common;

use common::*;

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
