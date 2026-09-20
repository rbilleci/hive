//! Ports `dev.hive.domain.audit`/`dev.hive.application.audit`: the read-only M17 audit
//! history contract (`AuditGraphql`'s `auditEvents`/`auditEvent`). Domain and application
//! logic collapse into one module here, matching this port's `hive_application::evaluation`
//! precedent, which merges Java's domain/application package split the same way.

pub mod action;
pub mod filter;
pub mod models;
pub mod repository;
pub mod resource_reference;
pub mod service;

pub use action::AuditAction;
pub use filter::AuditFilter;
pub use models::{AuditEvent, AuditPage, AuditRequestMetadata};
pub use repository::{AuditError, AuditRepository};
pub use resource_reference::AuditResourceReference;
pub use service::AuditQueryService;

/// A pure validation/parsing failure from a domain constructor (`AuditFilter::new`,
/// `AuditResourceReference::new`, `AuditAction::parse`) that has no I/O and so can never produce
/// `AuditError`'s `Unavailable`/`Dependency` variants — a typed wrapper instead of a bare
/// `Result<_, String>`, without pulling in variants a pure constructor could never return. Every
/// current caller of these three constructors discards the message and substitutes its own fixed
/// text (matching Java's own broad `catch (RuntimeException) -> generic message` callers), so the
/// message text itself carries no behavior; only the type needed to stop being a bare `String`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ValidationError(pub(crate) String);

impl ValidationError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Ports the shared `[A-Z][A-Z0-9_]{2,80}` pattern `AuditFilter`'s `resourceType`/`outcome`
/// fields and `AuditResourceReference::new`'s `type` field all validate against.
pub(crate) fn valid_capability_word(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && (3..=81).contains(&value.len())
        && chars.all(|value| value.is_ascii_uppercase() || value.is_ascii_digit() || value == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_capability_word_accepts_the_shortest_and_longest_forms() {
        assert!(valid_capability_word("ABC"));
        assert!(valid_capability_word(&"A".repeat(81)));
        assert!(!valid_capability_word(&"A".repeat(82)));
    }

    #[test]
    fn valid_capability_word_rejects_lowercase_and_short_input() {
        assert!(!valid_capability_word("AB"));
        assert!(!valid_capability_word("abc"));
        assert!(!valid_capability_word("1BC"));
        assert!(!valid_capability_word(""));
    }
}
