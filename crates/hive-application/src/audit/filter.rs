//! Ports `dev.hive.domain.audit.AuditFilter`.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{AuditAction, ValidationError};

#[derive(Debug, Clone)]
pub struct AuditFilter {
    pub organization_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub event_id: Option<String>,
    pub correlation_id: Option<Uuid>,
    pub actor_id: Option<Uuid>,
    pub action: Option<AuditAction>,
    pub outcome: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<Uuid>,
    pub occurred_after: Option<DateTime<Utc>>,
    pub occurred_before: Option<DateTime<Utc>>,
}

impl AuditFilter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        organization_id: Option<Uuid>,
        project_id: Option<Uuid>,
        event_id: Option<String>,
        correlation_id: Option<Uuid>,
        actor_id: Option<Uuid>,
        action: Option<AuditAction>,
        outcome: Option<String>,
        resource_type: Option<String>,
        resource_id: Option<Uuid>,
        occurred_after: Option<DateTime<Utc>>,
        occurred_before: Option<DateTime<Utc>>,
    ) -> Result<Self, ValidationError> {
        if organization_id.is_none() == project_id.is_none() {
            return Err(ValidationError::new("Specify exactly one audit scope."));
        }
        if resource_type.is_none() != resource_id.is_none() {
            return Err(ValidationError::new(
                "An audit resource filter requires both type and identifier.",
            ));
        }
        if let Some(value) = &resource_type {
            if !super::valid_capability_word(value) {
                return Err(ValidationError::new("The audit resource type is invalid."));
            }
        }
        if let Some(value) = &outcome {
            if !super::valid_capability_word(value) {
                return Err(ValidationError::new("The audit outcome is invalid."));
            }
        }
        if let (Some(after), Some(before)) = (occurred_after, occurred_before) {
            if after > before {
                return Err(ValidationError::new("The audit time range is invalid."));
            }
        }
        Ok(Self {
            organization_id,
            project_id,
            event_id,
            correlation_id,
            actor_id,
            action,
            outcome,
            resource_type,
            resource_id,
            occurred_after,
            occurred_before,
        })
    }

    pub fn has_meaningful_narrowing(&self) -> bool {
        self.event_id.is_some()
            || self.correlation_id.is_some()
            || self.actor_id.is_some()
            || self.action.is_some()
            || self.outcome.is_some()
            || self.resource_id.is_some()
            || self.occurred_after.is_some()
            || self.occurred_before.is_some()
    }

    pub fn scope_type(&self) -> &'static str {
        if self.organization_id.is_none() {
            "PROJECT"
        } else {
            "ORGANIZATION"
        }
    }

    /// Guaranteed by the exactly-one-of check `new` enforces.
    pub fn scope_id(&self) -> Uuid {
        self.organization_id
            .or(self.project_id)
            .expect("AuditFilter::new guarantees exactly one of organization_id/project_id is set")
    }

    /// Builds the `eventId`-overridden filter `findEvent` queries with (Java's local `byEvent`).
    pub fn with_event_id(&self, event_id: impl Into<String>) -> Self {
        Self {
            organization_id: self.organization_id,
            project_id: self.project_id,
            event_id: Some(event_id.into()),
            correlation_id: self.correlation_id,
            actor_id: self.actor_id,
            action: self.action,
            outcome: self.outcome.clone(),
            resource_type: self.resource_type.clone(),
            resource_id: self.resource_id,
            occurred_after: self.occurred_after,
            occurred_before: self.occurred_before,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Uuid {
        Uuid::new_v4()
    }

    #[test]
    fn requires_exactly_one_scope() {
        assert!(
            AuditFilter::new(None, None, None, None, None, None, None, None, None, None, None)
                .is_err()
        );
        assert!(AuditFilter::new(
            Some(Uuid::new_v4()),
            Some(Uuid::new_v4()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None
        )
        .is_err());
    }

    #[test]
    fn requires_resource_type_and_id_together() {
        assert!(AuditFilter::new(
            None,
            Some(project()),
            None,
            None,
            None,
            None,
            None,
            Some("AGENT".to_string()),
            None,
            None,
            None
        )
        .is_err());
    }

    #[test]
    fn rejects_a_reversed_time_range() {
        let now = Utc::now();
        let earlier = now - chrono::Duration::hours(1);
        assert!(AuditFilter::new(
            None,
            Some(project()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(now),
            Some(earlier)
        )
        .is_err());
    }

    #[test]
    fn has_meaningful_narrowing_requires_at_least_one_narrowing_field() {
        let filter = AuditFilter::new(
            None,
            Some(project()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(!filter.has_meaningful_narrowing());
        let narrowed = filter.with_event_id("deployment:00000000-0000-0000-0000-000000000001");
        assert!(narrowed.has_meaningful_narrowing());
    }

    #[test]
    fn scope_type_and_id_reflect_the_set_field() {
        let organization = Uuid::new_v4();
        let filter = AuditFilter::new(
            Some(organization),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(filter.scope_type(), "ORGANIZATION");
        assert_eq!(filter.scope_id(), organization);
    }
}
