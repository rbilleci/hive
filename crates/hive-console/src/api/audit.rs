//! Audit history: the `AuditEvents` list and the `AuditEvent` detail. Ports `graphql/audit.graphql`.

use crate::graphql::{execute_with_status, schema, TransportFailure};
use cynic::QueryBuilder;

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AuditResourceReference {
    #[cynic(rename = "type")]
    pub kind: Option<String>,
    pub id: Option<cynic::Id>,
}

impl AuditResourceReference {
    /// `TYPE id`, as the table and the drawer print a resource.
    pub fn label(&self) -> String {
        format!(
            "{} {}",
            self.kind.as_deref().unwrap_or("null"),
            self.id.as_ref().map_or("null", cynic::Id::inner)
        )
    }
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AuditEvent")]
pub struct AuditEventFields {
    pub id: cynic::Id,
    pub project_id: Option<cynic::Id>,
    pub actor_id: Option<cynic::Id>,
    pub action: String,
    pub outcome: String,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub changed_fields: Vec<String>,
    pub request_id: Option<cynic::Id>,
    pub correlation_id: Option<cynic::Id>,
    pub graphql_operation: Option<String>,
    pub source_ip: Option<String>,
    pub user_agent: Option<String>,
    pub sensitive_fields_redacted: bool,
    pub occurred_at: String,
    pub resource: Option<AuditResourceReference>,
    pub references: Vec<AuditResourceReference>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct AuditEventEdge {
    pub node: AuditEventFields,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct AuditPageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct AuditEventConnection {
    pub edges: Vec<AuditEventEdge>,
    pub page_info: AuditPageInfo,
}

#[derive(cynic::InputObject, Debug, Clone, Default, PartialEq)]
pub struct AuditEventFilter {
    pub organization_id: Option<cynic::Id>,
    pub project_id: Option<cynic::Id>,
    pub event_id: Option<cynic::Id>,
    pub correlation_id: Option<cynic::Id>,
    pub actor_id: Option<cynic::Id>,
    pub action: Option<String>,
    pub outcome: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<cynic::Id>,
    pub occurred_after: Option<String>,
    pub occurred_before: Option<String>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct AuditEventsVariables {
    pub filter: AuditEventFilter,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AuditEventsVariables")]
pub struct AuditEvents {
    #[arguments(filter: $filter, first: 25, after: $after)]
    pub audit_events: AuditEventConnection,
}

pub async fn request_audit_events(
    filter: AuditEventFilter,
    after: Option<String>,
) -> Result<AuditEventConnection, TransportFailure> {
    Ok(
        execute_with_status(AuditEvents::build(AuditEventsVariables { filter, after }))
            .await?
            .audit_events,
    )
}

#[derive(cynic::QueryVariables, Debug)]
pub struct AuditEventVariables {
    pub filter: AuditEventFilter,
    pub event_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AuditEventVariables")]
pub struct AuditEvent {
    #[arguments(filter: $filter, eventId: $event_id)]
    pub audit_event: Option<AuditEventFields>,
}

pub async fn request_audit_event(
    filter: AuditEventFilter,
    event_id: &str,
) -> Result<Option<AuditEventFields>, TransportFailure> {
    Ok(execute_with_status(AuditEvent::build(AuditEventVariables {
        filter,
        event_id: event_id.into(),
    }))
    .await?
    .audit_event)
}
