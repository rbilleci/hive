use crate::console::model::{DisplayPreferencesMutationResult, DisplayPreferencesProblem};
use async_trait::async_trait;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The display-preferences write. The console reads its context and preferences through the
/// generated API.
#[async_trait]
pub trait ConsoleRepository: Send + Sync {
    async fn update_preferences(
        &self,
        principal_id: Uuid,
        color_scheme: &str,
        density: &str,
        sidebar_state: &str,
    ) -> Result<DisplayPreferencesMutationResult, RepositoryError>;
}

const COLOR_SCHEMES: &[&str] = &["SYSTEM", "LIGHT", "DARK"];
const DENSITIES: &[&str] = &["COMFORTABLE", "COMPACT"];
const SIDEBAR_STATES: &[&str] = &["EXPANDED", "COLLAPSED"];

/// Application boundary for the deliberately narrow display-preferences write.
pub struct ConsoleContextService<R: ConsoleRepository> {
    repository: R,
}

impl<R: ConsoleRepository> ConsoleContextService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub async fn update_preferences(
        &self,
        principal_id: Uuid,
        color_scheme: &str,
        density: &str,
        sidebar_state: &str,
    ) -> Result<DisplayPreferencesMutationResult, RepositoryError> {
        if !COLOR_SCHEMES.contains(&color_scheme)
            || !DENSITIES.contains(&density)
            || !SIDEBAR_STATES.contains(&sidebar_state)
        {
            return Ok(DisplayPreferencesMutationResult::refused(
                DisplayPreferencesProblem::InvalidPreferences,
            ));
        }
        self.repository
            .update_preferences(principal_id, color_scheme, density, sidebar_state)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::model::UserDisplayPreferences;

    struct RecordingRepository;

    #[async_trait]
    impl ConsoleRepository for RecordingRepository {
        async fn update_preferences(
            &self,
            principal_id: Uuid,
            color_scheme: &str,
            density: &str,
            sidebar_state: &str,
        ) -> Result<DisplayPreferencesMutationResult, RepositoryError> {
            Ok(DisplayPreferencesMutationResult::success(
                UserDisplayPreferences {
                    principal_id,
                    color_scheme: color_scheme.to_string(),
                    density: density.to_string(),
                    sidebar_state: sidebar_state.to_string(),
                },
            ))
        }
    }

    #[tokio::test]
    async fn an_unrecognized_color_scheme_is_refused_before_reaching_the_repository() {
        let service = ConsoleContextService::new(RecordingRepository);
        let result = service
            .update_preferences(Uuid::new_v4(), "NEON", "COMFORTABLE", "EXPANDED")
            .await
            .unwrap();
        assert_eq!(
            result.problem,
            Some(DisplayPreferencesProblem::InvalidPreferences)
        );
    }

    #[tokio::test]
    async fn an_unrecognized_density_is_refused() {
        let service = ConsoleContextService::new(RecordingRepository);
        let result = service
            .update_preferences(Uuid::new_v4(), "LIGHT", "ROOMY", "EXPANDED")
            .await
            .unwrap();
        assert_eq!(
            result.problem,
            Some(DisplayPreferencesProblem::InvalidPreferences)
        );
    }

    #[tokio::test]
    async fn an_unrecognized_sidebar_state_is_refused() {
        let service = ConsoleContextService::new(RecordingRepository);
        let result = service
            .update_preferences(Uuid::new_v4(), "LIGHT", "COMFORTABLE", "HIDDEN")
            .await
            .unwrap();
        assert_eq!(
            result.problem,
            Some(DisplayPreferencesProblem::InvalidPreferences)
        );
    }

    #[tokio::test]
    async fn valid_values_reach_the_repository() {
        let service = ConsoleContextService::new(RecordingRepository);
        let result = service
            .update_preferences(Uuid::new_v4(), "DARK", "COMPACT", "COLLAPSED")
            .await
            .unwrap();
        assert!(result.problem.is_none());
        assert_eq!(result.preferences.unwrap().color_scheme, "DARK");
    }
}
