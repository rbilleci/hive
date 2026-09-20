//! The display-preferences command. The console's context (principal, organizations, projects,
//! capabilities) and the stored preferences are generated reads; see
//! `hive_persistence::console` for the computed `capabilities` field.
//!
//! `DisplayPreferencesProblem` is an interface with two implementors: a `dynamic::Interface` plus
//! `Object::implement(name)` on each concrete type, and a container enum deriving
//! `CustomOutputType` whose generated `CustomUnion` impl is left unregistered so only the
//! interface claims the shared type name. `#[derive(CustomOutputType)]` on a container enum
//! resolves each variant by the *variant's own identifier* (`custom_output_type.rs`:
//! `stringify!(#variant_ident)`), so each variant is named identically to its inner type.

use crate::schema::RequestPrincipal;
use async_graphql::dynamic::{Interface, InterfaceField, TypeRef};
use hive_application::console::{
    ConsoleContextService, DisplayPreferencesProblem as AppDisplayPreferencesProblem,
};
use hive_persistence::console::PgConsoleRepository;
use seaography::{
    BuilderContext, CustomFields, CustomInputType, CustomOutputObject, CustomOutputType,
};

pub const DISPLAY_PREFERENCES_PROBLEM_INTERFACE: &str = "DisplayPreferencesProblem";

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
    pub struct DisplayPreferencesNotFoundProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DisplayPreferencesValidationProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub enum DisplayPreferencesProblem {
        DisplayPreferencesNotFoundProblem(DisplayPreferencesNotFoundProblem),
        DisplayPreferencesValidationProblem(DisplayPreferencesValidationProblem),
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DisplayPreferencesMutationPayload {
        pub displayPreferences: Option<DisplayPreferences>,
        pub problems: Vec<DisplayPreferencesProblem>,
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
        // Ports `ConsoleContextResolver.updatePreferences`.
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

    fn to_display_preferences_problem(
        problem: AppDisplayPreferencesProblem,
    ) -> DisplayPreferencesProblem {
        match problem {
            AppDisplayPreferencesProblem::NotFound => {
                DisplayPreferencesProblem::DisplayPreferencesNotFoundProblem(
                    DisplayPreferencesNotFoundProblem {
                        code: "NOT_FOUND".to_string(),
                        message: "Display preferences are unavailable.".to_string(),
                    },
                )
            }
            AppDisplayPreferencesProblem::InvalidPreferences => {
                DisplayPreferencesProblem::DisplayPreferencesValidationProblem(
                    DisplayPreferencesValidationProblem {
                        code: "INVALID_PREFERENCES".to_string(),
                        message: "Only approved display preferences can be saved.".to_string(),
                    },
                )
            }
        }
    }
}

pub use wire::{
    ConsoleMutations, DisplayPreferences, DisplayPreferencesMutationPayload,
    DisplayPreferencesNotFoundProblem, DisplayPreferencesValidationProblem,
    UpdateDisplayPreferencesInput,
};

fn context() -> &'static BuilderContext {
    crate::schema::context()
}

/// This module's `Interface`, registered directly on the `SchemaBuilder` in `mod.rs::build()`
/// (`Builder` itself has no interface vector to push onto).
pub fn interfaces() -> Vec<Interface> {
    vec![Interface::new(DISPLAY_PREFERENCES_PROBLEM_INTERFACE)
        .field(InterfaceField::new(
            "code",
            TypeRef::named_nn(TypeRef::STRING),
        ))
        .field(InterfaceField::new(
            "message",
            TypeRef::named_nn(TypeRef::STRING),
        ))]
}

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_mutation::<ConsoleMutations>();
    builder.register_custom_output::<DisplayPreferences>();
    builder.outputs.push(
        DisplayPreferencesNotFoundProblem::basic_object(context())
            .implement(DISPLAY_PREFERENCES_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        DisplayPreferencesValidationProblem::basic_object(context())
            .implement(DISPLAY_PREFERENCES_PROBLEM_INTERFACE),
    );
    builder.register_custom_output::<DisplayPreferencesMutationPayload>();
    builder.register_custom_input::<UpdateDisplayPreferencesInput>();
}

