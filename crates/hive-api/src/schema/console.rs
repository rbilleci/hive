//! The display-preferences command. The console's context (principal, organizations, projects,
//! capabilities) and the stored preferences are generated reads; see
//! `hive_persistence::console` for the computed `capabilities` field.
//!
//! The command lists its refusals with the shared `Problem` type (`schema/problem.rs`), whose
//! `code` is the stable, machine-readable reason. `DisplayPreferencesProblem` was the last
//! hand-built interface in this schema; its two concrete types are gone and the codes
//! (`NOT_FOUND`, `INVALID_PREFERENCES`) and messages are unchanged.

use crate::schema::problem::Problem;
use crate::schema::RequestPrincipal;
use hive_application::console::{
    ConsoleContextService, DisplayPreferencesProblem as AppDisplayPreferencesProblem,
};
use hive_persistence::console::PgConsoleRepository;
use seaography::{CustomFields, CustomInputType, CustomOutputType};

#[allow(non_snake_case)]
mod wire {
    use super::*;

    #[derive(CustomOutputType, Clone)]
    pub struct DisplayPreferences {
        pub colorScheme: String,
        pub density: String,
        pub sidebarState: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DisplayPreferencesMutationPayload {
        pub displayPreferences: Option<DisplayPreferences>,
        pub problems: Vec<Problem>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "UpdateDisplayPreferencesInput")]
    pub struct UpdateDisplayPreferencesInput {
        pub colorScheme: String,
        pub density: String,
        pub sidebarState: String,
    }

    pub struct ConsoleMutations;

    #[CustomFields]
    impl ConsoleMutations {
        async fn updateDisplayPreferences(
            ctx: &async_graphql::Context<'_>,
            input: UpdateDisplayPreferencesInput,
        ) -> async_graphql::Result<DisplayPreferencesMutationPayload> {
            let principal = ctx.data::<RequestPrincipal>()?;
            let repository =
                PgConsoleRepository::new(ctx.data::<sea_orm::DatabaseConnection>()?.clone());
            let service = ConsoleContextService::new(repository);
            let result = service
                .update_preferences(
                    principal.0,
                    &input.colorScheme,
                    &input.density,
                    &input.sidebarState,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;

            Ok(DisplayPreferencesMutationPayload {
                displayPreferences: result.preferences.as_ref().map(to_display_preferences),
                problems: result
                    .problem
                    .into_iter()
                    .map(to_display_preferences_problem)
                    .collect(),
            })
        }
    }

    fn to_display_preferences(
        preferences: &hive_application::console::UserDisplayPreferences,
    ) -> DisplayPreferences {
        DisplayPreferences {
            colorScheme: preferences.color_scheme.clone(),
            density: preferences.density.clone(),
            sidebarState: preferences.sidebar_state.clone(),
        }
    }

    fn to_display_preferences_problem(problem: AppDisplayPreferencesProblem) -> Problem {
        match problem {
            AppDisplayPreferencesProblem::NotFound => {
                Problem::new("NOT_FOUND", "Display preferences are unavailable.")
            }
            AppDisplayPreferencesProblem::InvalidPreferences => Problem::new(
                "INVALID_PREFERENCES",
                "Only approved display preferences can be saved.",
            ),
        }
    }
}

pub use wire::{
    ConsoleMutations, DisplayPreferences, DisplayPreferencesMutationPayload,
    UpdateDisplayPreferencesInput,
};

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_mutation::<ConsoleMutations>();
    builder.register_custom_output::<DisplayPreferences>();
    builder.register_custom_output::<DisplayPreferencesMutationPayload>();
    builder.register_custom_input::<UpdateDisplayPreferencesInput>();
}
