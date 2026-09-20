//! Ports the `ConsoleShell` operation of `core.graphql` and the capability rules of `consoleModel.ts`.

use crate::graphql::{execute, schema, GraphqlError};
use cynic::{MutationBuilder, QueryBuilder};

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ConsolePrincipal {
    pub id: cynic::Id,
    pub subject: String,
    pub display_name: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ConsoleProject {
    pub id: cynic::Id,
    pub organization_id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ConsoleOrganization {
    pub id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub projects: Vec<ConsoleProject>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EffectiveCapability {
    pub code: String,
    pub scope_type: String,
    pub scope_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ConsoleContext {
    pub principal: ConsolePrincipal,
    pub organizations: Vec<ConsoleOrganization>,
    pub capabilities: Vec<EffectiveCapability>,
    pub revision: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct DisplayPreferences {
    pub color_scheme: String,
    pub density: String,
    pub sidebar_state: String,
}

impl DisplayPreferences {
    /// Ports `lightDefaults`: what the shell shows for an account with no stored choice.
    pub fn light_defaults() -> Self {
        Self {
            color_scheme: "LIGHT".to_string(),
            density: "COMFORTABLE".to_string(),
            sidebar_state: "EXPANDED".to_string(),
        }
    }

    fn supported(&self) -> bool {
        ["SYSTEM", "LIGHT", "DARK"].contains(&self.color_scheme.as_str())
            && ["COMFORTABLE", "COMPACT"].contains(&self.density.as_str())
            && ["EXPANDED", "COLLAPSED"].contains(&self.sidebar_state.as_str())
    }
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query")]
pub struct ConsoleShell {
    pub console_context: Option<ConsoleContext>,
    pub display_preferences: Option<DisplayPreferences>,
}

pub enum ConsoleAccess {
    Ready(ConsoleContext, DisplayPreferences),
    SessionError,
    AccessDenied,
}

const UNSUPPORTED_PREFERENCES: &str =
    "The GraphQL service returned unsupported display preferences.";

pub async fn request_console() -> Result<ConsoleAccess, GraphqlError> {
    let data = match execute(ConsoleShell::build(())).await {
        Ok(data) => data,
        Err(GraphqlError::SessionExpired) => return Ok(ConsoleAccess::SessionError),
        Err(error) => return Err(error),
    };
    let Some(context) = data.console_context else {
        return Ok(ConsoleAccess::AccessDenied);
    };
    let preferences = data
        .display_preferences
        .unwrap_or_else(DisplayPreferences::light_defaults);
    if !preferences.supported() {
        return Err(GraphqlError::Transport(UNSUPPORTED_PREFERENCES.to_string()));
    }
    Ok(ConsoleAccess::Ready(context, preferences))
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdateDisplayPreferencesInput {
    pub color_scheme: String,
    pub density: String,
    pub sidebar_state: String,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "DisplayPreferencesProblem")]
pub struct DisplayPreferencesProblemFields {
    pub message: String,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct DisplayPreferencesMutationPayload {
    pub display_preferences: Option<DisplayPreferences>,
    pub problems: Vec<DisplayPreferencesProblemFields>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct UpdateDisplayPreferencesVariables {
    pub input: UpdateDisplayPreferencesInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    graphql_type = "Mutation",
    variables = "UpdateDisplayPreferencesVariables"
)]
pub struct UpdateDisplayPreferences {
    #[arguments(input: $input)]
    pub update_display_preferences: DisplayPreferencesMutationPayload,
}

/// Ports `saveDisplayPreferences`. `Err` carries the message the page shows.
pub async fn save_display_preferences(
    preferences: &DisplayPreferences,
) -> Result<DisplayPreferences, String> {
    let input = UpdateDisplayPreferencesInput {
        color_scheme: preferences.color_scheme.clone(),
        density: preferences.density.clone(),
        sidebar_state: preferences.sidebar_state.clone(),
    };
    match execute(UpdateDisplayPreferences::build(
        UpdateDisplayPreferencesVariables { input },
    ))
    .await
    {
        Err(GraphqlError::SessionExpired) => {
            let _ = web_sys::window()
                .expect("a browser window")
                .location()
                .assign("/session-error");
            Err("Your session expired.".to_string())
        }
        Err(GraphqlError::Transport(message)) => Err(message),
        Ok(data) => match data.update_display_preferences.display_preferences {
            Some(saved) if saved.supported() => Ok(saved),
            Some(_) => Err(UNSUPPORTED_PREFERENCES.to_string()),
            None => Err(data
                .update_display_preferences
                .problems
                .into_iter()
                .next()
                .map_or_else(
                    || "We could not save display preferences.".to_string(),
                    |problem| problem.message,
                )),
        },
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct RequestedScope<'a> {
    pub scope_type: &'static str,
    pub id: &'a str,
}

/// Ports `requestedScope`: the organization or project a console path addresses.
pub fn requested_scope(pathname: &str) -> Option<RequestedScope<'_>> {
    for (prefix, scope_type) in [
        ("/organizations/", "ORGANIZATION"),
        ("/projects/", "PROJECT"),
    ] {
        if let Some(rest) = pathname.strip_prefix(prefix) {
            let id = rest.split('/').next().unwrap_or_default();
            if !id.is_empty() {
                return Some(RequestedScope { scope_type, id });
            }
        }
    }
    None
}

pub fn has_capability(
    context: &ConsoleContext,
    code: &str,
    scope_type: &str,
    scope_id: &str,
) -> bool {
    context.capabilities.iter().any(|capability| {
        capability.code == code
            && capability.scope_type == scope_type
            && capability.scope_id.inner() == scope_id
    })
}

pub fn has_any_capability(
    context: &ConsoleContext,
    codes: &[&str],
    scope_type: &str,
    scope_id: &str,
) -> bool {
    codes
        .iter()
        .any(|code| has_capability(context, code, scope_type, scope_id))
}

pub const ORGANIZATION_SETTINGS_CAPABILITIES: [&str; 8] = [
    "ORGANIZATION.UPDATE",
    "ORGANIZATION.ARCHIVE",
    "ORGANIZATION.RESTORE",
    "ORGANIZATION_MEMBERSHIP.VIEW",
    "ORGANIZATION_MEMBERSHIP.ADD",
    "ORGANIZATION_MEMBERSHIP.CHANGE_ROLES",
    "ORGANIZATION_MEMBERSHIP.END",
    "PROJECT.CREATE",
];

pub const PROJECT_SETTINGS_CAPABILITIES: [&str; 11] = [
    "PROJECT.UPDATE",
    "PROJECT.ARCHIVE",
    "PROJECT.RESTORE",
    "PROJECT_MEMBERSHIP.VIEW",
    "PROJECT_MEMBERSHIP.ADD",
    "PROJECT_MEMBERSHIP.CHANGE_ROLES",
    "PROJECT_MEMBERSHIP.END",
    "PROJECT_BUDGET.VIEW",
    "PROJECT_BUDGET.UPDATE",
    "PROJECT_APPROVAL_POLICY.VIEW",
    "PROJECT_APPROVAL_POLICY.UPDATE",
];

/// Ports `selectedContext`: the organization and project a path selects, as identifiers.
pub fn selected_context(context: &ConsoleContext, pathname: &str) -> (String, String) {
    match requested_scope(pathname) {
        Some(scope) if scope.scope_type == "ORGANIZATION" => (scope.id.to_string(), String::new()),
        Some(scope) => {
            let organization = context
                .organizations
                .iter()
                .find(|organization| {
                    organization
                        .projects
                        .iter()
                        .any(|project| project.id.inner() == scope.id)
                })
                .map(|organization| organization.id.inner().to_string())
                .unwrap_or_default();
            (organization, scope.id.to_string())
        }
        None => (String::new(), String::new()),
    }
}

/// Ports `hasViewCapability`: whether the verified context may open this path at all.
pub fn has_view_capability(context: &ConsoleContext, pathname: &str) -> bool {
    let segments: Vec<&str> = pathname.trim_matches('/').split('/').collect();
    if matches!(
        segments.as_slice(),
        ["organizations" | "projects", _, "audit"]
    ) {
        return true;
    }
    if pathname == "/preferences" {
        return has_capability(
            context,
            "PREFERENCES.UPDATE",
            "PRINCIPAL",
            context.principal.id.inner(),
        );
    }
    match requested_scope(pathname) {
        None => true,
        Some(scope) => {
            let code = if scope.scope_type == "ORGANIZATION" {
                "ORGANIZATION.VIEW"
            } else {
                "PROJECT.VIEW"
            };
            has_capability(context, code, scope.scope_type, scope.id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_scope_reads_the_first_identifier_segment() {
        assert_eq!(
            requested_scope("/projects/p1/agents/a1/edit"),
            Some(RequestedScope {
                scope_type: "PROJECT",
                id: "p1"
            })
        );
        assert_eq!(
            requested_scope("/organizations/o1"),
            Some(RequestedScope {
                scope_type: "ORGANIZATION",
                id: "o1"
            })
        );
        assert_eq!(requested_scope("/preferences"), None);
        assert_eq!(requested_scope("/projects/"), None);
    }
}
