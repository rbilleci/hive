//! The persistence boundary the console's one command is written against.

use crate::console::models::DisplayPreferencesMutationResult;
use crate::RepositoryError;
use async_trait::async_trait;
use uuid::Uuid;

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
