//! Ports `PostgresAuditRepository`: queries the immutable `audit_event_projection` view (six
//! `UNION ALL` branches over the per-domain `*_audit_events` tables, defined in
//! `V040__audit_history_projection.sql`) after PostgreSQL scope and capability predicates deny by
//! default. `auditEvents`/`auditEvent` never fan out across the six source tables themselves —
//! the view already materializes that union server-side.

pub mod context;
pub mod cursors;

pub use context::{
    current as current_audit_request_metadata, scope as audit_request_metadata_scope,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hive_application::audit::{
    AuditAction, AuditError, AuditEvent, AuditFilter, AuditPage, AuditRepository,
    AuditResourceReference,
};
use sea_orm::sea_query::ArrayType;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, QueryResult, Statement, Value};
use uuid::Uuid;

use crate::capability::{self, Scope};

const EVENT_CAPABILITIES: &[&str] = &[
    "ORGANIZATION.VIEW",
    "PROJECT.VIEW",
    "AGENT.VIEW",
    "CONFIGURATION.VIEW",
    "DEPLOYMENT.VIEW",
    "EVALUATION_DEFINITION.VIEW",
    "EVALUATION_RUN.VIEW",
];

const SELECT_COLUMNS: &str = "projection_id, source_kind, source_event_id, organization_id, project_id, actor_principal_id, action, \
     resource_type, resource_id, outcome, before_digest, after_digest, safe_changed_fields::text AS safe_changed_fields, \
     request_id, correlation_id, resource_references::text AS resource_references, graphql_operation, occurred_at";

pub struct PgAuditRepository {
    db: DatabaseConnection,
}

impl PgAuditRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

/// Ports the private `Scope` record: the capabilities this principal holds for the event types
/// `audit_event_projection.required_capability` can carry, plus whether `AUDIT_SENSITIVE.VIEW`
/// unlocks `source_ip`/`user_agent`.
struct ScopeGrant {
    capabilities: Vec<String>,
    sensitive: bool,
}

/// Ports the private `scope` helper.
async fn resolve_scope(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    filter: &AuditFilter,
) -> Result<Option<ScopeGrant>, DbErr> {
    let scope = match filter.organization_id {
        Some(organization_id) => Scope::Organization(organization_id),
        None => Scope::Project(filter.scope_id()),
    };
    if !capability::has_capability(db, principal_id, capability::AUDIT_VIEW, scope, false).await? {
        return Ok(None);
    }
    let allowed: Vec<String> = if filter.organization_id.is_some() {
        // AUDIT.VIEW on an organization is the explicit descendant-project audit grant. The
        // normal resource evaluator receives project scope, so it cannot answer this ancestor
        // query per-capability.
        EVENT_CAPABILITIES
            .iter()
            .map(|value| value.to_string())
            .collect()
    } else {
        let mut allowed = Vec::new();
        for candidate in EVENT_CAPABILITIES {
            if capability::has_capability(db, principal_id, candidate, scope, false).await? {
                allowed.push((*candidate).to_string());
            }
        }
        allowed
    };
    if allowed.is_empty() {
        return Ok(None);
    }
    let sensitive = capability::has_capability(
        db,
        principal_id,
        capability::AUDIT_SENSITIVE_VIEW,
        scope,
        false,
    )
    .await?;
    Ok(Some(ScopeGrant {
        capabilities: allowed,
        sensitive,
    }))
}

/// Ports the private `ProjectionId.parse` record method: a query-plan optimization on top of the
/// always-present `projection_id = ?` equality predicate, not a required correctness filter.
fn projection_source(event_id: &str) -> Option<(&'static str, Uuid)> {
    let (prefix, rest) = event_id.split_once(':')?;
    let source_kind = match prefix {
        "agent_draft" => "AGENT_DRAFT",
        "agent_authoring" => "AGENT_AUTHORING",
        "administration" => "ADMINISTRATION",
        "configuration" => "CONFIGURATION",
        "deployment" => "DEPLOYMENT",
        "evaluation" => "EVALUATION",
        _ => return None,
    };
    Uuid::parse_str(rest).ok().map(|id| (source_kind, id))
}

fn string_array(values: &[String]) -> Value {
    Value::Array(
        ArrayType::String,
        Some(Box::new(values.iter().cloned().map(Value::from).collect())),
    )
}