#[cfg(test)]
mod tests {
    //! Exercises the interface pattern through a real, throwaway schema: the SDL-shape comparison
    //! against the frozen contract (`evidence/2026-09-19-seaography-phase2-console/`) proves the
    //! *type declarations* line up, but not that `FieldValue::owned_any(inner).with_type(name)`
    //! actually resolves to the right concrete type at query time — this is the first of 7
    //! interfaces this port builds, so it is worth proving the mechanism executes correctly once,
    //! rather than trusting the SDL shape alone.
    use super::wire::DisplayPreferencesProblem;
    use super::*;
    use async_graphql::dynamic::{Field, FieldFuture, InputValue, Object, Schema};
    use seaography::async_graphql;
    use std::sync::LazyLock;

    static CONTEXT: LazyLock<BuilderContext> = LazyLock::new(BuilderContext::default);

    fn schema() -> Schema {
        let query = Object::new("Query").field(
            Field::new(
                "problem",
                TypeRef::named_nn(DISPLAY_PREFERENCES_PROBLEM_INTERFACE),
                |ctx| {
                    FieldFuture::new(async move {
                        let use_validation = ctx.args.try_get("useValidation")?.boolean()?;
                        let problem = if use_validation {
                            DisplayPreferencesProblem::DisplayPreferencesValidationProblem(
                                DisplayPreferencesValidationProblem {
                                    code: "INVALID_PREFERENCES".to_string(),
                                    message: "Only approved display preferences can be saved."
                                        .to_string(),
                                },
                            )
                        } else {
                            DisplayPreferencesProblem::DisplayPreferencesNotFoundProblem(
                                DisplayPreferencesNotFoundProblem {
                                    code: "NOT_FOUND".to_string(),
                                    message: "Display preferences are unavailable.".to_string(),
                                },
                            )
                        };
                        Ok(problem.gql_field_value(&CONTEXT))
                    })
                },
            )
            .argument(InputValue::new(
                "useValidation",
                TypeRef::named_nn(TypeRef::BOOLEAN),
            )),
        );
        Schema::build("Query", None, None)
            .register(query)
            .register(interfaces().remove(0))
            .register(
                DisplayPreferencesNotFoundProblem::basic_object(&CONTEXT)
                    .implement(DISPLAY_PREFERENCES_PROBLEM_INTERFACE),
            )
            .register(
                DisplayPreferencesValidationProblem::basic_object(&CONTEXT)
                    .implement(DISPLAY_PREFERENCES_PROBLEM_INTERFACE),
            )
            .finish()
            .expect("the interface smoke-test schema composes")
    }

    #[tokio::test]
    async fn interface_resolves_to_the_not_found_implementor() {
        let response = schema()
            .execute("{ problem(useValidation: false) { __typename code message } }")
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"problem": {
                "__typename": "DisplayPreferencesNotFoundProblem",
                "code": "NOT_FOUND",
                "message": "Display preferences are unavailable.",
            }})
        );
    }

    #[tokio::test]
    async fn interface_resolves_to_the_validation_implementor() {
        let response = schema()
            .execute("{ problem(useValidation: true) { __typename code message } }")
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"problem": {
                "__typename": "DisplayPreferencesValidationProblem",
                "code": "INVALID_PREFERENCES",
                "message": "Only approved display preferences can be saved.",
            }})
        );
    }

    #[tokio::test]
    async fn interface_supports_type_specific_inline_fragments() {
        let response = schema()
            .execute(
                "{ problem(useValidation: true) { ... on DisplayPreferencesValidationProblem { code } } }",
            )
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"problem": {"code": "INVALID_PREFERENCES"}})
        );
    }
}
