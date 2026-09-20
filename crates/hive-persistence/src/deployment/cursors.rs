//! Ports the cursor encode/decode pairs and the `deploymentWhere`/`canonicalFilter`
//! `SqlWhere` builder — all pure, no database access.

use chrono::{DateTime, Utc};
use hive_application::deployment::DeploymentFilter;
use hive_domain::deployment::DeploymentLifecycleStatus;
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

pub struct ListCursor {
    pub requested_at: DateTime<Utc>,
    pub id: Uuid,
}

pub fn encode_list_cursor(filter: &str, requested_at: DateTime<Utc>, id: Uuid) -> String {
    encode(&format!("v1|{filter}|{}|{id}", requested_at.to_rfc3339()))
}

pub fn decode_list_cursor(value: Option<&str>, filter: &str) -> Result<Option<ListCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let decoded = decode(value).ok_or(())?;
    let parts: Vec<&str> = decoded.split('|').collect();
    if parts.len() != 4 || parts[0] != "v1" || parts[1] != filter {
        return Err(());
    }
    let requested_at = DateTime::parse_from_rfc3339(parts[2])
        .map_err(|_| ())?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(parts[3]).map_err(|_| ())?;
    Ok(Some(ListCursor { requested_at, id }))
}

pub struct TimelineCursor {
    pub attempt_number: i64,
    pub sequence: i64,
    pub id: Uuid,
}

pub fn encode_timeline_cursor(attempt_number: i64, sequence: i64, id: Uuid) -> String {
    encode(&format!("v1|{attempt_number}|{sequence}|{id}"))
}

pub fn decode_timeline_cursor(value: Option<&str>) -> Result<Option<TimelineCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let decoded = decode(value).ok_or(())?;
    let parts: Vec<&str> = decoded.split('|').collect();
    if parts.len() != 4 || parts[0] != "v1" {
        return Err(());
    }
    let attempt_number: i64 = parts[1].parse().map_err(|_| ())?;
    let sequence: i64 = parts[2].parse().map_err(|_| ())?;
    let id = Uuid::parse_str(parts[3]).map_err(|_| ())?;
    Ok(Some(TimelineCursor {
        attempt_number,
        sequence,
        id,
    }))
}

pub struct EnvironmentCursor {
    pub stable_definition_id: String,
    pub version: String,
    pub id: Uuid,
}

pub fn encode_environment_cursor(stable_definition_id: &str, version: &str, id: &str) -> String {
    encode(&format!("v1|{stable_definition_id}|{version}|{id}"))
}

