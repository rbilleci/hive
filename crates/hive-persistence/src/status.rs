//! Conversions between the stored column enums (`entity::enums`) and the status types
//! `hive-domain` and `hive-application` decide on.
//!
//! Each is an exhaustive match, so adding a value to either side fails compilation here. The row
//! mappers, the computed fields and both workers convert through these instead of rendering a
//! column enum to a string and parsing it back.

use crate::entity::enums::{
    ApprovalRequirementStatus as StoredRequirementStatus,
    DeploymentLifecycleStatus as StoredDeploymentStatus,
    EvaluationLifecycleStatus as StoredEvaluationStatus,
    EvaluationOutcomeCategory as StoredOutcomeCategory,
};
use hive_application::evaluation::{EvaluationOutcomeCategory, EvaluationRunStatus};
use hive_domain::deployment::{ApprovalRequirementStatus, DeploymentLifecycleStatus};

impl From<StoredDeploymentStatus> for DeploymentLifecycleStatus {
    fn from(value: StoredDeploymentStatus) -> Self {
        match value {
            StoredDeploymentStatus::Requested => Self::Requested,
            StoredDeploymentStatus::AwaitingApproval => Self::AwaitingApproval,
            StoredDeploymentStatus::Approved => Self::Approved,
            StoredDeploymentStatus::InProgress => Self::InProgress,
            StoredDeploymentStatus::Active => Self::Active,
            StoredDeploymentStatus::Failed => Self::Failed,
            StoredDeploymentStatus::Canceled => Self::Canceled,
            StoredDeploymentStatus::RolledBack => Self::RolledBack,
        }
    }
}

impl From<StoredRequirementStatus> for ApprovalRequirementStatus {
    fn from(value: StoredRequirementStatus) -> Self {
        match value {
            StoredRequirementStatus::Pending => Self::Pending,
            StoredRequirementStatus::Satisfied => Self::Satisfied,
            StoredRequirementStatus::Rejected => Self::Rejected,
            StoredRequirementStatus::Expired => Self::Expired,
            StoredRequirementStatus::Invalidated => Self::Invalidated,
        }
    }
}

impl From<StoredEvaluationStatus> for EvaluationRunStatus {
    fn from(value: StoredEvaluationStatus) -> Self {
        match value {
            StoredEvaluationStatus::Queued => Self::Queued,
            StoredEvaluationStatus::Running => Self::Running,
            StoredEvaluationStatus::Completed => Self::Completed,
            StoredEvaluationStatus::Failed => Self::Failed,
            StoredEvaluationStatus::Canceled => Self::Canceled,
        }
    }
}

impl From<StoredOutcomeCategory> for EvaluationOutcomeCategory {
    fn from(value: StoredOutcomeCategory) -> Self {
        match value {
            StoredOutcomeCategory::Passed => Self::Passed,
            StoredOutcomeCategory::CaseFailed => Self::CaseFailed,
            StoredOutcomeCategory::TargetFailed => Self::TargetFailed,
            StoredOutcomeCategory::RunnerFailed => Self::RunnerFailed,
            StoredOutcomeCategory::Canceled => Self::Canceled,
        }
    }
}

impl From<EvaluationOutcomeCategory> for StoredOutcomeCategory {
    fn from(value: EvaluationOutcomeCategory) -> Self {
        match value {
            EvaluationOutcomeCategory::Passed => Self::Passed,
            EvaluationOutcomeCategory::CaseFailed => Self::CaseFailed,
            EvaluationOutcomeCategory::TargetFailed => Self::TargetFailed,
            EvaluationOutcomeCategory::RunnerFailed => Self::RunnerFailed,
            EvaluationOutcomeCategory::Canceled => Self::Canceled,
        }
    }
}
