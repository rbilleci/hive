//! Ports `dev.hive.domain.audit.AuditResourceReference`.

use uuid::Uuid;

use super::ValidationError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditResourceReference {
    pub kind: String,
    pub id: Uuid,
}

impl AuditResourceReference {
    /// Java's constructor also rejects a `null` id; `Uuid` is never null in this port, matching
    /// this codebase's established convention of dropping unreachable null checks.
    pub fn new(kind: impl Into<String>, id: Uuid) -> Result<Self, ValidationError> {
        let kind = kind.into();
        if !super::valid_capability_word(&kind) {
            return Err(ValidationError::new(
                "An audit resource reference requires a supported type and identifier.",
            ));
        }
        Ok(Self { kind, id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_lowercase_type() {
        assert!(AuditResourceReference::new("agent", Uuid::new_v4()).is_err());
    }

    #[test]
    fn accepts_a_supported_type() {
        let id = Uuid::new_v4();
        let reference = AuditResourceReference::new("AGENT_VERSION", id).unwrap();
        assert_eq!(reference.kind, "AGENT_VERSION");
        assert_eq!(reference.id, id);
    }
}
