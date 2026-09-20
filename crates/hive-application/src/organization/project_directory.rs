use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use uuid::Uuid;

/// Browser-safe project summary visible only through its member organization.
/// Ports `OrganizationProject`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationProject {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

const MAX_SEARCH_LENGTH: usize = 120;

/// Canonical, literal directory filters used by both keyset cursors and PostgreSQL
/// predicates. Ports `OrganizationProjectFilter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationProjectFilter {
    lifecycle_status: Option<String>,
    search: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    #[error("The project lifecycle filter is invalid.")]
    InvalidLifecycleStatus,
    #[error("The project search filter is too long.")]
    SearchTooLong,
}

impl OrganizationProjectFilter {
    pub fn from(
        lifecycle_status: Option<String>,
        search: Option<String>,
    ) -> Result<Self, FilterError> {
        let lifecycle_status = lifecycle_status.filter(|value| !value.is_empty());
        if let Some(value) = &lifecycle_status {
            if value != "ACTIVE" && value != "ARCHIVED" {
                return Err(FilterError::InvalidLifecycleStatus);
            }
        }
        let search = search.filter(|value| !value.is_empty());
        if let Some(value) = &search {
            if value.chars().count() > MAX_SEARCH_LENGTH {
                return Err(FilterError::SearchTooLong);
            }
        }
        Ok(Self {
            lifecycle_status,
            search: search.map(|value| value.to_lowercase()),
        })
    }

    pub fn none() -> Self {
        Self {
            lifecycle_status: None,
            search: None,
        }
    }

    pub fn lifecycle_status(&self) -> Option<&str> {
        self.lifecycle_status.as_deref()
    }

    pub fn search(&self) -> Option<&str> {
        self.search.as_deref()
    }

    /// Escapes `\`, `%`, and `_` for a `LIKE ... ESCAPE '\'` predicate, matching
    /// `literalSearchPattern`.
    pub fn literal_search_pattern(&self) -> String {
        let search = self
            .search
            .as_deref()
            .expect("a search pattern is available only when search is set");
        let escaped = search
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{escaped}%")
    }
}

#[derive(Debug, Clone)]
pub struct OrganizationProjectPage {
    pub projects: Vec<OrganizationProject>,
    pub start_cursor: Option<String>,
    pub end_cursor: Option<String>,
    pub has_previous_page: bool,
    pub has_next_page: bool,
    pub total_count: i64,
}

/// Opaque cursor containing order values and the canonical directory filter that
/// produced them. Ports `OrganizationProjectCursor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationProjectCursor {
    pub display_name: String,
    pub id: Uuid,
    pub filter: OrganizationProjectFilter,
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid project cursor.")]
pub struct InvalidCursor;

#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error(transparent)]
    Invalid(#[from] InvalidCursor),
    #[error("The project cursor does not match the directory filters.")]
    FilterMismatch,
}

impl OrganizationProjectCursor {
    pub fn encode(project: &OrganizationProject, filter: &OrganizationProjectFilter) -> String {
        [
            encode_segment(&project.display_name),
            encode_segment(&project.id.to_string()),
            encode_segment(filter.lifecycle_status().unwrap_or("")),
            encode_segment(filter.search().unwrap_or("")),
        ]
        .join(".")
    }

    pub fn decode(
        encoded: &str,
        expected_filter: &OrganizationProjectFilter,
    ) -> Result<Self, CursorError> {
        let parts: Vec<&str> = encoded.split('.').collect();
        if parts.len() != 4 {
            return Err(InvalidCursor.into());
        }
        let display_name = decode_segment(parts[0]).ok_or(InvalidCursor)?;
        let id = Uuid::parse_str(&decode_segment(parts[1]).ok_or(InvalidCursor)?)
            .map_err(|_| InvalidCursor)?;
        let lifecycle_status = non_empty(decode_segment(parts[2]).ok_or(InvalidCursor)?);
        let search = non_empty(decode_segment(parts[3]).ok_or(InvalidCursor)?);
        let cursor_filter =
            OrganizationProjectFilter::from(lifecycle_status, search).map_err(|_| InvalidCursor)?;
        if &cursor_filter != expected_filter {
            return Err(CursorError::FilterMismatch);
        }
        Ok(Self {
            display_name,
            id,
            filter: cursor_filter,
        })
    }
}

fn encode_segment(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(value)
}

fn decode_segment(value: &str) -> Option<String> {
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    String::from_utf8(bytes).ok()
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> OrganizationProject {
        OrganizationProject {
            id: Uuid::parse_str("50000000-0000-0000-0000-000000000001").unwrap(),
            slug: "customer-feedback-copilot".to_string(),
            display_name: "Customer Feedback Copilot".to_string(),
            lifecycle_status: "ACTIVE".to_string(),
        }
    }

    #[test]
    fn filter_rejects_an_unrecognized_lifecycle_status() {
        assert!(matches!(
            OrganizationProjectFilter::from(Some("PENDING".to_string()), None),
            Err(FilterError::InvalidLifecycleStatus)
        ));
    }

    #[test]
    fn filter_rejects_a_search_string_over_the_length_limit() {
        let too_long = "a".repeat(MAX_SEARCH_LENGTH + 1);
        assert!(matches!(
            OrganizationProjectFilter::from(None, Some(too_long)),
            Err(FilterError::SearchTooLong)
        ));
    }

    #[test]
    fn filter_lowercases_search() {
        let filter = OrganizationProjectFilter::from(None, Some("MixedCase".to_string())).unwrap();
        assert_eq!(filter.search(), Some("mixedcase"));
    }

    #[test]
    fn literal_search_pattern_escapes_wildcards() {
        let filter = OrganizationProjectFilter::from(None, Some("50%_off".to_string())).unwrap();
        assert_eq!(filter.literal_search_pattern(), "%50\\%\\_off%");
    }

    #[test]
    fn cursor_round_trips_with_no_filter() {
        let filter = OrganizationProjectFilter::none();
        let cursor = OrganizationProjectCursor::encode(&project(), &filter);
        let decoded = OrganizationProjectCursor::decode(&cursor, &filter).unwrap();
        assert_eq!(decoded.display_name, project().display_name);
        assert_eq!(decoded.id, project().id);
    }

    #[test]
    fn cursor_round_trips_with_a_filter() {
        let filter = OrganizationProjectFilter::from(
            Some("ACTIVE".to_string()),
            Some("copilot".to_string()),
        )
        .unwrap();
        let cursor = OrganizationProjectCursor::encode(&project(), &filter);
        let decoded = OrganizationProjectCursor::decode(&cursor, &filter).unwrap();
        assert_eq!(decoded.filter, filter);
    }

    #[test]
    fn cursor_rejects_a_mismatched_filter() {
        let issued_filter = OrganizationProjectFilter::none();
        let cursor = OrganizationProjectCursor::encode(&project(), &issued_filter);
        let different_filter =
            OrganizationProjectFilter::from(Some("ARCHIVED".to_string()), None).unwrap();
        assert!(matches!(
            OrganizationProjectCursor::decode(&cursor, &different_filter),
            Err(CursorError::FilterMismatch)
        ));
    }

    #[test]
    fn cursor_rejects_malformed_input() {
        let filter = OrganizationProjectFilter::none();
        assert!(OrganizationProjectCursor::decode("not.enough.parts", &filter).is_err());
    }
}
