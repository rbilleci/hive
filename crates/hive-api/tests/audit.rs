//! The audit event projection: the raw columns it does not expose, the grant it requires, and the
//! request metadata a real mutation binds into an audit row.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-api --test audit -- --ignored

mod common;

use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{Request, StatusCode};
use common::*;
use std::net::{Ipv4Addr, SocketAddr};
use tower::ServiceExt;

/// The stored `source_ip`, `user_agent` and `required_capability` columns are not part of the
/// generated API: no principal can select, filter or order on them. `sourceIp` and `userAgent`
/// exist only as the computed fields that `AUDIT_SENSITIVE.VIEW` gates.
#[tokio::test]
#[ignore]
async fn audit_events_do_not_expose_the_raw_sensitive_columns() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;
    for query in [
        "query { auditEventProjection(filters: { sourceIp: { eq: \"127.0.0.1\" } }) { nodes { projectionId } } }",
        "query { auditEventProjection(filters: { userAgent: { eq: \"x\" } }) { nodes { projectionId } } }",
        "query { auditEventProjection(filters: { requiredCapability: { eq: \"AGENT.VIEW\" } }) { nodes { projectionId } } }",
        "query { auditEventProjection(orderBy: { sourceIp: ASC }) { nodes { projectionId } } }",
        "query { auditEventProjection(orderBy: { userAgent: ASC }) { nodes { projectionId } } }",
        "query { auditEventProjection { nodes { requiredCapability } } }",
    ] {
        let body = graphql_as(&router, &cookie, query).await;
        assert!(
            body["errors"][0]["message"].is_string(),
            "{query} must be refused: {body}"
        );
        assert!(body.get("data").is_none_or(serde_json::Value::is_null));
    }
}

/// A principal with no audit grant reads no event, whatever it filters by. Bea
/// (00000000-...-0002) holds no role on organization 10000000-...-0001.
#[tokio::test]
#[ignore]
async fn audit_events_are_empty_for_a_principal_without_an_audit_grant() {
    let router = build_test_router().await;
    let cookie = authenticated_cookie_for(&router, "00000000-0000-0000-0000-000000000002").await;

    for filters in [
        "{ organizationId: { eq: \"10000000-0000-0000-0000-000000000001\" }, occurredAt: { gte: \"2020-01-01T00:00:00Z\" } }",
        "{ projectId: { eq: \"50000000-0000-0000-0000-000000000001\" } }",
        "{}",
    ] {
        let body = graphql_as(
            &router,
            &cookie,
            &format!(
                "query {{ auditEventProjection(filters: {filters}, pagination: {{ page: {{ limit: 10, page: 0 }} }}) \
                    {{ nodes {{ projectionId }} paginationInfo {{ total }} }} }}"
            ),
        )
        .await;
        assert!(body.get("errors").is_none(), "{body}");
        assert_eq!(
            body["data"]["auditEventProjection"]["nodes"],
            serde_json::json!([])
        );
        assert_eq!(
            body["data"]["auditEventProjection"]["paginationInfo"]["total"],
            0
        );
    }
}

