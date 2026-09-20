//! Ports `AuditGraphql`: M17's read-only `auditEvents`/`auditEvent` transport. Both are root
//! `Query` fields (`AuditGraphql` declares no mutation), matching this port's
//! `RootQuery`/`MergedObject` composition every other domain already uses.

use crate::schema::RequestPrincipal;
use async_graphql::{Context, InputObject, Object, SimpleObject};
use chrono::{DateTime, Utc};
use hive_application::audit::{
    AuditAction as AppAuditAction, AuditError as AppAuditError, AuditEvent as AppAuditEvent,
    AuditFilter as AppAuditFilter, AuditPage as AppAuditPage, AuditQueryService,
    AuditResourceReference as AppAuditResourceReference,
};
use hive_persistence::audit::PgAuditRepository;
use uuid::Uuid;

fn timestamp(value: DateTime<Utc>) -> String {
    hive_domain::java_offset_date_time_string(value)
}

// --- types ---

#[derive(SimpleObject)]
pub struct AuditResourceReference {
    #[graphql(name = "type")]
    pub kind: Option<String>,
    pub id: Option<async_graphql::ID>,
}

impl From<&AppAuditResourceReference> for AuditResourceReference {
    fn from(value: &AppAuditResourceReference) -> Self {
        Self {
            kind: Some(value.kind.clone()),
            id: Some(async_graphql::ID(value.id.to_string())),
        }
    }
}

#[derive(SimpleObject)]
pub struct AuditEvent {
    pub id: async_graphql::ID,
    pub source_kind: String,
    pub source_event_id: Option<async_graphql::ID>,
    pub organization_id: async_graphql::ID,
    pub project_id: Option<async_graphql::ID>,
    pub actor_id: Option<async_graphql::ID>,
    pub action: String,
    pub resource: Option<AuditResourceReference>,
    pub outcome: String,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub changed_fields: Vec<String>,
    pub references: Vec<AuditResourceReference>,
    pub request_id: Option<async_graphql::ID>,
    pub correlation_id: Option<async_graphql::ID>,
    pub graphql_operation: Option<String>,
    pub source_ip: Option<String>,
    pub user_agent: Option<String>,
    pub sensitive_fields_redacted: bool,
    pub occurred_at: String,
}

impl From<&AppAuditEvent> for AuditEvent {
    fn from(value: &AppAuditEvent) -> Self {
        Self {
            id: async_graphql::ID(value.id.clone()),
            source_kind: value.source_kind.clone(),
            source_event_id: value
                .source_event_id
                .map(|id| async_graphql::ID(id.to_string())),
            organization_id: async_graphql::ID(value.organization_id.to_string()),
            project_id: value.project_id.map(|id| async_graphql::ID(id.to_string())),
            actor_id: value.actor_id.map(|id| async_graphql::ID(id.to_string())),
            action: value.action.as_str().to_string(),
            resource: value.resource.as_ref().map(AuditResourceReference::from),
            outcome: value.outcome.clone(),
            before_digest: value.before_digest.clone(),
            after_digest: value.after_digest.clone(),
            changed_fields: value.changed_fields.clone(),
            references: value
                .references
                .iter()
                .map(AuditResourceReference::from)
                .collect(),
            request_id: value.request_id.map(|id| async_graphql::ID(id.to_string())),
            correlation_id: value
                .correlation_id
                .map(|id| async_graphql::ID(id.to_string())),
            graphql_operation: value.graphql_operation.clone(),
            source_ip: value.source_ip.clone(),
            user_agent: value.user_agent.clone(),
            sensitive_fields_redacted: value.sensitive_fields_redacted,
            occurred_at: timestamp(value.occurred_at),
        }
    }
}

#[derive(SimpleObject)]
pub struct AuditEventEdge {
    pub cursor: String,
    pub node: AuditEvent,
}

/// Ports `AuditPageInfo`: unlike the Relay `PageInfo` every other connection type in this port
/// uses, Java declares only `hasNextPage`/`endCursor` here (no backward pagination fields).
#[derive(SimpleObject)]
pub struct AuditPageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

#[derive(SimpleObject)]
pub struct AuditEventConnection {
    pub edges: Vec<AuditEventEdge>,
    pub page_info: AuditPageInfo,
    pub total_count: Option<i32>,
}