#[allow(clippy::too_many_arguments)]
async fn select_events(
    db: &impl ConnectionTrait,
    filter: &AuditFilter,
    grant: &ScopeGrant,
    cursor: Option<&cursors::DecodedCursor>,
    limit: i64,
) -> Result<Vec<AuditEvent>, DbErr> {
    let (source_kind, source_event_id) = filter
        .event_id
        .as_deref()
        .and_then(projection_source)
        .map(|(kind, id)| (Some(kind), Some(id)))
        .unwrap_or((None, None));
    let query = format!(
        "SELECT {SELECT_COLUMNS}, {sensitive_projection}, \
           (source_ip IS NOT NULL OR user_agent IS NOT NULL) AS sensitive_present \
         FROM audit_event_projection \
         WHERE ($1::uuid IS NULL OR project_id = $1) \
           AND ($2::uuid IS NULL OR organization_id = $2) \
           AND required_capability = ANY($3::text[]) \
           AND ($4::text IS NULL OR projection_id = $4) \
           AND ($5::text IS NULL OR source_kind = $5) \
           AND ($6::uuid IS NULL OR source_event_id = $6) \
           AND ($7::uuid IS NULL OR correlation_id = $7) \
           AND ($8::uuid IS NULL OR actor_principal_id = $8) \
           AND ($9::text IS NULL OR action = $9) \
           AND ($10::text IS NULL OR outcome = $10) \
           AND ($11::text IS NULL OR resource_type = $11) \
           AND ($12::uuid IS NULL OR resource_id = $12) \
           AND ($13::timestamptz IS NULL OR occurred_at >= $13) \
           AND ($14::timestamptz IS NULL OR occurred_at <= $14) \
           AND ($15::timestamptz IS NULL OR (occurred_at, projection_id) < ($15, $16)) \
         ORDER BY occurred_at DESC, projection_id DESC LIMIT $17",
        sensitive_projection = if grant.sensitive {
            "split_part(source_ip, '/', 1) AS source_ip, user_agent"
        } else {
            "NULL::text AS source_ip, NULL::text AS user_agent"
        }
    );
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        &query,
        [
            filter.project_id.into(),
            filter.organization_id.into(),
            string_array(&grant.capabilities),
            filter.event_id.clone().into(),
            source_kind.into(),
            source_event_id.into(),
            filter.correlation_id.into(),
            filter.actor_id.into(),
            filter.action.map(AuditAction::as_str).into(),
            filter.outcome.clone().into(),
            filter.resource_type.clone().into(),
            filter.resource_id.into(),
            filter.occurred_after.into(),
            filter.occurred_before.into(),
            cursor.map(|value| value.occurred_at).into(),
            cursor.map(|value| value.projection_id.clone()).into(),
            limit.into(),
        ],
    );
    let rows = db.query_all_raw(statement).await?;
    rows.iter().map(event_from_row).collect()
}

async fn count_events(
    db: &impl ConnectionTrait,
    filter: &AuditFilter,
    grant: &ScopeGrant,
) -> Result<i32, DbErr> {
    let (source_kind, source_event_id) = filter
        .event_id
        .as_deref()
        .and_then(projection_source)
        .map(|(kind, id)| (Some(kind), Some(id)))
        .unwrap_or((None, None));
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT count(*) AS count FROM audit_event_projection \
         WHERE ($1::uuid IS NULL OR project_id = $1) \
           AND ($2::uuid IS NULL OR organization_id = $2) \
           AND required_capability = ANY($3::text[]) \
           AND ($4::text IS NULL OR projection_id = $4) \
           AND ($5::text IS NULL OR source_kind = $5) \
           AND ($6::uuid IS NULL OR source_event_id = $6) \
           AND ($7::uuid IS NULL OR correlation_id = $7) \
           AND ($8::uuid IS NULL OR actor_principal_id = $8) \
           AND ($9::text IS NULL OR action = $9) \
           AND ($10::text IS NULL OR outcome = $10) \
           AND ($11::text IS NULL OR resource_type = $11) \
           AND ($12::uuid IS NULL OR resource_id = $12) \
           AND ($13::timestamptz IS NULL OR occurred_at >= $13) \
           AND ($14::timestamptz IS NULL OR occurred_at <= $14)",
        [
            filter.project_id.into(),
            filter.organization_id.into(),
            string_array(&grant.capabilities),
            filter.event_id.clone().into(),
            source_kind.into(),
            source_event_id.into(),
            filter.correlation_id.into(),
            filter.actor_id.into(),
            filter.action.map(AuditAction::as_str).into(),
            filter.outcome.clone().into(),
            filter.resource_type.clone().into(),
            filter.resource_id.into(),
            filter.occurred_after.into(),
            filter.occurred_before.into(),
        ],
    );
    let count: i64 = db
        .query_one_raw(statement)
        .await?
        .expect("count(*) always returns exactly one row")
        .try_get_by("count")?;
    Ok(count as i32)
}