/// Drives a real mutation through the full `/graphql` handler (not `graphql_as`, so this can set
/// a `User-Agent` header and a loopback `ConnectInfo`) and confirms the resulting audit row binds
/// `request_id`/`correlation_id`/`graphql_operation`/`source_ip`/`user_agent` from the tokio
/// task-local (`hive_persistence::audit::context`) instead of leaving them `NULL`, and that
/// `AUDIT_SENSITIVE.VIEW` gates `sourceIp`/`userAgent` (redacted for Ada's plain
/// `ORGANIZATION_ADMIN` grant, visible once she also holds `PLATFORM_ADMIN`).
#[tokio::test]
#[ignore]
async fn audit_events_bind_request_metadata_and_redact_sensitive_fields_by_capability() {
    use hive_persistence::entity::{
        administration_audit_events, enums, platform_role_assignments, projects,
    };
    use sea_orm::sea_query::Expr;
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

    let db = fixture_database().await;
    let router = build_test_router().await;
    let cookie = authenticated_cookie(&router).await;

    let mut request = Request::post("/graphql")
        .header("content-type", "application/json")
        .header("cookie", &cookie)
        .header("user-agent", "hive-http-integration/1.0")
        .body(Body::from(
            serde_json::json!({
                "query": "mutation TouchProjectGeneralForAudit { updateProjectGeneral(input: { \
                    projectId: \"50000000-0000-0000-0000-000000000001\", expectedRevision: 1, \
                    displayName: \"Customer Feedback Copilot\", description: \"\" }) { problems { code } } }"
            })
            .to_string(),
        ))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 54321))));
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(
        body["data"]["updateProjectGeneral"]["problems"],
        serde_json::json!([])
    );

    let event = administration_audit_events::Entity::find()
        .filter(
            administration_audit_events::Column::ScopeId
                .eq(uuid("50000000-0000-0000-0000-000000000001")),
        )
        .filter(administration_audit_events::Column::Action.eq("PROJECT_GENERAL_UPDATED"))
        .order_by_desc(administration_audit_events::Column::OccurredAt)
        .one(&db)
        .await
        .expect("read the administration audit events")
        .expect("find the newly written administration audit event");
    let event_id = event.id;
    let request_id = event
        .request_id
        .expect("a command's audit row binds its request metadata");

    let event_query = format!(
        "query {{ auditEventProjection(filters: {{ organizationId: {{ eq: \"10000000-0000-0000-0000-000000000001\" }}, \
            sourceKind: {{ eq: \"ADMINISTRATION\" }}, sourceEventId: {{ eq: \"{event_id}\" }}, \
            projectionId: {{ eq: \"administration:{event_id}\" }} }}) \
            {{ nodes {{ projectionId requestId correlationId graphqlOperation sourceIp userAgent sensitiveFieldsRedacted }} }} }}"
    );
    let unprivileged = graphql_as(&router, &cookie, &event_query).await;
    let node = &unprivileged["data"]["auditEventProjection"]["nodes"][0];
    assert_eq!(node["projectionId"], format!("administration:{event_id}"));
    assert_eq!(node["requestId"], request_id.to_string());
    assert_eq!(node["correlationId"], request_id.to_string());
    assert_eq!(node["graphqlOperation"], "TouchProjectGeneralForAudit");
    assert_eq!(node["sourceIp"], serde_json::Value::Null);
    assert_eq!(node["userAgent"], serde_json::Value::Null);
    assert_eq!(node["sensitiveFieldsRedacted"], true);

    // Ada is a platform administrator for the next few statements. The deployment, approval and
    // evaluation round trips assert what she may do on this project without that role and hold
    // this lock while they do, so the grant waits for them and they wait for the revoke.
    let authority_guard = lock_project_agents().await;
    platform_role_assignments::ActiveModel {
        principal_id: Set(uuid("00000000-0000-0000-0000-000000000001")),
        role_code: Set(enums::PlatformRoleCode::PlatformAdmin),
    }
    .insert(&db)
    .await
    .expect("grant platform admin");

    let privileged = graphql_as(&router, &cookie, &event_query).await;
    let node = &privileged["data"]["auditEventProjection"]["nodes"][0];
    assert_eq!(node["sourceIp"], "127.0.0.1");
    assert_eq!(node["userAgent"], "hive-http-integration/1.0");
    assert_eq!(node["sensitiveFieldsRedacted"], false);

    platform_role_assignments::Entity::delete_by_id((
        uuid("00000000-0000-0000-0000-000000000001"),
        enums::PlatformRoleCode::PlatformAdmin,
    ))
    .exec(&db)
    .await
    .expect("revoke platform admin");
    drop(authority_guard);
    administration_audit_events::Entity::delete_by_id(event_id)
        .exec(&db)
        .await
        .expect("clean up the test audit event");
    projects::Entity::update_many()
        .col_expr(projects::Column::Revision, Expr::value(1_i64))
        .filter(projects::Column::Id.eq(uuid("50000000-0000-0000-0000-000000000001")))
        .filter(projects::Column::Revision.eq(2_i64))
        .exec(&db)
        .await
        .expect("restore the project revision");
}
