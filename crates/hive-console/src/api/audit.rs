//! Audit history: the `AuditEvents` list and the `AuditEvent` detail, both the generated
//! `auditEventProjection` read. The server decides which events a principal reads and withholds
//! `sourceIp` / `userAgent` without `AUDIT_SENSITIVE.VIEW`.
//!
//! Every request names its scope (`organizationId` or `projectId`), which is what the audit
//! tables are indexed by. The list also reads the scope's `capabilities`: a principal without
//! `AUDIT.VIEW` reads no event, and the page says "unavailable" instead of "no events".

use crate::api::generated::{
    OrderByEnum, OrganizationsFilterInput, PageInput, PaginationInput, ProjectsFilterInput,
    StringFilterInput, TextFilterInput,
};
use crate::api::page::{Page, PaginationInfo};
use crate::graphql::{execute_with_status, schema, GeneratedJson, TransportFailure};
use cynic::QueryBuilder;

pub const PAGE_SIZE: i32 = 25;
const AUDIT_VIEW: &str = "AUDIT.VIEW";

/// What the page filters by. Exactly one of `organization_id` and `project_id` is set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditEventFilter {
    pub organization_id: Option<String>,
    pub project_id: Option<String>,
    pub event_id: Option<String>,
    pub correlation_id: Option<String>,
    pub actor_id: Option<String>,
    pub action: Option<String>,
    pub outcome: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub occurred_after: Option<String>,
    pub occurred_before: Option<String>,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct AuditEventProjectionFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<StringFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub actor_principal_id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub action: Option<StringFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<StringFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<StringFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub projection_id: Option<StringFilterInput>,
}

/// The source table and row an event id (`agent_authoring:<uuid>`) names. The id itself is
/// computed by the view, so only these two are indexed.
fn projection_source(event_id: &str) -> Option<(String, &str)> {
    let (prefix, source_event_id) = event_id.split_once(':')?;
    let known = [
        "agent_draft",
        "agent_authoring",
        "administration",
        "configuration",
        "deployment",
        "evaluation",
    ];
    (known.contains(&prefix) && crate::api::generated::is_uuid(source_event_id))
        .then(|| (prefix.to_uppercase(), source_event_id))
}

impl From<&AuditEventFilter> for AuditEventProjectionFilterInput {
    fn from(filter: &AuditEventFilter) -> Self {
        let text = |value: &Option<String>| value.as_deref().map(TextFilterInput::eq);
        let string = |value: &Option<String>| value.as_deref().map(StringFilterInput::eq);
        let source = filter.event_id.as_deref().and_then(projection_source);
        Self {
            source_kind: source
                .as_ref()
                .map(|(source_kind, _)| StringFilterInput::eq(source_kind)),
            source_event_id: source
                .as_ref()
                .map(|(_, source_event_id)| TextFilterInput::eq(source_event_id)),
            organization_id: text(&filter.organization_id),
            project_id: text(&filter.project_id),
            actor_principal_id: text(&filter.actor_id),
            action: string(&filter.action),
            resource_type: string(&filter.resource_type),
            resource_id: text(&filter.resource_id),
            outcome: string(&filter.outcome),
            correlation_id: text(&filter.correlation_id),
            occurred_at: (filter.occurred_after.is_some() || filter.occurred_before.is_some())
                .then(|| TextFilterInput {
                    gte: filter.occurred_after.clone(),
                    lte: filter.occurred_before.clone(),
                    ..Default::default()
                }),
            projection_id: string(&filter.event_id),
        }
    }
}

/// Newest first; the key is the tie-break, so events of one instant keep one order across pages.
#[derive(cynic::InputObject, Debug, Clone)]
pub struct AuditEventProjectionOrderInput {
    pub occurred_at: OrderByEnum,
    pub projection_id: OrderByEnum,
}

fn newest_first() -> AuditEventProjectionOrderInput {
    AuditEventProjectionOrderInput {
        occurred_at: OrderByEnum::Desc,
        projection_id: OrderByEnum::Desc,
    }
}

