use crate::schema::RequestPrincipal;
use async_graphql::{Context, InputObject, Interface, Object, SimpleObject};
use hive_application::console::{
    ConsoleCapability as AppConsoleCapability, ConsoleContext as AppConsoleContext,
    ConsoleContextService, ConsoleOrganization as AppConsoleOrganization,
    ConsoleProject as AppConsoleProject, DisplayPreferencesProblem as AppDisplayPreferencesProblem,
};
use hive_persistence::console::PgConsoleRepository;
use uuid::Uuid;

/// Mirrors the SDL's `ConsolePrincipal { id, subject, displayName }` — distinct
/// from the top-level `Principal` type `currentPrincipal` returns, which has no
/// `displayName`.
#[derive(SimpleObject)]
pub struct ConsolePrincipal {
    pub id: async_graphql::ID,
    pub subject: String,
    pub display_name: String,
}

/// Ports `ConsoleCapability`; the SDL calls this type `EffectiveCapability`.
#[derive(SimpleObject)]
pub struct EffectiveCapability {
    pub code: String,
    pub scope_type: String,
    pub scope_id: async_graphql::ID,
}

#[derive(SimpleObject)]
pub struct ConsoleProject {
    pub id: async_graphql::ID,
    pub organization_id: async_graphql::ID,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

#[derive(SimpleObject)]
pub struct ConsoleOrganization {
    pub id: async_graphql::ID,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub projects: Vec<ConsoleProject>,
}

#[derive(SimpleObject)]
pub struct ConsoleContext {
    pub principal: ConsolePrincipal,
    pub organizations: Vec<ConsoleOrganization>,
    pub capabilities: Vec<EffectiveCapability>,
    pub revision: String,
}

impl From<AppConsoleProject> for ConsoleProject {
    fn from(project: AppConsoleProject) -> Self {
        Self {
            id: async_graphql::ID(project.id.to_string()),
            organization_id: async_graphql::ID(project.organization_id.to_string()),
            slug: project.slug,
            display_name: project.display_name,
            lifecycle_status: project.lifecycle_status,
        }
    }
}

impl From<AppConsoleOrganization> for ConsoleOrganization {
    fn from(organization: AppConsoleOrganization) -> Self {
        Self {
            id: async_graphql::ID(organization.id.to_string()),
            slug: organization.slug,
            display_name: organization.display_name,
            lifecycle_status: organization.lifecycle_status,
            projects: organization
                .projects
                .into_iter()
                .map(ConsoleProject::from)
                .collect(),
        }
    }
}

impl From<AppConsoleCapability> for EffectiveCapability {
    fn from(capability: AppConsoleCapability) -> Self {
        Self {
            code: capability.code,
            scope_type: capability.scope_type,
            scope_id: async_graphql::ID(capability.scope_id.to_string()),
        }
    }
}

impl From<AppConsoleContext> for ConsoleContext {
    fn from(context: AppConsoleContext) -> Self {
        Self {
            principal: ConsolePrincipal {
                id: async_graphql::ID(context.principal_id.to_string()),
                subject: context.principal_id.to_string(),
                display_name: context.display_name,
            },
            organizations: context
                .organizations
                .into_iter()
                .map(ConsoleOrganization::from)
                .collect(),
            capabilities: context
                .capabilities
                .into_iter()
                .map(EffectiveCapability::from)
                .collect(),
            revision: context.revision,
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct DisplayPreferences {
    pub color_scheme: String,
    pub density: String,
    pub sidebar_state: String,
}

#[derive(SimpleObject)]
pub struct DisplayPreferencesNotFoundProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct DisplayPreferencesValidationProblem {
    pub code: String,
    pub message: String,
}

/// Ports the `DisplayPreferencesProblem` GraphQL interface: both implementations
/// share exactly `code`/`message`, matching the two concrete SDL types.
#[derive(Interface)]
// clippy::duplicated_attributes is a false positive here: the two `field(...)`
// entries are distinct interface fields (code, message) that both happen to be
// typed String, not a literal repeated attribute.
#[allow(clippy::duplicated_attributes)]
#[graphql(field(name = "code", ty = "String"))]
#[graphql(field(name = "message", ty = "String"))]
pub enum DisplayPreferencesProblem {
    NotFound(DisplayPreferencesNotFoundProblem),
    Validation(DisplayPreferencesValidationProblem),
}

impl From<AppDisplayPreferencesProblem> for DisplayPreferencesProblem {
    fn from(problem: AppDisplayPreferencesProblem) -> Self {
        match problem {
            AppDisplayPreferencesProblem::NotFound => {
                DisplayPreferencesProblem::NotFound(DisplayPreferencesNotFoundProblem {
                    code: "NOT_FOUND".to_string(),
                    message: "Display preferences are unavailable.".to_string(),
                })
            }
            AppDisplayPreferencesProblem::InvalidPreferences => {
                DisplayPreferencesProblem::Validation(DisplayPreferencesValidationProblem {
                    code: "INVALID_PREFERENCES".to_string(),
                    message: "Only approved display preferences can be saved.".to_string(),
                })
            }
        }
    }
}

#[derive(SimpleObject)]
pub struct DisplayPreferencesMutationPayload {
    pub display_preferences: Option<DisplayPreferences>,
    pub problems: Vec<DisplayPreferencesProblem>,
}

#[derive(InputObject)]
pub struct UpdateDisplayPreferencesInput {
    pub color_scheme: String,
    pub density: String,
    pub sidebar_state: String,
}

fn to_display_preferences(
    preferences: &hive_application::console::UserDisplayPreferences,
) -> DisplayPreferences {
    DisplayPreferences {
        color_scheme: preferences.color_scheme.clone(),
        density: preferences.density.clone(),
        sidebar_state: preferences.sidebar_state.clone(),
    }
}

fn service(ctx: &Context<'_>) -> async_graphql::Result<ConsoleContextService<PgConsoleRepository>> {
    let repository = PgConsoleRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
    Ok(ConsoleContextService::new(repository))
}

fn principal(ctx: &Context<'_>) -> async_graphql::Result<Uuid> {
    Ok(ctx.data::<RequestPrincipal>()?.0)
}

pub struct ConsoleQueries;

#[Object]
impl ConsoleQueries {
    /// Ports `ConsoleContextResolver.context`.
    async fn console_context(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<ConsoleContext>> {
        let context = service(ctx)?
            .find_context(principal(ctx)?)
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(context.map(ConsoleContext::from))
    }

    /// Ports `ConsoleContextResolver.preferences`.
    async fn display_preferences(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<DisplayPreferences>> {
        let preferences = service(ctx)?
            .find_preferences(principal(ctx)?)
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(preferences.as_ref().map(to_display_preferences))
    }
}

pub struct ConsoleMutations;

#[Object]
impl ConsoleMutations {
    /// Ports `ConsoleContextResolver.updatePreferences`.
    async fn update_display_preferences(
        &self,
        ctx: &Context<'_>,
        input: UpdateDisplayPreferencesInput,
    ) -> async_graphql::Result<DisplayPreferencesMutationPayload> {
        let result = service(ctx)?
            .update_preferences(
                principal(ctx)?,
                &input.color_scheme,
                &input.density,
                &input.sidebar_state,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;

        Ok(DisplayPreferencesMutationPayload {
            display_preferences: result.preferences.as_ref().map(to_display_preferences),
            problems: result
                .problem
                .into_iter()
                .map(DisplayPreferencesProblem::from)
                .collect(),
        })
    }
}
