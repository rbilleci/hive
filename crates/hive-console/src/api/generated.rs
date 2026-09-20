//! Input types of the Seaography-generated API that more than one operation uses: filters and
//! `pagination`. Only the operations the console sends are declared.

use crate::graphql::schema;

#[derive(cynic::InputObject, Debug, Clone)]
pub struct PageInput {
    pub limit: i32,
    pub page: i32,
}

/// Seaography's `pagination` argument is a `@oneOf` input; the console pages by page number.
#[derive(cynic::InputObject, Debug, Clone)]
pub enum PaginationInput {
    Page(PageInput),
}

/// The filter Seaography generates for id and other `Text`/`Uuid` columns, and for timestamps.
#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct TextFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub eq: Option<String>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub gte: Option<String>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub lte: Option<String>,
}

impl TextFilterInput {
    pub fn eq(value: &str) -> Self {
        Self {
            eq: Some(value.to_string()),
            ..Default::default()
        }
    }
}

/// The filter Seaography generates for `String` columns.
#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct StringFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub eq: Option<String>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub ne: Option<String>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub ilike: Option<String>,
}

impl StringFilterInput {
    pub fn eq(value: &str) -> Self {
        Self {
            eq: Some(value.to_string()),
            ..Default::default()
        }
    }

    pub fn ne(value: &str) -> Self {
        Self {
            ne: Some(value.to_string()),
            ..Default::default()
        }
    }

    pub fn ilike(pattern: &str) -> Self {
        Self {
            ilike: Some(pattern.to_string()),
            ..Default::default()
        }
    }
}

/// Whether `value` is written as a UUID. Seaography answers a malformed id filter with an error,
/// not with no rows, so a page that takes an id from its route checks it before asking.
pub fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.chars().enumerate().all(|(index, character)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                character == '-'
            } else {
                character.is_ascii_hexdigit()
            }
        })
}

/// A `LIKE` pattern matching `text` literally anywhere: `%`, `_` and `\` lose their meaning.
pub fn like_pattern(text: &str) -> String {
    let mut pattern = String::with_capacity(text.len() + 2);
    pattern.push('%');
    for character in text.chars() {
        if matches!(character, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

#[derive(cynic::Enum, Debug, Clone, Copy)]
pub enum OrderByEnum {
    Asc,
    Desc,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct OrganizationsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub lifecycle_status: Option<StringFilterInput>,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct OrganizationsOrderInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<OrderByEnum>,
    /// The tie-break; see `directory::by_name`.
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<OrderByEnum>,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct ProjectsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub lifecycle_status: Option<StringFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<StringFilterInput>,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct ProjectsOrderInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<OrderByEnum>,
    /// The tie-break; see `directory::by_name`.
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<OrderByEnum>,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct AgentsOrderInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<OrderByEnum>,
    /// The tie-break; see `directory::by_name`.
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<OrderByEnum>,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct AgentVersionsOrderInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub version_number: Option<OrderByEnum>,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct AgentVersionsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<TextFilterInput>,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct AgentsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub lifecycle_status: Option<StringFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<StringFilterInput>,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct AgentOperationalViewProjectionFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<TextFilterInput>,
}

#[cfg(test)]
mod tests {
    use super::is_uuid;

    #[test]
    fn is_uuid_accepts_only_the_hyphenated_form() {
        assert!(is_uuid("60000000-0000-0000-0000-00000000000A"));
        assert!(!is_uuid("not-a-uuid"));
        assert!(!is_uuid("60000000000000000000000000000001"));
        assert!(!is_uuid("60000000-0000-0000-0000-00000000000g"));
    }
}
