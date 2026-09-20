use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use uuid::Uuid;

/// Browser-safe agent identity and lifecycle values for one project directory row.
/// `latest_published_version` and `model` are `None` for an agent that has never
/// published a version; both otherwise come from that agent's highest-numbered
/// published version. Ports `Agent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub latest_published_version: Option<i32>,
    pub model: Option<String>,
}

/// Project context returned only after the requesting principal passes the
/// active-membership scope. Ports `AgentDirectoryProject`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDirectoryProject {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

const MAX_SEARCH_LENGTH: usize = 120;

/// Canonical literal filters carried by agent-directory cursors and PostgreSQL
/// predicates. Ports `AgentFilter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFilter {
    lifecycle_status: Option<String>,
    search: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    #[error("The agent lifecycle filter is invalid.")]
    InvalidLifecycleStatus,
    #[error("The agent search filter is too long.")]
    SearchTooLong,
}

impl AgentFilter {
    pub fn from(
        lifecycle_status: Option<String>,
        search: Option<String>,
    ) -> Result<Self, FilterError> {
        let lifecycle_status = lifecycle_status.filter(|value| !value.is_empty());
        if let Some(value) = &lifecycle_status {
            if value != "ACTIVE" && value != "DEPRECATED" && value != "ARCHIVED" {
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
pub struct AgentPage {
    pub agents: Vec<Agent>,
    pub start_cursor: Option<String>,
    pub end_cursor: Option<String>,
    pub has_previous_page: bool,
    pub has_next_page: bool,
    pub total_count: i64,
}

/// Opaque keyset cursor with normalized (lowercased) ordering data and the filter
/// that produced it. Ports `AgentCursor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCursor {
    pub sort_name: String,
    pub id: Uuid,
    pub filter: AgentFilter,
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid agent cursor.")]
pub struct InvalidCursor;

#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error(transparent)]
    Invalid(#[from] InvalidCursor),
    #[error("The agent cursor does not match the directory filters.")]
    FilterMismatch,
}

impl AgentCursor {
    pub fn encode(agent: &Agent, filter: &AgentFilter) -> String {
        [
            encode_segment(&agent.display_name.to_lowercase()),
            encode_segment(&agent.id.to_string()),
            encode_segment(filter.lifecycle_status().unwrap_or("")),
            encode_segment(filter.search().unwrap_or("")),
        ]
        .join(".")
    }

    pub fn decode(encoded: &str, expected_filter: &AgentFilter) -> Result<Self, CursorError> {
        let parts: Vec<&str> = encoded.split('.').collect();
        if parts.len() != 4 {
            return Err(InvalidCursor.into());
        }
        let sort_name = decode_segment(parts[0]).ok_or(InvalidCursor)?;
        let id = Uuid::parse_str(&decode_segment(parts[1]).ok_or(InvalidCursor)?)
            .map_err(|_| InvalidCursor)?;
        let lifecycle_status = non_empty(decode_segment(parts[2]).ok_or(InvalidCursor)?);
        let search = non_empty(decode_segment(parts[3]).ok_or(InvalidCursor)?);
        let cursor_filter =
            AgentFilter::from(lifecycle_status, search).map_err(|_| InvalidCursor)?;
        if &cursor_filter != expected_filter {
            return Err(CursorError::FilterMismatch);
        }
        Ok(Self {
            sort_name,
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

    fn agent() -> Agent {
        Agent {
            id: Uuid::parse_str("60000000-0000-0000-0000-000000000001").unwrap(),
            slug: "feedback-triage-agent".to_string(),
            display_name: "Feedback Triage Agent".to_string(),
            lifecycle_status: "ACTIVE".to_string(),
            latest_published_version: Some(3),
            model: Some("anthropic/claude".to_string()),
        }
    }

    #[test]
    fn filter_rejects_an_unrecognized_lifecycle_status() {
        assert!(matches!(
            AgentFilter::from(Some("PENDING".to_string()), None),
            Err(FilterError::InvalidLifecycleStatus)
        ));
    }

    #[test]
    fn filter_accepts_deprecated_which_projects_does_not() {
        assert!(AgentFilter::from(Some("DEPRECATED".to_string()), None).is_ok());
    }

    #[test]
    fn filter_rejects_a_search_string_over_the_length_limit() {
        let too_long = "a".repeat(MAX_SEARCH_LENGTH + 1);
        assert!(matches!(
            AgentFilter::from(None, Some(too_long)),
            Err(FilterError::SearchTooLong)
        ));
    }

    #[test]
    fn cursor_sort_name_is_lowercased_even_when_display_name_is_not() {
        let filter = AgentFilter::none();
        let cursor = AgentCursor::encode(&agent(), &filter);
        let decoded = AgentCursor::decode(&cursor, &filter).unwrap();
        assert_eq!(decoded.sort_name, "feedback triage agent");
    }

    #[test]
    fn cursor_round_trips_with_a_filter() {
        let filter =
            AgentFilter::from(Some("DEPRECATED".to_string()), Some("triage".to_string())).unwrap();
        let cursor = AgentCursor::encode(&agent(), &filter);
        let decoded = AgentCursor::decode(&cursor, &filter).unwrap();
        assert_eq!(decoded.filter, filter);
    }

    #[test]
    fn cursor_rejects_a_mismatched_filter() {
        let issued_filter = AgentFilter::none();
        let cursor = AgentCursor::encode(&agent(), &issued_filter);
        let different_filter = AgentFilter::from(Some("ARCHIVED".to_string()), None).unwrap();
        assert!(matches!(
            AgentCursor::decode(&cursor, &different_filter),
            Err(CursorError::FilterMismatch)
        ));
    }
}
