//! Ports `dev.hive.application.audit.AuditModels`/`AuditRequestMetadata`.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{AuditAction, AuditResourceReference};

#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub id: String,
    pub source_kind: String,
    pub source_event_id: Option<Uuid>,
    pub organization_id: Uuid,
    pub project_id: Option<Uuid>,
    pub actor_id: Option<Uuid>,
    pub action: AuditAction,
    pub resource: Option<AuditResourceReference>,
    pub outcome: String,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub changed_fields: Vec<String>,
    pub references: Vec<AuditResourceReference>,
    pub request_id: Option<Uuid>,
    pub correlation_id: Option<Uuid>,
    pub graphql_operation: Option<String>,
    /// Redacted (replaced with `NULL` by the SQL projection) whenever the reading principal
    /// lacks `AUDIT_SENSITIVE.VIEW`, independent of `sensitive_fields_redacted`.
    pub source_ip: Option<String>,
    pub user_agent: Option<String>,
    /// True whenever the underlying row has sensitive data, whether or not this reader was
    /// authorized to see it — "there was something here that got hidden from you," not "this
    /// event carries personal data." Ports `(source_ip IS NOT NULL OR user_agent IS NOT NULL)`.
    pub sensitive_fields_redacted: bool,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AuditPage {
    pub events: Vec<AuditEvent>,
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
    pub total_count: Option<i32>,
}

/// Ports `AuditRequestMetadata`: server-observed request facts that travel from the `/graphql`
/// handler into the transaction that creates immutable audit facts via a tokio task-local
/// (`hive_persistence::audit::context`), replacing Java's `PostgresAuditRequestContext`
/// thread-local.
#[derive(Debug, Clone)]
pub struct AuditRequestMetadata {
    pub request_id: Uuid,
    pub correlation_id: Uuid,
    pub graphql_operation: Option<String>,
    pub source_ip: Option<String>,
    pub user_agent: Option<String>,
}

impl AuditRequestMetadata {
    pub fn new(
        request_id: Uuid,
        correlation_id: Uuid,
        graphql_operation: Option<&str>,
        source_ip: Option<&str>,
        user_agent: Option<&str>,
    ) -> Self {
        Self {
            request_id,
            correlation_id,
            graphql_operation: bounded(graphql_operation, 160),
            source_ip: bounded(source_ip, 64),
            user_agent: bounded(user_agent, 512),
        }
    }
}

fn bounded(value: Option<&str>, limit: usize) -> Option<String> {
    let normalized: String = value?
        .chars()
        .map(|value| if value.is_control() { ' ' } else { value })
        .collect();
    let normalized = normalized.trim();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.chars().take(limit).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_blanks_control_characters_and_trims() {
        let metadata = AuditRequestMetadata::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Some("  mutation\tCreate  "),
            None,
            None,
        );
        assert_eq!(
            metadata.graphql_operation.as_deref(),
            Some("mutation Create")
        );
    }

    #[test]
    fn bounded_collapses_an_empty_result_to_none() {
        let metadata =
            AuditRequestMetadata::new(Uuid::new_v4(), Uuid::new_v4(), Some("   "), None, None);
        assert_eq!(metadata.graphql_operation, None);
    }

    #[test]
    fn bounded_truncates_to_the_configured_limit() {
        let metadata = AuditRequestMetadata::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            None,
            None,
            Some(&"x".repeat(600)),
        );
        assert_eq!(metadata.user_agent.unwrap().len(), 512);
    }
}
