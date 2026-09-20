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

/// The filter Seaography generates for id and other `Text`/`Uuid` columns.
#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct TextFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub eq: Option<String>,
}

impl TextFilterInput {
    pub fn eq(value: &str) -> Self {
        Self {
            eq: Some(value.to_string()),
        }
    }
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
}
