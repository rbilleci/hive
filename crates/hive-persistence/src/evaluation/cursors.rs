//! Ports `PageCursor`/`pageCursor`/`decodePageCursor`: the cursor encode/decode
//! pairs for every evaluation keyset query. The wire form is Java's own: a
//! base64url JSON record `{version, kind, scope, filter, keys}`. JSON framing
//! matters because a scope (`project|version`), a filter, and a display name may
//! contain any delimiter a joined string could use, and `kind` matters because
//! two lists over one scope (a run's metrics and its artifacts) would otherwise
//! accept each other's cursors.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct PageCursor {
    version: u32,
    kind: String,
    scope: String,
    filter: String,
    keys: Vec<String>,
}

fn encode(kind: &str, scope: &str, filter: &str, keys: &[&str]) -> String {
    use base64::Engine;
    let cursor = PageCursor {
        version: 1,
        kind: kind.to_string(),
        scope: scope.to_string(),
        filter: filter.to_string(),
        keys: keys.iter().map(|key| key.to_string()).collect(),
    };
    let json = serde_json::to_vec(&cursor).expect("a page cursor always serializes");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

/// Returns the keys of a cursor minted for exactly this `kind`, `scope`, and `filter`.
fn decode(
    value: &str,
    kind: &str,
    scope: &str,
    filter: &str,
    key_count: usize,
) -> Result<Vec<String>, ()> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ())?;
    let cursor: PageCursor = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if cursor.version != 1
        || cursor.kind != kind
        || cursor.scope != scope
        || cursor.filter != filter
        || cursor.keys.len() != key_count
    {
        return Err(());
    }
    Ok(cursor.keys)
}

pub struct TimeIdCursor {
    pub time: DateTime<Utc>,
    pub id: Uuid,
}

pub fn encode_time_id_cursor(
    kind: &str,
    scope: &str,
    filter: &str,
    time: DateTime<Utc>,
    id: Uuid,
) -> String {
    encode(kind, scope, filter, &[&time.to_rfc3339(), &id.to_string()])
}

pub fn decode_time_id_cursor(
    value: Option<&str>,
    kind: &str,
    scope: &str,
    filter: &str,
) -> Result<Option<TimeIdCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let keys = decode(value, kind, scope, filter, 2)?;
    let time = DateTime::parse_from_rfc3339(&keys[0])
        .map_err(|_| ())?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(&keys[1]).map_err(|_| ())?;
    Ok(Some(TimeIdCursor { time, id }))
}

pub struct NumberIdCursor {
    pub number: i64,
    pub id: Uuid,
}

pub fn encode_number_id_cursor(kind: &str, scope: &str, number: i64, id: Uuid) -> String {
    encode(kind, scope, "", &[&number.to_string(), &id.to_string()])
}

pub fn decode_number_id_cursor(
    value: Option<&str>,
    kind: &str,
    scope: &str,
) -> Result<Option<NumberIdCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let keys = decode(value, kind, scope, "", 2)?;
    let number: i64 = keys[0].parse().map_err(|_| ())?;
    let id = Uuid::parse_str(&keys[1]).map_err(|_| ())?;
    Ok(Some(NumberIdCursor { number, id }))
}

pub struct OrdinalIdCursor {
    pub ordinal: i32,
    pub id: Uuid,
}

pub fn encode_ordinal_id_cursor(kind: &str, scope: &str, ordinal: i32, id: Uuid) -> String {
    encode(kind, scope, "", &[&ordinal.to_string(), &id.to_string()])
}

pub fn decode_ordinal_id_cursor(
    value: Option<&str>,
    kind: &str,
    scope: &str,
) -> Result<Option<OrdinalIdCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let keys = decode(value, kind, scope, "", 2)?;
    let ordinal: i32 = keys[0].parse().map_err(|_| ())?;
    let id = Uuid::parse_str(&keys[1]).map_err(|_| ())?;
    Ok(Some(OrdinalIdCursor { ordinal, id }))
}

pub struct TextIdCursor {
    pub text: String,
    pub id: Uuid,
}

pub fn encode_text_id_cursor(kind: &str, scope: &str, text: &str, id: Uuid) -> String {
    encode(kind, scope, "", &[text, &id.to_string()])
}