fn page(page: i32, limit: i32) -> PaginationInput {
    PaginationInput::Page(PageInput { limit, page })
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "AuditEventProjection")]
pub struct AuditEventRow {
    pub projection_id: String,
    pub project_id: Option<String>,
    pub actor_principal_id: Option<String>,
    pub action: String,
    pub outcome: String,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub safe_changed_fields: GeneratedJson,
    pub resource_references: GeneratedJson,
    pub request_id: Option<String>,
    pub correlation_id: Option<String>,
    pub graphql_operation: Option<String>,
    pub source_ip: Option<String>,
    pub user_agent: Option<String>,
    pub sensitive_fields_redacted: bool,
    pub occurred_at: String,
}

/// `TYPE id`, as the table and the drawer print a resource.
fn label(kind: &str, id: &str) -> String {
    format!("{kind} {id}")
}

/// One audit event as the page shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEventFields {
    pub id: String,
    pub project_id: Option<String>,
    pub actor_id: Option<String>,
    pub action: String,
    pub outcome: String,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub changed_fields: Vec<String>,
    pub request_id: Option<String>,
    pub correlation_id: Option<String>,
    pub graphql_operation: Option<String>,
    pub source_ip: Option<String>,
    pub user_agent: Option<String>,
    pub sensitive_fields_redacted: bool,
    pub occurred_at: String,
    /// The `TYPE id` label of the resource the event is about.
    pub resource: Option<String>,
    /// The `TYPE id` labels of the related resources and evidence.
    pub references: Vec<String>,
}

impl From<AuditEventRow> for AuditEventFields {
    fn from(row: AuditEventRow) -> Self {
        let changed_fields = row
            .safe_changed_fields
            .0
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|field| field.as_str().map(str::to_string))
            .collect();
        let references = row
            .resource_references
            .0
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|reference| {
                Some(label(
                    reference.get("type")?.as_str()?,
                    reference.get("id")?.as_str()?,
                ))
            })
            .collect();
        Self {
            resource: row
                .resource_id
                .as_deref()
                .map(|id| label(row.resource_type.as_deref().unwrap_or("null"), id)),
            id: row.projection_id,
            project_id: row.project_id,
            actor_id: row.actor_principal_id,
            action: row.action,
            outcome: row.outcome,
            before_digest: row.before_digest,
            after_digest: row.after_digest,
            changed_fields,
            request_id: row.request_id,
            correlation_id: row.correlation_id,
            graphql_operation: row.graphql_operation,
            source_ip: row.source_ip,
            user_agent: row.user_agent,
            sensitive_fields_redacted: row.sensitive_fields_redacted,
            occurred_at: row.occurred_at,
            references,
        }
    }
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "AuditEventProjectionConnection")]
pub struct AuditEventPageConnection {
    pub nodes: Vec<AuditEventRow>,
    pub pagination_info: Option<PaginationInfo>,
}

/// One page of events, newest first. `None` when the scope is not visible or the principal holds
/// no `AUDIT.VIEW` at it.
fn audit_events_page(
    capabilities: Option<Vec<String>>,
    connection: AuditEventPageConnection,
) -> Option<Page<AuditEventFields>> {
    capabilities?
        .iter()
        .any(|capability| capability == AUDIT_VIEW)
        .then(|| {
            Page::new(connection.nodes, connection.pagination_info).map(AuditEventFields::from)
        })
}

// The list has one operation name, `AuditEvents`, and one document per scope kind: the scope's
// `capabilities` are read from `organizations` or from `projects`.
mod organization_scope {
    use super::*;

    #[derive(cynic::QueryVariables, Debug)]
    pub struct AuditEventsVariables {
        pub scope: OrganizationsFilterInput,
        pub filters: AuditEventProjectionFilterInput,
        pub order_by: AuditEventProjectionOrderInput,
        pub pagination: PaginationInput,
    }

    #[derive(cynic::QueryFragment, Debug)]
    #[cynic(graphql_type = "Organizations")]
    pub struct Scope {
        pub capabilities: Vec<String>,
    }

    #[derive(cynic::QueryFragment, Debug)]
    #[cynic(graphql_type = "OrganizationsConnection")]
    pub struct ScopeConnection {
        pub nodes: Vec<Scope>,
    }