pub fn decode_environment_cursor(value: Option<&str>) -> Result<Option<EnvironmentCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let decoded = decode(value).ok_or(())?;
    let parts: Vec<&str> = decoded.split('|').collect();
    if parts.len() != 4 || parts[0] != "v1" {
        return Err(());
    }
    let id = Uuid::parse_str(parts[3]).map_err(|_| ())?;
    Ok(Some(EnvironmentCursor {
        stable_definition_id: parts[1].to_string(),
        version: parts[2].to_string(),
        id,
    }))
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
    let expected_organization = organization.map(|id| id.to_string()).unwrap_or_default();
    let expected_project = project.map(|id| id.to_string()).unwrap_or_default();
    if parts[1] != expected_organization || parts[2] != expected_project {
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

pub struct SqlWhere {
    pub sql: String,
    pub values: Vec<SqlValue>,
}

/// A small closed set of bind-value shapes `deploymentWhere` needs — narrower than a generic
/// dynamic-SQL value type since this is the only caller.
#[derive(Clone)]
pub enum SqlValue {
    Uuid(Uuid),
    Text(String),
    DateTime(DateTime<Utc>),
}

pub fn canonical_filter(filter: &DeploymentFilter) -> String {
    let joined = format!(
        "{}|{}|{}|{}|{}|{}",
        optional_uuid(filter.project_id),
        optional_uuid(filter.agent_id),
        optional_uuid(filter.agent_version_id),
        optional_uuid(filter.environment_definition_version_id),
        filter
            .lifecycle_status
            .map_or("null", DeploymentLifecycleStatus::as_str),
        filter.strategy.as_deref().unwrap_or("null"),
    );
    hive_application::deployment::compiler::digest(&joined)
}

fn optional_uuid(value: Option<Uuid>) -> String {
    value
        .map(|id| id.to_string())
        .unwrap_or_else(|| "null".to_string())
}

/// `visibility_sql`/`visibility_values` are the caller's `ScopedPredicate` (from
/// `capability::deployment_view_predicate`), already `$`-numbered starting at 1; every clause this
/// function adds is renumbered to continue after them.
pub fn deployment_where(
    filter: &DeploymentFilter,
    cursor: &Option<ListCursor>,
    visibility_sql: &str,
    visibility_values: Vec<Uuid>,
) -> SqlWhere {
    let mut clauses = vec!["WHERE project_id = $1".to_string()];
    let mut values =
        vec![SqlValue::Uuid(filter.project_id.expect(
            "the service layer refuses a filter with no projectId",
        ))];
    let mut index = 1usize;

    let renumbered_visibility = renumber(visibility_sql, index);
    clauses.push(format!("AND {renumbered_visibility}"));
    index += visibility_values.len();
    values.extend(visibility_values.into_iter().map(SqlValue::Uuid));

    if let Some(agent_id) = filter.agent_id {
        index += 1;
        clauses.push(format!("AND agent_id = ${index}"));
        values.push(SqlValue::Uuid(agent_id));
    }
    if let Some(agent_version_id) = filter.agent_version_id {
        index += 1;
        clauses.push(format!("AND agent_version_id = ${index}"));
        values.push(SqlValue::Uuid(agent_version_id));
    }
    if let Some(environment_definition_version_id) = filter.environment_definition_version_id {
        index += 1;
        clauses.push(format!("AND environment_definition_version_id = ${index}"));
        values.push(SqlValue::Uuid(environment_definition_version_id));
    }
    if let Some(lifecycle_status) = filter.lifecycle_status {
        index += 1;
        clauses.push(format!("AND lifecycle_status = ${index}"));
        values.push(SqlValue::Text(lifecycle_status.as_str().to_string()));
    }
    if let Some(strategy) = &filter.strategy {
        index += 1;
        clauses.push(format!("AND strategy = ${index}"));
        values.push(SqlValue::Text(strategy.clone()));
    }
    if let Some(cursor) = cursor {
        clauses.push(format!(
            "AND (requested_at, id) < (${}, ${})",
            index + 1,
            index + 2
        ));
        values.push(SqlValue::DateTime(cursor.requested_at));
        values.push(SqlValue::Uuid(cursor.id));
    }
    SqlWhere {
        sql: clauses.join(" "),
        values,
    }
}

/// Shifts every `$n` placeholder in a caller-supplied predicate fragment so it can be concatenated
/// after `start` already-numbered placeholders — replacing each distinct placeholder found, in
/// first-appearance order, with `start+1`, `start+2`, ...
pub(crate) fn renumber(sql: &str, start: usize) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut chars = sql.char_indices().peekable();
    let mut seen: Vec<(String, usize)> = Vec::new();
    while let Some((_, character)) = chars.next() {
        if character == '$' {
            let mut digits = String::new();
            while let Some(&(_, next)) = chars.peek() {
                if next.is_ascii_digit() {
                    digits.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            if digits.is_empty() {
                result.push('$');
                continue;
            }
            let mapped = match seen.iter().find(|(digit, _)| *digit == digits) {
                Some((_, mapped)) => *mapped,
                None => {
                    let mapped = start + seen.len() + 1;
                    seen.push((digits.clone(), mapped));
                    mapped
                }
            };
            result.push('$');
            result.push_str(&mapped.to_string());
        } else {
            result.push(character);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_cursor_round_trips() {
        let requested_at = Utc::now();
        let id = Uuid::new_v4();
        let encoded = encode_list_cursor("filter-key", requested_at, id);
        let decoded = decode_list_cursor(Some(&encoded), "filter-key")
            .unwrap()
            .unwrap();
        assert_eq!(decoded.id, id);
    }

    #[test]
    fn list_cursor_rejects_a_mismatched_filter() {
        let encoded = encode_list_cursor("filter-a", Utc::now(), Uuid::new_v4());
        assert!(decode_list_cursor(Some(&encoded), "filter-b").is_err());
    }

    #[test]
    fn renumber_shifts_repeated_placeholders_consistently() {
        let renumbered = renumber("EXISTS (SELECT 1 WHERE a = $1 OR b = $1 OR c = $2)", 1);
        assert_eq!(
            renumbered,
            "EXISTS (SELECT 1 WHERE a = $2 OR b = $2 OR c = $3)"
        );
    }
}
