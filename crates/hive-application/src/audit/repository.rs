//! Ports `AuditRepository`, `AuditUnavailableException`, and
//! `AuditDependencyUnavailableException`.

use async_trait::async_trait;
use uuid::Uuid;

use super::{AuditEvent, AuditFilter, AuditPage};

/// Collapses Java's two `RuntimeException` subclasses plus the uncaught `IllegalArgumentException`
/// paths (`AuditQueryService`'s narrowing check, `AuditFilter`'s own validation, the audit
/// cursor's decode/mismatch checks) into one Rust error type. `AuditGraphql`'s resolver matches
/// `Unavailable`/`Dependency` specially; every other case surfaces its message the same way
/// graphql-java's default exception handler surfaces an uncaught `RuntimeException`'s message.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AuditError {
    #[error("{0}")]
    InvalidRequest(String),
    /// Ports `AuditUnavailableException`: the principal has no visible audit scope. Only
    /// `find_page` (`auditEvents`) raises this; `find_event` (`auditEvent`) resolves to `None`
    /// instead, matching Java's asymmetry between the two repository methods.
    #[error("Audit history is unavailable.")]
    Unavailable,
    /// Ports `AuditDependencyUnavailableException`: a `SQLException` reached the repository.
    /// `GraphqlExecutor.dependencyUnavailable()`'s Rust port answers a `503` for this case.
    #[error("Audit history is temporarily unavailable.")]
    Dependency,
}

#[async_trait]
pub trait AuditRepository: Send + Sync {
    async fn find_page(
        &self,
        principal_id: Uuid,
        filter: &AuditFilter,
        first: i32,
        after: Option<&str>,
        include_total_count: bool,
    ) -> Result<AuditPage, AuditError>;

    async fn find_event(
        &self,
        principal_id: Uuid,
        filter: &AuditFilter,
        event_id: &str,
    ) -> Result<Option<AuditEvent>, AuditError>;
}
