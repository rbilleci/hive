//! Ports `schema/audit.rs` (`AuditGraphql`): read-only, two `Query` root fields, no mutation.
//!
//! `AuditResourceReference` cannot go through `#[derive(CustomOutputType)]`: one of its GraphQL
//! fields is named `type`, a Rust keyword. The derive macro names every field with
//! `stringify!(#field_ident)` (`custom_output_type.rs`), and `stringify!(r#type)` produces the
//! *literal string* `"r#type"`, not `"type"` — confirmed directly (`rustc` a one-line program
//! printing `stringify!(r#type)`), not assumed. Hand-built instead, replicating exactly what the
//! derive macro itself generates for a plain struct (`custom_output_type.rs`'s
//! `derive_custom_output_type_struct`): one `Field` per property, each resolving
//! `<FieldType>::gql_field_value(parent.field.clone(), ctx)`.
//!
//! `auditEvents` carries two defaults (`first: Int = 25`, `includeTotalCount: Boolean = false`,
//! `GSR-DEFAULTS`) so it is hand-built like `accessibleOrganizations`; `auditEvent` has none, so it
//! goes through `#[CustomFields]` like `organization(id)`.
//!
//! `changedFields` is `scalars::StringList`, not a bare `Vec<String>`: Seaography's SeaORM-column
//! blanket also covers `Vec<String>` itself (a valid Postgres array column type), which wins field
//! resolution over `CustomOutputType`'s own `Vec<T>` blanket and panics at schema-build time with
//! `unreachable!("Vec<T> is not handled")` — see `StringList`'s own doc comment for the full trail.

use crate::schema::scalars;
use crate::schema::scalars::{Id, StringList};
use crate::schema::RequestPrincipal;
use async_graphql::dynamic::{Field, FieldFuture, FieldValue, InputValue, Object, TypeRef};
use chrono::{DateTime, Utc};
use hive_application::audit::{
    AuditAction as AppAuditAction, AuditError as AppAuditError, AuditEvent as AppAuditEvent,
    AuditFilter as AppAuditFilter, AuditPage as AppAuditPage, AuditQueryService,
    AuditResourceReference as AppAuditResourceReference,
};
use hive_persistence::audit::PgAuditRepository;
use seaography::{
    BuilderContext, CustomFields, CustomInputType, CustomOutputObject, CustomOutputType,
};
use uuid::Uuid;

fn timestamp(value: DateTime<Utc>) -> String {
    hive_domain::java_offset_date_time_string(value)
}

#[allow(non_snake_case)]
mod wire {
    use super::*;

    #[derive(Clone)]
    pub struct AuditResourceReference {
        pub kind: Option<String>,
        pub id: Option<Id>,
    }

    impl CustomOutputType for AuditResourceReference {
        fn gql_output_type_ref(_ctx: &'static BuilderContext) -> TypeRef {
            TypeRef::named_nn("AuditResourceReference")
        }

        fn gql_field_value(self, _ctx: &'static BuilderContext) -> Option<FieldValue<'static>> {
            Some(FieldValue::owned_any(self))
        }
    }

    impl CustomOutputObject for AuditResourceReference {
        fn basic_object(context: &'static BuilderContext) -> Object {
            // `String` implements Seaography's `GqlScalarValueType` (its SeaORM-column-oriented
            // scalar system), not `CustomOutputType` directly; `#[derive(CustomOutputType)]`'s own
            // generated code imports this trait (plus two others this file doesn't need) for
            // exactly this reason (`custom_output_type.rs`'s `output_type_imports`).
            use seaography::GqlScalarValueType;
            Object::new("AuditResourceReference")
                .field(Field::new(
                    "type",
                    Option::<String>::gql_output_type_ref(context),
                    move |ctx| {
                        FieldFuture::new(async move {
                            let obj = ctx
                                .parent_value
                                .try_downcast_ref::<AuditResourceReference>()?;
                            Ok(Option::<String>::gql_field_value(obj.kind.clone(), context))
                        })
                    },
                ))
                .field(Field::new(
                    "id",
                    Option::<Id>::gql_output_type_ref(context),
                    move |ctx| {
                        FieldFuture::new(async move {
                            let obj = ctx
                                .parent_value
                                .try_downcast_ref::<AuditResourceReference>()?;
                            Ok(Option::<Id>::gql_field_value(obj.id.clone(), context))
                        })
                    },
                ))
        }
    }

