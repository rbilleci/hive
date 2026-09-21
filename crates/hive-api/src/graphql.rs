use crate::schema::tenant_hooks::{RequestAuthority, AUTHORITY_UNAVAILABLE};
use crate::schema::{DependencyUnavailable, RequestCorrelationId, RequestPrincipal};
use crate::state::AppState;
use crate::telemetry::Outcome;
use async_graphql::parser::types::DocumentOperations;
use async_graphql::Variables;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use hive_application::audit::AuditRequestMetadata;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::Instant;
use uuid::Uuid;

/// What a generated read answers when the database refused or failed its statement. The
/// database's own message is not sent to the client.
const DEPENDENCY_UNAVAILABLE_MESSAGE: &str = "The service is temporarily unavailable.";

/// A document that fails to parse returns `true` so that execution reports the syntax error. A
/// document that parses but does not define the requested operation name is a transport-level
/// `400`, not a `200` with a GraphQL error, because async-graphql treats an unmatched name as an
/// execution error by default. A single anonymous operation is never a name match: any *named*
/// operation makes async-graphql-parser classify the document as `Multiple`, even with one
/// operation in it.
fn requested_operation_exists(query: &str, requested: &str) -> bool {
    match async_graphql::parser::parse_query(query) {
        Ok(document) => match document.operations {
            DocumentOperations::Single(_) => false,
            DocumentOperations::Multiple(operations) => {
                operations.keys().any(|name| name.as_str() == requested)
            }
        },
        Err(_) => true,
    }
}

/// What the audit record names as the GraphQL operation: the request's supplied name when
/// present (already validated against the document by `requested_operation_exists`), else the
/// sole named operation when the document defines exactly one. An anonymous
/// `DocumentOperations::Single` document, and a `Multiple` document with more than one operation,
/// both resolve to `None`.
fn audited_operation_name(query: &str, requested: Option<&str>) -> Option<String> {
    if let Some(name) = requested {
        return Some(name.to_string());
    }
    match async_graphql::parser::parse_query(query).ok()?.operations {
        DocumentOperations::Single(_) => None,
        DocumentOperations::Multiple(operations) if operations.len() == 1 => {
            operations.keys().next().map(|name| name.to_string())
        }
        DocumentOperations::Multiple(_) => None,
    }
}

/// Whether a resolver error is a failed database crossing. A generated read (or a computed
/// field) that fails in SeaORM carries the `DbErr` as the error's `source`, which async-graphql
/// never serializes; a command marks its own with `DependencyUnavailable`.
fn failed_database_crossing(error: &async_graphql::ServerError) -> bool {
    error.source::<DependencyUnavailable>().is_some()
        || matches!(
            error.source::<sea_orm::DbErr>(),
            Some(
                sea_orm::DbErr::ConnectionAcquire(_)
                    | sea_orm::DbErr::Conn(_)
                    | sea_orm::DbErr::Exec(_)
                    | sea_orm::DbErr::Query(_)
            )
        )
}

/// A `503` when a resolver failed at the database, or when the principal's authority could not be
/// loaded and a generated read was refused for it. The body keeps its GraphQL error shape; only the
/// status gains retryable HTTP semantics. A `DbErr`'s text names tables and columns, so it is
/// replaced.
fn dependency_unavailable(response: &mut async_graphql::Response, authority_loaded: bool) -> bool {
    let mut unavailable = false;
    for error in &mut response.errors {
        if error.source::<sea_orm::DbErr>().is_some() && failed_database_crossing(error) {
            error.message = DEPENDENCY_UNAVAILABLE_MESSAGE.to_string();
        }
        unavailable |= failed_database_crossing(error)
            || (!authority_loaded && error.message == AUTHORITY_UNAVAILABLE);
    }
    unavailable
}

/// A transport-level (pre-execution) error response: `{"errors": [{"message": ...}]}`, deliberately
/// without a `data` key. The shape is built by hand because `async_graphql::Response` always
/// serializes `data` (`null` when absent), and a pre-execution refusal must not carry that key.
fn transport_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"errors": [{"message": message}]}))).into_response()
}

/// JSON-body validation, then session verification (401 before any query parsing), then
/// execution, then the `X-Request-Id` header.
pub async fn graphql(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(query) = body.get("query").and_then(Value::as_str) else {
        return transport_error(StatusCode::BAD_REQUEST, "A GraphQL query is required.");
    };

    let request_id = Uuid::new_v4();
    let started_at = Instant::now();

    let cookie_header = headers.get("cookie").and_then(|value| value.to_str().ok());
    let principal = match state.session_verifier.verify_cookie_header(cookie_header) {
        Ok(principal) => principal,
        Err(_) => {
            return transport_error(StatusCode::UNAUTHORIZED, "Authenticated session required.");
        }
    };

    let operation_name = body
        .get("operationName")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty());
    if let Some(name) = operation_name {
        if !requested_operation_exists(query, name) {
            return transport_error(
                StatusCode::BAD_REQUEST,
                "The GraphQL operationName does not match the document.",
            );
        }
    }

    // A failed load does not fail the request: commands report an unavailable dependency their
    // own way. Generated reads are refused by `TenantHooks::entity_guard` instead.
    let authority = hive_persistence::authority::Authority::load(&state.db, principal)
        .await
        .ok();
    let authority_loaded = authority.is_some();

    let mut request = async_graphql::Request::new(query)
        .data(RequestPrincipal(principal))
        .data(RequestAuthority(authority))
        .data(hive_persistence::audit::SensitiveAuditAccess::default())
        .data(RequestCorrelationId(request_id));
    if let Some(variables) = body.get("variables") {
        request = request.variables(Variables::from_json(variables.clone()));
    }
    if let Some(name) = operation_name {
        request = request.operation_name(name);
    }

    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let source_ip = peer.map(|ConnectInfo(addr)| addr.ip().to_string());
    let audit_metadata = AuditRequestMetadata::new(
        request_id,
        request_id,
        audited_operation_name(query, operation_name).as_deref(),
        source_ip.as_deref(),
        user_agent,
    );

    let mut graphql_response = hive_persistence::audit::audit_request_metadata_scope(
        audit_metadata,
        state.schema.execute(request),
    )
    .await;
    let duration_nanos = started_at.elapsed().as_nanos() as u64;
    let outcome = if graphql_response.is_err() {
        Outcome::Failed
    } else {
        Outcome::Completed
    };
    state.telemetry.record(outcome, duration_nanos);

    let status = if dependency_unavailable(&mut graphql_response, authority_loaded) {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    let mut response = (status, Json(graphql_response)).into_response();
    response.headers_mut().insert(
        "x-request-id",
        request_id
            .to_string()
            .parse()
            .expect("uuid text is a valid header value"),
    );
    response
}
