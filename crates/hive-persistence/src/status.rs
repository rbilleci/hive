//! Conversions between the stored column enums (`entity::enums`) and the status types
//! `hive-application` decides on.
//!
//! Each is an exhaustive match, so adding a value to either side fails compilation here. The row
//! mappers, the computed fields and both workers convert through these instead of rendering a
//! column enum to a string and parsing it back.

use crate::entity::enums::{
    ApprovalRequirementStatus as StoredRequirementStatus,
    DeploymentAttemptStatus as StoredAttemptStatus,
    DeploymentLifecycleStatus as StoredDeploymentStatus, DeploymentRisk as StoredRisk,
    DeploymentStrategy as StoredStrategy, EvaluationLifecycleStatus as StoredEvaluationStatus,
    EvaluationOutcomeCategory as StoredOutcomeCategory,
};
use hive_application::deployment::{
    ApprovalRequirementStatus, DeploymentAttemptStatus, DeploymentLifecycleStatus,
    DeploymentRiskLevel, DeploymentStrategy,
};
use hive_application::evaluation::{EvaluationOutcomeCategory, EvaluationRunStatus};

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

impl From<StoredStrategy> for DeploymentStrategy {
    fn from(value: StoredStrategy) -> Self {
        match value {
            StoredStrategy::Replace => Self::Replace,
            StoredStrategy::Rolling => Self::Rolling,
            StoredStrategy::Canary => Self::Canary,
            StoredStrategy::BlueGreen => Self::BlueGreen,
        }
    }
}

impl From<DeploymentStrategy> for StoredStrategy {
    fn from(value: DeploymentStrategy) -> Self {
        match value {
            DeploymentStrategy::Replace => Self::Replace,
            DeploymentStrategy::Rolling => Self::Rolling,
            DeploymentStrategy::Canary => Self::Canary,
            DeploymentStrategy::BlueGreen => Self::BlueGreen,
        }
    }
}

impl From<StoredRisk> for DeploymentRiskLevel {
    fn from(value: StoredRisk) -> Self {
        match value {
            StoredRisk::Low => Self::Low,
            StoredRisk::Medium => Self::Medium,
            StoredRisk::High => Self::High,
        }
    }
}

impl From<DeploymentRiskLevel> for StoredRisk {
    fn from(value: DeploymentRiskLevel) -> Self {
        match value {
            DeploymentRiskLevel::Low => Self::Low,
            DeploymentRiskLevel::Medium => Self::Medium,
            DeploymentRiskLevel::High => Self::High,
        }
    }
}

impl From<StoredAttemptStatus> for DeploymentAttemptStatus {
    fn from(value: StoredAttemptStatus) -> Self {
        match value {
            StoredAttemptStatus::Queued => Self::Queued,
            StoredAttemptStatus::Running => Self::Running,
            StoredAttemptStatus::Succeeded => Self::Succeeded,
            StoredAttemptStatus::Failed => Self::Failed,
            StoredAttemptStatus::Canceled => Self::Canceled,
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
