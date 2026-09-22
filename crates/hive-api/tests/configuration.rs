//! The configuration domain: reusable resource drafts and their publish lifecycle, MCP servers,
//! and the legacy project tool connection.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test configuration -- --ignored

mod common;

use common::*;

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
