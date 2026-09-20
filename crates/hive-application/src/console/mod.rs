pub mod model;
pub mod service;

pub use model::{
    DisplayPreferencesMutationResult, DisplayPreferencesProblem, UserDisplayPreferences,
};
pub use service::{
    ConsoleContextService, ConsoleRepository, RepositoryError as ConsoleRepositoryError,
};
