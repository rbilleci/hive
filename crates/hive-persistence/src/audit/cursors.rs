//! Ports `AuditCursor`. Unlike Java's SHA-256 digest binding, this embeds the filter's scope and
//! narrowing fields directly in the (already base64, already opaque-to-clients) cursor, matching
//! this codebase's established cursor convention (see `hive_persistence::deployment::cursors`'s
//! doc comment) instead of hashing them. The cursor grants no authority by itself — `find_page`
//! re-derives scope and capability from the request's own `filter` argument on every call — so a
//! direct embedded-value comparison is exactly as tamper-evident as a digest comparison.

use base64::Engine;
use chrono::{DateTime, Utc};
use hive_application::audit::{AuditAction, AuditError, AuditFilter};
use uuid::Uuid;

fn encode(value: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.as_bytes())
}

fn decode(value: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .ok()?;
    String::from_utf8(bytes).ok()
}

fn uuid_text(value: Option<Uuid>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn time_text(value: Option<DateTime<Utc>>) -> String {
    value.map(|value| value.to_rfc3339()).unwrap_or_default()
}

/// Ports `AuditCursor.binding(AuditFilter)`.
fn binding(filter: &AuditFilter) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        filter.scope_type(),
        filter.scope_id(),
        filter.event_id.as_deref().unwrap_or(""),
        uuid_text(filter.correlation_id),
        uuid_text(filter.actor_id),
        filter.action.map(AuditAction::as_str).unwrap_or(""),
        filter.outcome.as_deref().unwrap_or(""),
        filter.resource_type.as_deref().unwrap_or(""),
        uuid_text(filter.resource_id),
        time_text(filter.occurred_after),
        time_text(filter.occurred_before),
    )
}

pub struct DecodedCursor {
    pub occurred_at: DateTime<Utc>,
    pub projection_id: String,
}

/// Ports `AuditCursor.encode()`, combined with the constructor call every caller in
/// `PostgresAuditRepository` makes immediately beforehand.
pub fn encode_cursor(
    occurred_at: DateTime<Utc>,
    projection_id: &str,
    filter: &AuditFilter,
) -> String {
    encode(&format!(
        "v1|{}|{}|{}",
        occurred_at.to_rfc3339(),
        projection_id,
        binding(filter)
    ))
}

/// Ports `AuditCursor.decode(String, AuditFilter)`.
pub fn decode_cursor(value: &str, filter: &AuditFilter) -> Result<DecodedCursor, AuditError> {
    let invalid = || AuditError::InvalidRequest("The audit cursor is invalid.".to_string());
    let decoded = decode(value).ok_or_else(invalid)?;
    let parts: Vec<&str> = decoded.splitn(4, '|').collect();
    if parts.len() != 4 || parts[0] != "v1" {
        return Err(invalid());
    }
    let occurred_at = DateTime::parse_from_rfc3339(parts[1])
        .map_err(|_| invalid())?
        .with_timezone(&Utc);
    if parts[3] != binding(filter) {
        return Err(AuditError::InvalidRequest(
            "The audit cursor does not match this filter.".to_string(),
        ));
    }
    Ok(DecodedCursor {
        occurred_at,
        projection_id: parts[2].to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> AuditFilter {
        AuditFilter::new(
            None,
            Some(Uuid::new_v4()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn round_trips_through_encode_and_decode() {
        let filter = filter();
        let occurred_at = Utc::now();
        let encoded = encode_cursor(
            occurred_at,
            "deployment:00000000-0000-0000-0000-000000000001",
            &filter,
        );
        let decoded = decode_cursor(&encoded, &filter).unwrap();
        assert_eq!(
            decoded.projection_id,
            "deployment:00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(
            decoded.occurred_at.timestamp_millis(),
            occurred_at.timestamp_millis()
        );
    }

    #[test]
    fn rejects_a_cursor_minted_against_a_different_filter() {
        let issuing_filter = filter();
        let encoded = encode_cursor(Utc::now(), "deployment:x", &issuing_filter);
        let other_filter = filter();
        assert!(decode_cursor(&encoded, &other_filter).is_err());
    }

    #[test]
    fn rejects_garbage_input() {
        assert!(decode_cursor("not-base64!!", &filter()).is_err());
    }
}
