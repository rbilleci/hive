//! The approval surface's cursor encode/decode pairs — pure, no database access. The list,
//! timeline and environment cursors went with the reads they paged, which are generated entity
//! queries with Seaography's own pagination now.

use chrono::{DateTime, Utc};
use uuid::Uuid;

fn encode(value: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.as_bytes())
}

fn decode(value: &str) -> Option<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .ok()?;
    String::from_utf8(bytes).ok()
}

fn optional_uuid(value: Option<Uuid>) -> String {
    value
        .map(|id| id.to_string())
        .unwrap_or_else(|| "null".to_string())
}

pub struct ApprovalCursor {
    pub requested_at: DateTime<Utc>,
    pub id: Uuid,
}

pub fn encode_approval_cursor(
    organization: Option<Uuid>,
    project: Option<Uuid>,
    requested_at: DateTime<Utc>,
    id: Uuid,
) -> String {
    encode(&format!(
        "v1|{}|{}|{}|{id}",
        optional_uuid(organization),
        optional_uuid(project),
        requested_at.to_rfc3339()
    ))
}

pub fn decode_approval_cursor(
    value: Option<&str>,
    organization: Option<Uuid>,
    project: Option<Uuid>,
) -> Result<Option<ApprovalCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let decoded = decode(value).ok_or(())?;
    let parts: Vec<&str> = decoded.split('|').collect();
    if parts.len() != 5 || parts[0] != "v1" {
        return Err(());
    }
    if parts[1] != optional_uuid(organization) || parts[2] != optional_uuid(project) {
        return Err(());
    }
    let requested_at = DateTime::parse_from_rfc3339(parts[3])
        .map_err(|_| ())?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(parts[4]).map_err(|_| ())?;
    Ok(Some(ApprovalCursor { requested_at, id }))
}

pub struct ApprovalDecisionCursor {
    pub requirement_id: Uuid,
    pub decided_at: DateTime<Utc>,
    pub id: Uuid,
}

pub fn encode_approval_decision_cursor(
    requirement_id: Uuid,
    decided_at: DateTime<Utc>,
    id: Uuid,
) -> String {
    encode(&format!(
        "v1|{requirement_id}|{}|{id}",
        decided_at.to_rfc3339()
    ))
}

pub fn decode_approval_decision_cursor(
    value: Option<&str>,
) -> Result<Option<ApprovalDecisionCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let decoded = decode(value).ok_or(())?;
    let parts: Vec<&str> = decoded.split('|').collect();
    if parts.len() != 4 || parts[0] != "v1" {
        return Err(());
    }
    let requirement_id = Uuid::parse_str(parts[1]).map_err(|_| ())?;
    let decided_at = DateTime::parse_from_rfc3339(parts[2])
        .map_err(|_| ())?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(parts[3]).map_err(|_| ())?;
    Ok(Some(ApprovalDecisionCursor {
        requirement_id,
        decided_at,
        id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unscoped inbox passes neither an organization nor a project, so the absent-scope
    /// spelling has to agree between the encoder and the decoder.
    #[test]
    fn approval_cursor_round_trips_for_every_scope_shape() {
        let requested_at = Utc::now();
        let id = Uuid::new_v4();
        let organization = Some(Uuid::new_v4());
        for (organization, project) in [
            (None, None),
            (organization, None),
            (organization, Some(Uuid::new_v4())),
        ] {
            let encoded = encode_approval_cursor(organization, project, requested_at, id);
            let decoded = decode_approval_cursor(Some(&encoded), organization, project)
                .unwrap()
                .unwrap();
            assert_eq!(decoded.id, id);
        }
        let unscoped = encode_approval_cursor(None, None, requested_at, id);
        assert!(decode_approval_cursor(Some(&unscoped), organization, None).is_err());
    }
}
