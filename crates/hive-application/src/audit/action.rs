//! Ports `dev.hive.domain.audit.AuditAction`: the safe business actions the
//! `audit_event_projection` view's `action` column is restricted to by construction (every
//! `V040__audit_history_projection.sql` UNION branch's `CASE` expression is exhaustive over
//! its own `WHERE ... action IN (...)` clause).

use super::ValidationError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuditAction {
    AgentCreated,
    AgentSaved,
    AgentValidated,
    AgentVersionPublished,
    ConfigurationChanged,
    DeploymentRequested,
    DeploymentCanceled,
    DeploymentApprovalRecorded,
    DeploymentApprovalRejected,
    DeploymentApproved,
    DeploymentApprovalInvalidated,
    DeploymentApprovalExpired,
    DeploymentApprovalReplayed,
    DeploymentExecutionStarted,
    DeploymentExecutionSucceeded,
    DeploymentExecutionFailed,
    DeploymentPromoted,
    DeploymentRolledBack,
    DeploymentRetried,
    DeploymentDeliveryRetried,
    DeploymentDeliveryDeadLettered,
    DeploymentLeaseReclaimed,
    EvaluationCreated,
    EvaluationUpdated,
    EvaluationValidated,
    EvaluationPublished,
    EvaluationRunStarted,
    EvaluationRunPassed,
    EvaluationRunFailed,
    EvaluationRunCanceled,
    AdministrationChanged,
}

