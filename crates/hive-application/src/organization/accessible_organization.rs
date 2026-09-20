use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use uuid::Uuid;

/// One organization row as `accessibleOrganizations` projects it. Ports
/// `AccessibleOrganization`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibleOrganization {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

/// The opaque keyset cursor: exactly the `(display_name, id)` pair that defines the
/// query order. Ports `AccessibleOrganizationCursor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibleOrganizationCursor {
    pub display_name: String,
    pub id: Uuid,
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid selector cursor.")]
pub struct InvalidCursor;

impl AccessibleOrganizationCursor {
    pub fn encode(organization: &AccessibleOrganization) -> String {
        let value = format!("{}\t{}", organization.display_name, organization.id);
        URL_SAFE_NO_PAD.encode(value)
    }

    pub fn decode(encoded: &str) -> Result<Self, InvalidCursor> {
        let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| InvalidCursor)?;
        let value = String::from_utf8(bytes).map_err(|_| InvalidCursor)?;
        let separator = value
            .rfind('\t')
            .filter(|position| *position > 0)
            .ok_or(InvalidCursor)?;
        let (display_name, id_text) = value.split_at(separator);
        let id = Uuid::parse_str(&id_text[1..]).map_err(|_| InvalidCursor)?;
        Ok(Self {
            display_name: display_name.to_string(),
            id,
        })
    }
}

#[derive(Debug, Clone)]
pub struct AccessibleOrganizationPage {
    pub organizations: Vec<AccessibleOrganization>,
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
    pub total_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn organization() -> AccessibleOrganization {
        AccessibleOrganization {
            id: Uuid::parse_str("10000000-0000-0000-0000-000000000001").unwrap(),
            slug: "product".to_string(),
            display_name: "Alpha Command".to_string(),
            lifecycle_status: "ACTIVE".to_string(),
        }
    }

    #[test]
    fn round_trips_through_encode_and_decode() {
        let organization = organization();
        let cursor = AccessibleOrganizationCursor::encode(&organization);
        let decoded = AccessibleOrganizationCursor::decode(&cursor).unwrap();
        assert_eq!(decoded.display_name, organization.display_name);
        assert_eq!(decoded.id, organization.id);
    }

    #[test]
    fn rejects_a_cursor_with_no_tab_separator() {
        let bogus = URL_SAFE_NO_PAD.encode("no-separator-here");
        assert!(AccessibleOrganizationCursor::decode(&bogus).is_err());
    }

    #[test]
    fn rejects_a_cursor_whose_trailing_segment_is_not_a_uuid() {
        let bogus = URL_SAFE_NO_PAD.encode("Alpha Command\tnot-a-uuid");
        assert!(AccessibleOrganizationCursor::decode(&bogus).is_err());
    }

    #[test]
    fn rejects_non_base64_input() {
        assert!(AccessibleOrganizationCursor::decode("not valid base64!!").is_err());
    }
}