pub fn decode_text_id_cursor(
    value: Option<&str>,
    kind: &str,
    scope: &str,
) -> Result<Option<TextIdCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let mut keys = decode(value, kind, scope, "", 2)?;
    let id = Uuid::parse_str(&keys[1]).map_err(|_| ())?;
    Ok(Some(TextIdCursor {
        text: keys.swap_remove(0),
        id,
    }))
}

pub struct TargetCursor {
    pub kind: String,
    pub display_name: String,
    pub id: Uuid,
    pub environment_id: Uuid,
}

pub fn encode_target_cursor(
    scope: &str,
    filter: &str,
    kind: &str,
    display_name: &str,
    id: Uuid,
    environment_id: Uuid,
) -> String {
    encode(
        "targets",
        scope,
        filter,
        &[
            kind,
            display_name,
            &id.to_string(),
            &environment_id.to_string(),
        ],
    )
}

pub fn decode_target_cursor(
    value: Option<&str>,
    scope: &str,
    filter: &str,
) -> Result<Option<TargetCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let keys = decode(value, "targets", scope, filter, 4)?;
    let id = Uuid::parse_str(&keys[2]).map_err(|_| ())?;
    let environment_id = Uuid::parse_str(&keys[3]).map_err(|_| ())?;
    Ok(Some(TargetCursor {
        kind: keys[0].clone(),
        display_name: keys[1].clone(),
        id,
        environment_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_id_cursor_round_trips() {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let encoded = encode_time_id_cursor("runs", "scope", "filter", now, id);
        let decoded = decode_time_id_cursor(Some(&encoded), "runs", "scope", "filter")
            .unwrap()
            .unwrap();
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.time.timestamp_millis(), now.timestamp_millis());
    }

    #[test]
    fn time_id_cursor_rejects_a_mismatched_scope() {
        let encoded = encode_time_id_cursor("runs", "scope", "filter", Utc::now(), Uuid::new_v4());
        assert!(decode_time_id_cursor(Some(&encoded), "runs", "other", "filter").is_err());
    }

    #[test]
    fn target_cursor_round_trips_with_pipe_delimited_display_name() {
        let id = Uuid::new_v4();
        let environment_id = Uuid::new_v4();
        let encoded = encode_target_cursor(
            "scope",
            "filter",
            "AGENT_VERSION",
            "Display Name",
            id,
            environment_id,
        );
        let decoded = decode_target_cursor(Some(&encoded), "scope", "filter")
            .unwrap()
            .unwrap();
        assert_eq!(decoded.kind, "AGENT_VERSION");
        assert_eq!(decoded.display_name, "Display Name");
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.environment_id, environment_id);
    }

    /// `targets()` builds `scope` as `project|version` and `filter` as `kinds|environments`, so
    /// both carry the character a joined encoding would split on.
    #[test]
    fn target_cursor_round_trips_with_delimiters_in_every_text_field() {
        let id = Uuid::new_v4();
        let environment_id = Uuid::new_v4();
        let scope = format!("{}|{}", Uuid::new_v4(), Uuid::new_v4());
        let filter = "AGENT_VERSION,DEPLOYMENT|PRODUCTION,STAGING";
        let encoded = encode_target_cursor(
            &scope,
            filter,
            "AGENT_VERSION",
            "Triage | Support",
            id,
            environment_id,
        );
        let decoded = decode_target_cursor(Some(&encoded), &scope, filter)
            .unwrap()
            .unwrap();
        assert_eq!(decoded.display_name, "Triage | Support");
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.environment_id, environment_id);
        assert!(decode_target_cursor(Some(&encoded), "other", filter).is_err());
    }

    /// A run's metrics and artifacts share one scope, so only `kind` keeps their cursors apart.
    #[test]
    fn text_id_cursor_rejects_a_cursor_minted_for_another_list() {
        let encoded = encode_text_id_cursor("metrics", "run", "latency", Uuid::new_v4());
        assert!(decode_text_id_cursor(Some(&encoded), "metrics", "run")
            .unwrap()
            .is_some());
        assert!(decode_text_id_cursor(Some(&encoded), "artifacts", "run").is_err());
    }
}