fn json_array_of_strings(text: &str) -> Vec<String> {
    let value: serde_json::Value =
        serde_json::from_str(text).expect("safe_changed_fields is always a JSON array");
    value
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Ports the private `references` helper: a malformed element aborts the *entire* parse back to
/// an empty list (matching Java's outer `catch (Exception ignored) { return List.of(); }`, which
/// discards every already-collected reference, not just the offending one), while a `null` type
/// or id on an otherwise well-formed element is skipped individually (Java's `continue`).
fn references_from_json(text: &str) -> Vec<AuditResourceReference> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for entry in entries {
        let kind = entry.get("type").and_then(|value| value.as_str());
        let id = entry.get("id").and_then(|value| value.as_str());
        let (Some(kind), Some(id)) = (kind, id) else {
            continue;
        };
        let Ok(id) = Uuid::parse_str(id) else {
            return Vec::new();
        };
        match AuditResourceReference::new(kind, id) {
            Ok(reference) => result.push(reference),
            Err(_) => return Vec::new(),
        }
    }
    result
}

/// Ports the private `event` row mapper.
fn event_from_row(row: &QueryResult) -> Result<AuditEvent, DbErr> {
    let resource_id: Option<Uuid> = row.try_get_by("resource_id")?;
    let resource = match resource_id {
        Some(id) => Some(
            AuditResourceReference::new(row.try_get_by::<String, _>("resource_type")?, id).expect(
                "audit_event_projection always pairs a supported resource_type with resource_id",
            ),
        ),
        None => None,
    };
    let action = AuditAction::parse(&row.try_get_by::<String, _>("action")?)
        .expect("audit_event_projection.action is always a supported AuditAction");
    let safe_changed_fields: String = row.try_get_by("safe_changed_fields")?;
    let resource_references: String = row.try_get_by("resource_references")?;
    Ok(AuditEvent {
        id: row.try_get_by("projection_id")?,
        source_kind: row.try_get_by("source_kind")?,
        source_event_id: row.try_get_by("source_event_id")?,
        organization_id: row.try_get_by("organization_id")?,
        project_id: row.try_get_by("project_id")?,
        actor_id: row.try_get_by("actor_principal_id")?,
        action,
        resource,
        outcome: row.try_get_by("outcome")?,
        before_digest: row.try_get_by("before_digest")?,
        after_digest: row.try_get_by("after_digest")?,
        changed_fields: json_array_of_strings(&safe_changed_fields),
        references: references_from_json(&resource_references),
        request_id: row.try_get_by("request_id")?,
        correlation_id: row.try_get_by("correlation_id")?,
        graphql_operation: row.try_get_by("graphql_operation")?,
        source_ip: row.try_get_by("source_ip")?,
        user_agent: row.try_get_by("user_agent")?,
        sensitive_fields_redacted: row.try_get_by("sensitive_present")?,
        occurred_at: row.try_get_by::<DateTime<Utc>, _>("occurred_at")?,
    })
}

#[async_trait]
impl AuditRepository for PgAuditRepository {
    async fn find_page(
        &self,
        principal_id: Uuid,
        filter: &AuditFilter,
        first: i32,
        after: Option<&str>,
        include_total_count: bool,
    ) -> Result<AuditPage, AuditError> {
        let grant = resolve_scope(&self.db, principal_id, filter)
            .await
            .map_err(|_| AuditError::Dependency)?
            .ok_or(AuditError::Unavailable)?;
        let cursor = after
            .map(|value| cursors::decode_cursor(value, filter))
            .transpose()?;
        let mut events = select_events(&self.db, filter, &grant, cursor.as_ref(), first as i64 + 1)
            .await
            .map_err(|_| AuditError::Dependency)?;
        let has_next_page = events.len() > first as usize;
        if has_next_page {
            events.truncate(first as usize);
        }
        let end_cursor = events
            .last()
            .map(|event| cursors::encode_cursor(event.occurred_at, &event.id, filter));
        let total_count = if include_total_count {
            Some(
                count_events(&self.db, filter, &grant)
                    .await
                    .map_err(|_| AuditError::Dependency)?,
            )
        } else {
            None
        };
        Ok(AuditPage {
            events,
            has_next_page,
            end_cursor,
            total_count,
        })
    }

    async fn find_event(
        &self,
        principal_id: Uuid,
        filter: &AuditFilter,
        event_id: &str,
    ) -> Result<Option<AuditEvent>, AuditError> {
        let Some(grant) = resolve_scope(&self.db, principal_id, filter)
            .await
            .map_err(|_| AuditError::Dependency)?
        else {
            return Ok(None);
        };
        let by_event = filter.with_event_id(event_id);
        let events = select_events(&self.db, &by_event, &grant, None, 1)
            .await
            .map_err(|_| AuditError::Dependency)?;
        Ok(events.into_iter().next())
    }
}
