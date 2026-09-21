//! The `ConsoleShell` operation over the generated API, and the console's capability rules.

use crate::api::generated::{OrderByEnum, OrganizationsOrderInput, ProjectsOrderInput};
use crate::graphql::{execute, schema, GraphqlError};
use cynic::{MutationBuilder, QueryBuilder};

/// The signed-in principal, as the shell and the pages read it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsolePrincipal {
    pub id: cynic::Id,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleProject {
    pub id: cynic::Id,
    pub organization_id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleOrganization {
    pub id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub projects: Vec<ConsoleProject>,
}

/// One capability code the principal holds at one scope.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveCapability {
    pub code: String,
    pub scope_type: String,
    pub scope_id: cynic::Id,
}

/// The access state the shell verified. `revision` is a fingerprint of it, computed here: it
/// changes when the visible organizations, projects or capabilities change, and pages reload on it.
#[derive(Debug, Clone, PartialEq)]
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
    /// What the shell shows for an account with no stored choice.
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

// `ConsoleShell` reads the generated API: the principal's own row, the organizations and projects
// it can see, the computed `capabilities` on each, and its stored display preferences. The server
// scopes every connection to the requester.

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Principals")]
pub struct PrincipalRow {
    pub id: String,
    pub display_name: String,
    pub capabilities: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "PrincipalsConnection")]
pub struct PrincipalRows {
    pub nodes: Vec<PrincipalRow>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Projects")]
pub struct ProjectRow {
    pub id: String,
    pub organization_id: String,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub capabilities: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "ProjectsConnection")]
pub struct ProjectRows {
    pub nodes: Vec<ProjectRow>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Organizations", variables = "ConsoleShellVariables")]
pub struct OrganizationRow {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub capabilities: Vec<String>,
    #[arguments(orderBy: $projects_by)]
    pub projects: ProjectRows,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    graphql_type = "OrganizationsConnection",
    variables = "ConsoleShellVariables"
)]
pub struct OrganizationRows {
    pub nodes: Vec<OrganizationRow>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "PrincipalDisplayPreferences")]
pub struct PreferencesRow {
    pub color_scheme: String,
    pub density: String,
    pub sidebar_state: Option<String>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "PrincipalDisplayPreferencesConnection")]
pub struct PreferencesRows {
    pub nodes: Vec<PreferencesRow>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ConsoleShellVariables {
    pub organizations_by: OrganizationsOrderInput,
    pub projects_by: ProjectsOrderInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ConsoleShellVariables")]
pub struct ConsoleShell {
    pub principals: PrincipalRows,
    #[arguments(orderBy: $organizations_by)]
    pub organizations: OrganizationRows,
    pub principal_display_preferences: PreferencesRows,
}

pub enum ConsoleAccess {
    Ready(ConsoleContext, DisplayPreferences),
    SessionError,
}

const UNSUPPORTED_PREFERENCES: &str =
    "The GraphQL service returned unsupported display preferences.";

fn scoped<'a>(
    codes: Vec<String>,
    scope_type: &'static str,
    scope_id: &'a str,
) -> impl Iterator<Item = EffectiveCapability> + 'a {
    codes.into_iter().map(move |code| EffectiveCapability {
        code,
        scope_type: scope_type.to_string(),
        scope_id: scope_id.into(),
    })
}

/// The fingerprint of an access state: every visible scope with its lifecycle status, and every
/// capability held there. The server sorts capability codes and the console orders the scopes, so
/// equal access gives an equal fingerprint.
fn fingerprint(
    principal: &ConsolePrincipal,
    organizations: &[ConsoleOrganization],
    capabilities: &[EffectiveCapability],
) -> String {
    let mut parts = vec![principal.id.inner().to_string()];
    for organization in organizations {
        parts.push(format!(
            "O:{}:{}",
            organization.id.inner(),
            organization.lifecycle_status
        ));
        for project in &organization.projects {
            parts.push(format!(
                "P:{}:{}",
                project.id.inner(),
                project.lifecycle_status
            ));
        }
    }
    for capability in capabilities {
        parts.push(format!(
            "C:{}:{}:{}",
            capability.scope_type,
            capability.scope_id.inner(),
            capability.code
        ));
    }
    parts.join("|")
}

/// Builds the console's context from the generated rows. A verified principal with no stored
/// profile has no row: it gets an empty, default-deny context.
fn console_context(principals: PrincipalRows, organizations: OrganizationRows) -> ConsoleContext {
    let mut capabilities = Vec::new();
    let principal = match principals.nodes.into_iter().next() {
        Some(row) => {
            capabilities.extend(scoped(row.capabilities, "PRINCIPAL", &row.id));
            ConsolePrincipal {
                id: row.id.into(),
                display_name: row.display_name,
            }
        }
        None => ConsolePrincipal {
            id: "".into(),
            display_name: "Local user".to_string(),
        },
    };
    let organizations: Vec<ConsoleOrganization> = organizations
        .nodes
        .into_iter()
        .map(|organization| {
            capabilities.extend(scoped(
                organization.capabilities,
                "ORGANIZATION",
                &organization.id,
            ));
            let projects = organization
                .projects
                .nodes
                .into_iter()
                .map(|project| {
                    capabilities.extend(scoped(project.capabilities, "PROJECT", &project.id));
                    ConsoleProject {
                        id: project.id.into(),
                        organization_id: project.organization_id.into(),
                        slug: project.slug,
                        display_name: project.display_name,
                        lifecycle_status: project.lifecycle_status,
                    }
                })
                .collect();
            ConsoleOrganization {
                id: organization.id.into(),
                slug: organization.slug,
                display_name: organization.display_name,
                lifecycle_status: organization.lifecycle_status,
                projects,
            }
        })
        .collect();
    let revision = fingerprint(&principal, &organizations, &capabilities);
    ConsoleContext {
        principal,
        organizations,
        capabilities,
        revision,
    }
}

pub async fn request_console() -> Result<ConsoleAccess, GraphqlError> {
    // The primary key is the final tie-break of every ordering; see `directory::ascending`.
    let variables = ConsoleShellVariables {
        organizations_by: OrganizationsOrderInput {
            display_name: Some(OrderByEnum::Asc),
            id: Some(OrderByEnum::Asc),
        },
        projects_by: ProjectsOrderInput {
            display_name: Some(OrderByEnum::Asc),
            id: Some(OrderByEnum::Asc),
        },
    };
    let data = match execute(ConsoleShell::build(variables)).await {
        Ok(data) => data,
        Err(GraphqlError::SessionExpired) => return Ok(ConsoleAccess::SessionError),
        Err(error) => return Err(error),
    };
    let defaults = DisplayPreferences::light_defaults();
    let preferences = data
        .principal_display_preferences
        .nodes
        .into_iter()
        .next()
        .map_or(defaults.clone(), |row| DisplayPreferences {
            color_scheme: row.color_scheme,
            density: row.density,
            sidebar_state: row.sidebar_state.unwrap_or(defaults.sidebar_state),
        });
    if !preferences.supported() {
        return Err(GraphqlError::Transport(UNSUPPORTED_PREFERENCES.to_string()));
    }
    Ok(ConsoleAccess::Ready(
        console_context(data.principals, data.organizations),
        preferences,
    ))
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdateDisplayPreferencesInput {
    pub color_scheme: String,
    pub density: String,
    pub sidebar_state: String,
}

/// The one problem type every command payload lists its refusals with.
#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Problem")]
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

/// `Err` carries the message the page shows.
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

/// The organization or project a console path addresses.
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

/// The organization and project a path selects, as identifiers.
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

/// Whether the verified context may open this path at all.
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