impl AuditAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentCreated => "AGENT_CREATED",
            Self::AgentSaved => "AGENT_SAVED",
            Self::AgentValidated => "AGENT_VALIDATED",
            Self::AgentVersionPublished => "AGENT_VERSION_PUBLISHED",
            Self::ConfigurationChanged => "CONFIGURATION_CHANGED",
            Self::DeploymentRequested => "DEPLOYMENT_REQUESTED",
            Self::DeploymentCanceled => "DEPLOYMENT_CANCELED",
            Self::DeploymentApprovalRecorded => "DEPLOYMENT_APPROVAL_RECORDED",
            Self::DeploymentApprovalRejected => "DEPLOYMENT_APPROVAL_REJECTED",
            Self::DeploymentApproved => "DEPLOYMENT_APPROVED",
            Self::DeploymentApprovalInvalidated => "DEPLOYMENT_APPROVAL_INVALIDATED",
            Self::DeploymentApprovalExpired => "DEPLOYMENT_APPROVAL_EXPIRED",
            Self::DeploymentApprovalReplayed => "DEPLOYMENT_APPROVAL_REPLAYED",
            Self::DeploymentExecutionStarted => "DEPLOYMENT_EXECUTION_STARTED",
            Self::DeploymentExecutionSucceeded => "DEPLOYMENT_EXECUTION_SUCCEEDED",
            Self::DeploymentExecutionFailed => "DEPLOYMENT_EXECUTION_FAILED",
            Self::DeploymentPromoted => "DEPLOYMENT_PROMOTED",
            Self::DeploymentRolledBack => "DEPLOYMENT_ROLLED_BACK",
            Self::DeploymentRetried => "DEPLOYMENT_RETRIED",
            Self::DeploymentDeliveryRetried => "DEPLOYMENT_DELIVERY_RETRIED",
            Self::DeploymentDeliveryDeadLettered => "DEPLOYMENT_DELIVERY_DEAD_LETTERED",
            Self::DeploymentLeaseReclaimed => "DEPLOYMENT_LEASE_RECLAIMED",
            Self::EvaluationCreated => "EVALUATION_CREATED",
            Self::EvaluationUpdated => "EVALUATION_UPDATED",
            Self::EvaluationValidated => "EVALUATION_VALIDATED",
            Self::EvaluationPublished => "EVALUATION_PUBLISHED",
            Self::EvaluationRunStarted => "EVALUATION_RUN_STARTED",
            Self::EvaluationRunPassed => "EVALUATION_RUN_PASSED",
            Self::EvaluationRunFailed => "EVALUATION_RUN_FAILED",
            Self::EvaluationRunCanceled => "EVALUATION_RUN_CANCELED",
            Self::AdministrationChanged => "ADMINISTRATION_CHANGED",
        }
    }

    /// Ports `AuditAction.valueOf(String)`.
    pub fn parse(value: &str) -> Result<Self, ValidationError> {
        Ok(match value {
            "AGENT_CREATED" => Self::AgentCreated,
            "AGENT_SAVED" => Self::AgentSaved,
            "AGENT_VALIDATED" => Self::AgentValidated,
            "AGENT_VERSION_PUBLISHED" => Self::AgentVersionPublished,
            "CONFIGURATION_CHANGED" => Self::ConfigurationChanged,
            "DEPLOYMENT_REQUESTED" => Self::DeploymentRequested,
            "DEPLOYMENT_CANCELED" => Self::DeploymentCanceled,
            "DEPLOYMENT_APPROVAL_RECORDED" => Self::DeploymentApprovalRecorded,
            "DEPLOYMENT_APPROVAL_REJECTED" => Self::DeploymentApprovalRejected,
            "DEPLOYMENT_APPROVED" => Self::DeploymentApproved,
            "DEPLOYMENT_APPROVAL_INVALIDATED" => Self::DeploymentApprovalInvalidated,
            "DEPLOYMENT_APPROVAL_EXPIRED" => Self::DeploymentApprovalExpired,
            "DEPLOYMENT_APPROVAL_REPLAYED" => Self::DeploymentApprovalReplayed,
            "DEPLOYMENT_EXECUTION_STARTED" => Self::DeploymentExecutionStarted,
            "DEPLOYMENT_EXECUTION_SUCCEEDED" => Self::DeploymentExecutionSucceeded,
            "DEPLOYMENT_EXECUTION_FAILED" => Self::DeploymentExecutionFailed,
            "DEPLOYMENT_PROMOTED" => Self::DeploymentPromoted,
            "DEPLOYMENT_ROLLED_BACK" => Self::DeploymentRolledBack,
            "DEPLOYMENT_RETRIED" => Self::DeploymentRetried,
            "DEPLOYMENT_DELIVERY_RETRIED" => Self::DeploymentDeliveryRetried,
            "DEPLOYMENT_DELIVERY_DEAD_LETTERED" => Self::DeploymentDeliveryDeadLettered,
            "DEPLOYMENT_LEASE_RECLAIMED" => Self::DeploymentLeaseReclaimed,
            "EVALUATION_CREATED" => Self::EvaluationCreated,
            "EVALUATION_UPDATED" => Self::EvaluationUpdated,
            "EVALUATION_VALIDATED" => Self::EvaluationValidated,
            "EVALUATION_PUBLISHED" => Self::EvaluationPublished,
            "EVALUATION_RUN_STARTED" => Self::EvaluationRunStarted,
            "EVALUATION_RUN_PASSED" => Self::EvaluationRunPassed,
            "EVALUATION_RUN_FAILED" => Self::EvaluationRunFailed,
            "EVALUATION_RUN_CANCELED" => Self::EvaluationRunCanceled,
            "ADMINISTRATION_CHANGED" => Self::AdministrationChanged,
            other => {
                return Err(ValidationError::new(format!(
                    "`{other}` is not a supported audit action."
                )))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_round_trips_through_as_str_and_parse() {
        let variants = [
            AuditAction::AgentCreated,
            AuditAction::AgentSaved,
            AuditAction::AgentValidated,
            AuditAction::AgentVersionPublished,
            AuditAction::ConfigurationChanged,
            AuditAction::DeploymentRequested,
            AuditAction::DeploymentCanceled,
            AuditAction::DeploymentApprovalRecorded,
            AuditAction::DeploymentApprovalRejected,
            AuditAction::DeploymentApproved,
            AuditAction::DeploymentApprovalInvalidated,
            AuditAction::DeploymentApprovalExpired,
            AuditAction::DeploymentApprovalReplayed,
            AuditAction::DeploymentExecutionStarted,
            AuditAction::DeploymentExecutionSucceeded,
            AuditAction::DeploymentExecutionFailed,
            AuditAction::DeploymentPromoted,
            AuditAction::DeploymentRolledBack,
            AuditAction::DeploymentRetried,
            AuditAction::DeploymentDeliveryRetried,
            AuditAction::DeploymentDeliveryDeadLettered,
            AuditAction::DeploymentLeaseReclaimed,
            AuditAction::EvaluationCreated,
            AuditAction::EvaluationUpdated,
            AuditAction::EvaluationValidated,
            AuditAction::EvaluationPublished,
            AuditAction::EvaluationRunStarted,
            AuditAction::EvaluationRunPassed,
            AuditAction::EvaluationRunFailed,
            AuditAction::EvaluationRunCanceled,
            AuditAction::AdministrationChanged,
        ];
        assert_eq!(variants.len(), 31);
        for variant in variants {
            assert_eq!(AuditAction::parse(variant.as_str()).unwrap(), variant);
        }
    }

    #[test]
    fn parse_rejects_an_unsupported_value() {
        assert!(AuditAction::parse("NOT_A_REAL_ACTION").is_err());
    }
}
