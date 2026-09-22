//! The one place a `DbErr` becomes the `RepositoryError` the application boundary answers with.

use hive_application::RepositoryError;
use sea_orm::DbErr;

/// Classifies a statement failure for the transport above. A serialization failure or a unique
/// violation that reaches here is one the command did not expect and did not turn into a typed
/// refusal, but it is still the caller's to retry, so it must not be reported as a dependency
/// failure.
pub fn repository_error(error: DbErr) -> RepositoryError {
    if crate::retry::is_serialization_failure_db(&error)
        || crate::retry::is_unique_violation_db(&error)
    {
        return RepositoryError::Conflict(error.to_string());
    }
    match error {
        DbErr::RecordNotFound(message) => RepositoryError::NotFound(message),
        other => RepositoryError::Unavailable(other.to_string()),
    }
}