/// Builds the connection from a page and the filter that produced it: Java re-derives each
/// edge's own cursor from `(event.occurredAt, event.id)` plus `AuditCursor.binding(filter)`,
/// rather than reusing the page's shared `endCursor` for every row.
fn to_connection(page: AppAuditPage, filter: &AppAuditFilter) -> AuditEventConnection {
    let edges = page
        .events
        .iter()
        .map(|event| AuditEventEdge {
            cursor: hive_persistence::audit::cursors::encode_cursor(
                event.occurred_at,
                &event.id,
                filter,
            ),
            node: AuditEvent::from(event),
        })
        .collect();
    AuditEventConnection {
        edges,
        page_info: AuditPageInfo {
            has_next_page: page.has_next_page,
            end_cursor: page.end_cursor,
        },
        total_count: page.total_count,
    }
}

#[derive(InputObject)]
pub struct AuditEventFilter {
    pub organization_id: Option<async_graphql::ID>,
    pub project_id: Option<async_graphql::ID>,
    pub event_id: Option<async_graphql::ID>,
    pub correlation_id: Option<async_graphql::ID>,
    pub actor_id: Option<async_graphql::ID>,
    pub action: Option<String>,
    pub outcome: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<async_graphql::ID>,
    pub occurred_after: Option<String>,
    pub occurred_before: Option<String>,
}

fn parse_uuid(value: &async_graphql::ID) -> Result<Uuid, ()> {
    Uuid::parse_str(value.as_str()).map_err(|_| ())
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, ()> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| ())
}

/// Ports `AuditGraphql.Resolver.filter`: every failure (a malformed id, an unsupported action
/// name, an unparseable timestamp, or the domain constructor's own scope/regex/range checks)
/// collapses to the caller's single generic "The audit filter is invalid." message, matching
/// Java's broad `catch (RuntimeException exception)`.
fn build_filter(input: &AuditEventFilter) -> Result<AppAuditFilter, ()> {
    let organization_id = input.organization_id.as_ref().map(parse_uuid).transpose()?;
    let project_id = input.project_id.as_ref().map(parse_uuid).transpose()?;
    let correlation_id = input.correlation_id.as_ref().map(parse_uuid).transpose()?;
    let actor_id = input.actor_id.as_ref().map(parse_uuid).transpose()?;
    let resource_id = input.resource_id.as_ref().map(parse_uuid).transpose()?;
    let action = input
        .action
        .as_deref()
        .map(|value| AppAuditAction::parse(value).map_err(|_| ()))
        .transpose()?;
    let occurred_after = input
        .occurred_after
        .as_deref()
        .map(parse_timestamp)
        .transpose()?;
    let occurred_before = input
        .occurred_before
        .as_deref()
        .map(parse_timestamp)
        .transpose()?;
    AppAuditFilter::new(
        organization_id,
        project_id,
        input.event_id.as_ref().map(|value| value.to_string()),
        correlation_id,
        actor_id,
        action,
        input.outcome.clone(),
        input.resource_type.clone(),
        resource_id,
        occurred_after,
        occurred_before,
    )
    .map_err(|_| ())
}

// --- resolvers ---

fn audit_service(ctx: &Context<'_>) -> async_graphql::Result<AuditQueryService<PgAuditRepository>> {
    let repository = PgAuditRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
    Ok(AuditQueryService::new(repository))
}

fn principal(ctx: &Context<'_>) -> async_graphql::Result<Uuid> {
    Ok(ctx.data::<RequestPrincipal>()?.0)
}

fn invalid_filter() -> async_graphql::Error {
    async_graphql::Error::new("The audit filter is invalid.")
}

fn map_error(error: AppAuditError) -> async_graphql::Error {
    async_graphql::Error::new(error.to_string())
}

pub struct AuditQueries;

#[Object]
impl AuditQueries {
    /// Ports `AuditGraphql.Resolver.events`.
    async fn audit_events(
        &self,
        ctx: &Context<'_>,
        filter: AuditEventFilter,
        #[graphql(default = 25)] first: Option<i32>,
        after: Option<String>,
        #[graphql(default_with = "Some(false)")] include_total_count: Option<bool>,
    ) -> async_graphql::Result<AuditEventConnection> {
        let app_filter = build_filter(&filter).map_err(|_| invalid_filter())?;
        let page = audit_service(ctx)?
            .events(
                principal(ctx)?,
                &app_filter,
                first.unwrap_or(25),
                after.as_deref(),
                include_total_count.unwrap_or(false),
            )
            .await
            .map_err(map_error)?;
        Ok(to_connection(page, &app_filter))
    }

    /// Ports `AuditGraphql.Resolver.event`.
    async fn audit_event(
        &self,
        ctx: &Context<'_>,
        filter: AuditEventFilter,
        event_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<AuditEvent>> {
        let app_filter = build_filter(&filter).map_err(|_| invalid_filter())?;
        let event = audit_service(ctx)?
            .event(principal(ctx)?, &app_filter, event_id.as_str())
            .await
            .map_err(map_error)?;
        Ok(event.as_ref().map(AuditEvent::from))
    }
}