    impl From<&AppAuditResourceReference> for AuditResourceReference {
        fn from(value: &AppAuditResourceReference) -> Self {
            Self {
                kind: Some(value.kind.clone()),
                id: Some(value.id.to_string().into()),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AuditEvent {
        pub id: Id,
        pub sourceKind: String,
        pub sourceEventId: Option<Id>,
        pub organizationId: Id,
        pub projectId: Option<Id>,
        pub actorId: Option<Id>,
        pub action: String,
        pub resource: Option<AuditResourceReference>,
        pub outcome: String,
        pub beforeDigest: Option<String>,
        pub afterDigest: Option<String>,
        pub changedFields: StringList,
        pub references: Vec<AuditResourceReference>,
        pub requestId: Option<Id>,
        pub correlationId: Option<Id>,
        pub graphqlOperation: Option<String>,
        pub sourceIp: Option<String>,
        pub userAgent: Option<String>,
        pub sensitiveFieldsRedacted: bool,
        pub occurredAt: String,
    }

    impl From<&AppAuditEvent> for AuditEvent {
        fn from(value: &AppAuditEvent) -> Self {
            Self {
                id: value.id.clone().into(),
                sourceKind: value.source_kind.clone(),
                sourceEventId: value.source_event_id.map(|id| id.to_string().into()),
                organizationId: value.organization_id.to_string().into(),
                projectId: value.project_id.map(|id| id.to_string().into()),
                actorId: value.actor_id.map(|id| id.to_string().into()),
                action: value.action.as_str().to_string(),
                resource: value.resource.as_ref().map(AuditResourceReference::from),
                outcome: value.outcome.clone(),
                beforeDigest: value.before_digest.clone(),
                afterDigest: value.after_digest.clone(),
                changedFields: value.changed_fields.clone().into(),
                references: value
                    .references
                    .iter()
                    .map(AuditResourceReference::from)
                    .collect(),
                requestId: value.request_id.map(|id| id.to_string().into()),
                correlationId: value.correlation_id.map(|id| id.to_string().into()),
                graphqlOperation: value.graphql_operation.clone(),
                sourceIp: value.source_ip.clone(),
                userAgent: value.user_agent.clone(),
                sensitiveFieldsRedacted: value.sensitive_fields_redacted,
                occurredAt: timestamp(value.occurred_at),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AuditEventEdge {
        pub cursor: String,
        pub node: AuditEvent,
    }

    // Ports `AuditPageInfo`: unlike `seaography::PageInfo` (Relay-style, 4 fields), Java declares
    // only `hasNextPage`/`endCursor` here — no backward pagination fields.
    #[derive(CustomOutputType, Clone)]
    pub struct AuditPageInfo {
        pub hasNextPage: bool,
        pub endCursor: Option<String>,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AuditEventConnection {
        pub edges: Vec<AuditEventEdge>,
        pub pageInfo: AuditPageInfo,
        pub totalCount: Option<i32>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "AuditEventFilter")]
    pub struct AuditEventFilter {
        pub organizationId: Option<Id>,
        pub projectId: Option<Id>,
        pub eventId: Option<Id>,
        pub correlationId: Option<Id>,
        pub actorId: Option<Id>,
        pub action: Option<String>,
        pub outcome: Option<String>,
        pub resourceType: Option<String>,
        pub resourceId: Option<Id>,
        pub occurredAfter: Option<String>,
        pub occurredBefore: Option<String>,
    }

    pub struct AuditQueries;

    #[CustomFields]
    impl AuditQueries {
        // Ports `AuditGraphql.Resolver.event`.
        async fn auditEvent(
            ctx: &async_graphql::Context<'_>,
            filter: AuditEventFilter,
            eventId: Id,
        ) -> async_graphql::Result<Option<AuditEvent>> {
            let app_filter = build_filter(&filter).map_err(|_| invalid_filter())?;
            let event = audit_service(ctx)?
                .event(principal(ctx)?, &app_filter, &eventId.0)
                .await
                .map_err(map_error)?;
            Ok(event.as_ref().map(AuditEvent::from))
        }
    }
}

pub use wire::{
    AuditEvent, AuditEventConnection, AuditEventEdge, AuditEventFilter, AuditPageInfo,
    AuditQueries, AuditResourceReference,
};

fn context() -> &'static BuilderContext {
    crate::schema::context()
}

fn parse_uuid(value: &Id) -> Result<Uuid, ()> {
    Uuid::parse_str(&value.0).map_err(|_| ())
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
    let organization_id = input.organizationId.as_ref().map(parse_uuid).transpose()?;
    let project_id = input.projectId.as_ref().map(parse_uuid).transpose()?;
    let correlation_id = input.correlationId.as_ref().map(parse_uuid).transpose()?;
    let actor_id = input.actorId.as_ref().map(parse_uuid).transpose()?;
    let resource_id = input.resourceId.as_ref().map(parse_uuid).transpose()?;
    let action = input
        .action
        .as_deref()
        .map(|value| AppAuditAction::parse(value).map_err(|_| ()))
        .transpose()?;
    let occurred_after = input
        .occurredAfter
        .as_deref()
        .map(parse_timestamp)
        .transpose()?;
    let occurred_before = input
        .occurredBefore
        .as_deref()
        .map(parse_timestamp)
        .transpose()?;
    AppAuditFilter::new(
        organization_id,
        project_id,
        input.eventId.as_ref().map(|value| value.0.clone()),
        correlation_id,
        actor_id,
        action,
        input.outcome.clone(),
        input.resourceType.clone(),
        resource_id,
        occurred_after,
        occurred_before,
    )
    .map_err(|_| ())
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
        pageInfo: AuditPageInfo {
            hasNextPage: page.has_next_page,
            endCursor: page.end_cursor,
        },
        totalCount: page.total_count,
    }
}

fn audit_service(
    ctx: &async_graphql::Context<'_>,
) -> async_graphql::Result<AuditQueryService<PgAuditRepository>> {
    let repository = PgAuditRepository::new(ctx.data::<sea_orm::DatabaseConnection>()?.clone());
    Ok(AuditQueryService::new(repository))
}

fn principal(ctx: &async_graphql::Context<'_>) -> async_graphql::Result<Uuid> {
    Ok(ctx.data::<RequestPrincipal>()?.0)
}

fn invalid_filter() -> async_graphql::Error {
    async_graphql::Error::new("The audit filter is invalid.")
}

fn map_error(error: AppAuditError) -> async_graphql::Error {
    async_graphql::Error::new(error.to_string())
}

/// `auditEvents(after, filter: AuditEventFilter!, first: Int = 25, includeTotalCount: Boolean =
/// false)` (`GSR-DEFAULTS`). Ports `AuditGraphql.Resolver.events`.
fn audit_events_field() -> Field {
    Field::new(
        "auditEvents",
        TypeRef::named_nn("AuditEventConnection"),
        |ctx| {
            FieldFuture::new(async move {
                let filter_value = ctx.args.try_get("filter")?;
                let filter = AuditEventFilter::parse_value(context(), Some(filter_value))?;
                let app_filter = build_filter(&filter).map_err(|_| invalid_filter())?;
                // The `first` argument's own TypeRef is `Int` (32-bit); the engine's argument
                // coercion already guarantees the value fits before the resolver runs.
                let first = ctx.args.try_get("first")?.i64()? as i32;
                let after = scalars::optional_string(ctx.args.get("after"))?;
                let include_total_count = ctx.args.try_get("includeTotalCount")?.boolean()?;

                let page = audit_service(ctx.ctx)?
                    .events(
                        principal(ctx.ctx)?,
                        &app_filter,
                        first,
                        after.as_deref(),
                        include_total_count,
                    )
                    .await
                    .map_err(map_error)?;
                let connection = to_connection(page, &app_filter);
                Ok(connection.gql_field_value(context()))
            })
        },
    )
    .argument(InputValue::new("after", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new(
        "filter",
        TypeRef::named_nn("AuditEventFilter"),
    ))
    .argument(InputValue::new("first", TypeRef::named(TypeRef::INT)).default_value(25i32))
    .argument(
        InputValue::new("includeTotalCount", TypeRef::named(TypeRef::BOOLEAN)).default_value(false),
    )
}

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_query::<AuditQueries>();
    builder.register_custom_output::<AuditResourceReference>();
    builder.register_custom_output::<AuditEvent>();
    builder.register_custom_output::<AuditEventEdge>();
    builder.register_custom_output::<AuditPageInfo>();
    builder.register_custom_output::<AuditEventConnection>();
    builder.register_custom_input::<AuditEventFilter>();
    builder.queries.push(audit_events_field());
}
