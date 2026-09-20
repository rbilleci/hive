//! Ports `PageCursor`/`pageCursor`/`decodePageCursor`: the cursor encode/decode
//! pairs for every evaluation keyset query. Unlike Java's JSON+base64
//! `PageCursor` record, cursors are opaque strings to every client, so this
//! mirrors the deployment domain's simpler pipe-delimited encoding instead —
//! `scope`/`filter` are still embedded and checked on decode, rejecting a
//! cursor minted against a different scope or filter, matching Java's own
//! `kind`/`scope`/`filter` equality checks in `decodePageCursor`.

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

pub struct TimeIdCursor {
    pub time: DateTime<Utc>,
    pub id: Uuid,
}

pub fn encode_time_id_cursor(scope: &str, filter: &str, time: DateTime<Utc>, id: Uuid) -> String {
    encode(&format!("v1|{scope}|{filter}|{}|{id}", time.to_rfc3339()))
}

pub fn decode_time_id_cursor(
    value: Option<&str>,
    scope: &str,
    filter: &str,
) -> Result<Option<TimeIdCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let decoded = decode(value).ok_or(())?;
    let parts: Vec<&str> = decoded.split('|').collect();
    if parts.len() != 5 || parts[0] != "v1" || parts[1] != scope || parts[2] != filter {
        return Err(());
    }
    let time = DateTime::parse_from_rfc3339(parts[3])
        .map_err(|_| ())?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(parts[4]).map_err(|_| ())?;
    Ok(Some(TimeIdCursor { time, id }))
}

pub struct NumberIdCursor {
    pub number: i64,
    pub id: Uuid,
}

pub fn encode_number_id_cursor(scope: &str, number: i64, id: Uuid) -> String {
    encode(&format!("v1|{scope}|{number}|{id}"))
}

pub fn decode_number_id_cursor(
    value: Option<&str>,
    scope: &str,
) -> Result<Option<NumberIdCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let decoded = decode(value).ok_or(())?;
    let parts: Vec<&str> = decoded.split('|').collect();
    if parts.len() != 4 || parts[0] != "v1" || parts[1] != scope {
        return Err(());
    }
    let number: i64 = parts[2].parse().map_err(|_| ())?;
    let id = Uuid::parse_str(parts[3]).map_err(|_| ())?;
    Ok(Some(NumberIdCursor { number, id }))
}

pub struct OrdinalIdCursor {
    pub ordinal: i32,
    pub id: Uuid,
}

pub fn encode_ordinal_id_cursor(scope: &str, ordinal: i32, id: Uuid) -> String {
    encode(&format!("v1|{scope}|{ordinal}|{id}"))
}

pub fn decode_ordinal_id_cursor(
    value: Option<&str>,
    scope: &str,
) -> Result<Option<OrdinalIdCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let decoded = decode(value).ok_or(())?;
    let parts: Vec<&str> = decoded.split('|').collect();
    if parts.len() != 4 || parts[0] != "v1" || parts[1] != scope {
        return Err(());
    }
    let ordinal: i32 = parts[2].parse().map_err(|_| ())?;
    let id = Uuid::parse_str(parts[3]).map_err(|_| ())?;
    Ok(Some(OrdinalIdCursor { ordinal, id }))
}

pub struct TextIdCursor {
    pub text: String,
    pub id: Uuid,
}

pub fn encode_text_id_cursor(scope: &str, text: &str, id: Uuid) -> String {
    encode(&format!("v1|{scope}|{text}|{id}"))
}

pub fn decode_text_id_cursor(value: Option<&str>, scope: &str) -> Result<Option<TextIdCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let decoded = decode(value).ok_or(())?;
    let parts: Vec<&str> = decoded.splitn(4, '|').collect();
    if parts.len() != 4 || parts[0] != "v1" || parts[1] != scope {
        return Err(());
    }
    let id = Uuid::parse_str(parts[3]).map_err(|_| ())?;
    Ok(Some(TextIdCursor {
        text: parts[2].to_string(),
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
    encode(&format!(
        "v1|{scope}|{filter}|{kind}|{display_name}|{id}|{environment_id}"
    ))
}

pub fn decode_target_cursor(
    value: Option<&str>,
    scope: &str,
    filter: &str,
) -> Result<Option<TargetCursor>, ()> {
    let Some(value) = value else { return Ok(None) };
    let decoded = decode(value).ok_or(())?;
    let parts: Vec<&str> = decoded.splitn(7, '|').collect();
    if parts.len() != 7 || parts[0] != "v1" || parts[1] != scope || parts[2] != filter {
        return Err(());
    }
    let id = Uuid::parse_str(parts[5]).map_err(|_| ())?;
    let environment_id = Uuid::parse_str(parts[6]).map_err(|_| ())?;
    Ok(Some(TargetCursor {
        kind: parts[3].to_string(),
        display_name: parts[4].to_string(),
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
        let encoded = encode_time_id_cursor("scope", "filter", now, id);
        let decoded = decode_time_id_cursor(Some(&encoded), "scope", "filter")
            .unwrap()
            .unwrap();
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.time.timestamp_millis(), now.timestamp_millis());
    }

    #[test]
    fn time_id_cursor_rejects_a_mismatched_scope() {
        let encoded = encode_time_id_cursor("scope", "filter", Utc::now(), Uuid::new_v4());
        assert!(decode_time_id_cursor(Some(&encoded), "other", "filter").is_err());
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
}