    #[derive(cynic::QueryFragment, Debug)]
    #[cynic(graphql_type = "Query", variables = "AuditEventsVariables")]
    pub struct AuditEvents {
        #[arguments(filters: $scope)]
        pub organizations: ScopeConnection,
        #[arguments(filters: $filters, orderBy: $order_by, pagination: $pagination)]
        pub audit_event_projection: AuditEventPageConnection,
    }
}

mod project_scope {
    use super::*;

    #[derive(cynic::QueryVariables, Debug)]
    pub struct AuditEventsVariables {
        pub scope: ProjectsFilterInput,
        pub filters: AuditEventProjectionFilterInput,
        pub order_by: AuditEventProjectionOrderInput,
        pub pagination: PaginationInput,
    }

    #[derive(cynic::QueryFragment, Debug)]
    #[cynic(graphql_type = "Projects")]
    pub struct Scope {
        pub capabilities: Vec<String>,
    }

    #[derive(cynic::QueryFragment, Debug)]
    #[cynic(graphql_type = "ProjectsConnection")]
    pub struct ScopeConnection {
        pub nodes: Vec<Scope>,
    }

    #[derive(cynic::QueryFragment, Debug)]
    #[cynic(graphql_type = "Query", variables = "AuditEventsVariables")]
    pub struct AuditEvents {
        #[arguments(filters: $scope)]
        pub projects: ScopeConnection,
        #[arguments(filters: $filters, orderBy: $order_by, pagination: $pagination)]
        pub audit_event_projection: AuditEventPageConnection,
    }
}

/// One page of the scope's events; `None` when audit history is unavailable to the principal.
pub async fn request_audit_events(
    filter: &AuditEventFilter,
    page_number: i32,
) -> Result<Option<Page<AuditEventFields>>, TransportFailure> {
    let (filters, order_by, pagination) = (
        AuditEventProjectionFilterInput::from(filter),
        newest_first(),
        page(page_number, PAGE_SIZE),
    );
    if let Some(organization_id) = &filter.organization_id {
        let data = execute_with_status(organization_scope::AuditEvents::build(
            organization_scope::AuditEventsVariables {
                scope: OrganizationsFilterInput {
                    id: Some(TextFilterInput::eq(organization_id)),
                    ..Default::default()
                },
                filters,
                order_by,
                pagination,
            },
        ))
        .await?;
        let scope = data.organizations.nodes.into_iter().next();
        return Ok(audit_events_page(
            scope.map(|scope| scope.capabilities),
            data.audit_event_projection,
        ));
    }
    let data = execute_with_status(project_scope::AuditEvents::build(
        project_scope::AuditEventsVariables {
            scope: ProjectsFilterInput {
                id: filter.project_id.as_deref().map(TextFilterInput::eq),
                ..Default::default()
            },
            filters,
            order_by,
            pagination,
        },
    ))
    .await?;
    let scope = data.projects.nodes.into_iter().next();
    Ok(audit_events_page(
        scope.map(|scope| scope.capabilities),
        data.audit_event_projection,
    ))
}

#[derive(cynic::QueryVariables, Debug)]
pub struct AuditEventVariables {
    pub filters: AuditEventProjectionFilterInput,
    pub order_by: AuditEventProjectionOrderInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "AuditEventProjectionConnection")]
pub struct AuditEventDetailConnection {
    pub nodes: Vec<AuditEventRow>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AuditEventVariables")]
pub struct AuditEvent {
    #[arguments(filters: $filters, orderBy: $order_by, pagination: $pagination)]
    pub audit_event_projection: AuditEventDetailConnection,
}

/// The event `event_id` names within the filter's scope, if the principal may read it.
pub async fn request_audit_event(
    filter: &AuditEventFilter,
    event_id: &str,
) -> Result<Option<AuditEventFields>, TransportFailure> {
    let scoped = AuditEventFilter {
        organization_id: filter.organization_id.clone(),
        project_id: filter.project_id.clone(),
        event_id: Some(event_id.to_string()),
        ..Default::default()
    };
    Ok(execute_with_status(AuditEvent::build(AuditEventVariables {
        filters: AuditEventProjectionFilterInput::from(&scoped),
        order_by: newest_first(),
        pagination: page(0, 1),
    }))
    .await?
    .audit_event_projection
    .nodes
    .into_iter()
    .next()
    .map(Into::into))
}
