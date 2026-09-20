//! Ports `AuditQueryService`.

use uuid::Uuid;

use super::repository::{AuditError, AuditRepository};
use super::{AuditEvent, AuditFilter, AuditPage};

pub struct AuditQueryService<R: AuditRepository> {
    repository: R,
}

impl<R: AuditRepository> AuditQueryService<R> {
    pub const MAX_PAGE_SIZE: i32 = 50;

    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    /// Java's `principalId == null` check is dropped: this port's `principal_id` is always a
    /// verified `Uuid`, never absent, by the time a GraphQL resolver reaches this call.
    pub async fn events(
        &self,
        principal_id: Uuid,
        filter: &AuditFilter,
        requested_first: i32,
        after: Option<&str>,
        include_total_count: bool,
    ) -> Result<AuditPage, AuditError> {
        if !filter.has_meaningful_narrowing() {
            return Err(AuditError::InvalidRequest(
                "An audit query requires a narrowing filter.".to_string(),
            ));
        }
        let first = if requested_first <= 0 {
            25
        } else {
            requested_first.min(Self::MAX_PAGE_SIZE)
        };
        self.repository
            .find_page(principal_id, filter, first, after, include_total_count)
            .await
    }

    pub async fn event(
        &self,
        principal_id: Uuid,
        filter: &AuditFilter,
        event_id: &str,
    ) -> Result<Option<AuditEvent>, AuditError> {
        if event_id.trim().is_empty() {
            return Ok(None);
        }
        self.repository
            .find_event(principal_id, filter, event_id)
            .await
    }
}
