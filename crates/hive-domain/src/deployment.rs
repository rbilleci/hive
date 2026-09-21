//! Locked server facts and the pure P-05 decision types the approval planner consumes.
//! `ApprovalDecisionProblem` here is deliberately distinct from `hive_application::deployment`'s
//! type of the same name, which is an application-facing refusal projection: transport callers stay
//! independent of the domain planning types.

use std::str::FromStr;
use uuid::Uuid;

/// The `deployments.lifecycle_status` state machine. Persistence parses the column into this type
/// once at the row-mapping boundary; every state predicate is an exhaustive `match`, so adding a
/// variant forces each predicate to classify it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeploymentLifecycleStatus {
    Requested,
    AwaitingApproval,
    Approved,
    InProgress,
    Active,
    Failed,
    Canceled,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unrecognized deployment lifecycle status `{0}`")]
pub struct UnknownDeploymentLifecycleStatus(pub String);

impl DeploymentLifecycleStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "REQUESTED",
            Self::AwaitingApproval => "AWAITING_APPROVAL",
            Self::Approved => "APPROVED",
            Self::InProgress => "IN_PROGRESS",
            Self::Active => "ACTIVE",
            Self::Failed => "FAILED",
            Self::Canceled => "CANCELED",
            Self::RolledBack => "ROLLED_BACK",
        }
    }

    /// No further lifecycle transition leaves this state through the outbox worker.
    pub const fn is_terminal(self) -> bool {
        match self {
            Self::Active | Self::Failed | Self::Canceled | Self::RolledBack => true,
            Self::Requested | Self::AwaitingApproval | Self::Approved | Self::InProgress => false,
        }
    }

    /// A cancel command may still claim the deployment.
    pub const fn is_cancellable(self) -> bool {
        match self {
            Self::Requested | Self::AwaitingApproval | Self::Approved | Self::InProgress => true,
            Self::Active | Self::Failed | Self::Canceled | Self::RolledBack => false,
        }
    }

    /// The outbox worker may start executing the deployment from this state.
    pub const fn awaits_execution(self) -> bool {
        match self {
            Self::Requested | Self::Approved => true,
            Self::AwaitingApproval
            | Self::InProgress
            | Self::Active
            | Self::Failed
            | Self::Canceled
            | Self::RolledBack => false,
        }
    }

    /// Execution has begun or finished, so a pending approval requirement can no longer apply.
    pub const fn has_started_execution(self) -> bool {
        match self {
            Self::InProgress | Self::Active | Self::Failed | Self::Canceled | Self::RolledBack => {
                true
            }
            Self::Requested | Self::AwaitingApproval | Self::Approved => false,
        }
    }
}

impl FromStr for DeploymentLifecycleStatus {
    type Err = UnknownDeploymentLifecycleStatus;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "REQUESTED" => Ok(Self::Requested),
            "AWAITING_APPROVAL" => Ok(Self::AwaitingApproval),
            "APPROVED" => Ok(Self::Approved),
            "IN_PROGRESS" => Ok(Self::InProgress),
            "ACTIVE" => Ok(Self::Active),
            "FAILED" => Ok(Self::Failed),
            "CANCELED" => Ok(Self::Canceled),
            "ROLLED_BACK" => Ok(Self::RolledBack),
            other => Err(UnknownDeploymentLifecycleStatus(other.to_string())),
        }
    }
}

/// `deployment_approval_requirements.status`. Persistence parses the column once at the row
/// boundary; comparisons name a variant instead of repeating its column string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalRequirementStatus {
    Pending,
    Satisfied,
    Rejected,
    Expired,
    Invalidated,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unrecognized approval requirement status `{0}`")]
pub struct UnknownApprovalRequirementStatus(pub String);

impl ApprovalRequirementStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Satisfied => "SATISFIED",
            Self::Rejected => "REJECTED",
            Self::Expired => "EXPIRED",
            Self::Invalidated => "INVALIDATED",
        }
    }
}

impl FromStr for ApprovalRequirementStatus {
    type Err = UnknownApprovalRequirementStatus;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "PENDING" => Ok(Self::Pending),
            "SATISFIED" => Ok(Self::Satisfied),
            "REJECTED" => Ok(Self::Rejected),
            "EXPIRED" => Ok(Self::Expired),
            "INVALIDATED" => Ok(Self::Invalidated),
            other => Err(UnknownApprovalRequirementStatus(other.to_string())),
        }
    }
}

impl std::fmt::Display for ApprovalRequirementStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::fmt::Display for DeploymentLifecycleStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One normalized approval decision command at the application-to-persistence boundary.
#[derive(Debug, Clone)]
pub struct ApprovalDecisionCommand {
    pub principal_id: Uuid,
    pub requirement_id: Uuid,
    pub expected_revision: i64,
    pub value: String,
    pub comment: Option<String>,
    pub rejection_reason: Option<String>,
    pub request_id: Uuid,
    pub correlation_id: Uuid,
}

/// Locked server facts that the P-05 policy evaluates without browser-provided authority.
#[derive(Debug, Clone)]
pub struct ApprovalDecisionFacts {
    pub requirement_id: Uuid,
    pub requester_id: Uuid,
    pub revision: i64,
    pub status: ApprovalRequirementStatus,
    pub invalidation_code: Option<String>,
    pub required_approvers: i32,
    pub qualifying_approvers: i32,
    pub visible: bool,
    pub eligible: bool,
    pub duplicate: bool,
    pub expired: bool,
    pub evidence_issue: Option<String>,
    pub waiting_for_evaluation: bool,
}

/// Refusal selected from immutable approval facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalDecisionProblem {
    pub code: String,
    pub resource_id: Option<Uuid>,
    pub expected_revision: i64,
    pub actual_revision: i64,
}

impl ApprovalDecisionProblem {
    pub fn unavailable() -> Self {
        Self {
            code: "NOT_FOUND".to_string(),
            resource_id: None,
            expected_revision: 0,
            actual_revision: 0,
        }
    }

    pub fn conflict(id: Uuid, expected: i64, actual: i64) -> Self {
        Self {
            code: "REVISION_CONFLICT".to_string(),
            resource_id: Some(id),
            expected_revision: expected,
            actual_revision: actual,
        }
    }

    pub fn of(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            resource_id: None,
            expected_revision: 0,
            actual_revision: 0,
        }
    }
}

/// Immutable decision outcome selected from a command and locked facts.
#[derive(Debug, Clone)]
pub struct ApprovalDecisionPlan {
    pub problem: Option<ApprovalDecisionProblem>,
    pub rejection: bool,
    pub satisfies_requirement: bool,
}

impl ApprovalDecisionPlan {
    pub fn refuse(problem: ApprovalDecisionProblem) -> Self {
        Self {
            problem: Some(problem),
            rejection: false,
            satisfies_requirement: false,
        }
    }

    pub fn record(rejection: bool, satisfies_requirement: bool) -> Self {
        Self {
            problem: None,
            rejection,
            satisfies_requirement,
        }
    }

    pub fn accepted(&self) -> bool {
        self.problem.is_none()
    }
}

#[cfg(test)]
mod lifecycle_status_tests {
    use super::DeploymentLifecycleStatus as Status;

    const ALL: [Status; 8] = [
        Status::Requested,
        Status::AwaitingApproval,
        Status::Approved,
        Status::InProgress,
        Status::Active,
        Status::Failed,
        Status::Canceled,
        Status::RolledBack,
    ];

    #[test]
    fn every_variant_round_trips_through_its_column_value() {
        for status in ALL {
            assert_eq!(status.as_str().parse::<Status>(), Ok(status));
        }
        assert!("PAUSED".parse::<Status>().is_err());
    }

    #[test]
    fn every_approval_requirement_status_round_trips_through_its_column_value() {
        use super::ApprovalRequirementStatus as Requirement;
        for status in [
            Requirement::Pending,
            Requirement::Satisfied,
            Requirement::Rejected,
            Requirement::Expired,
            Requirement::Invalidated,
        ] {
            assert_eq!(status.as_str().parse::<Requirement>(), Ok(status));
        }
        assert!("WAIVED".parse::<Requirement>().is_err());
    }

    #[test]
    fn predicates_match_the_literal_sets_they_replaced() {
        let named = |predicate: fn(Status) -> bool| -> Vec<&'static str> {
            ALL.into_iter()
                .filter(|status| predicate(*status))
                .map(Status::as_str)
                .collect()
        };
        assert_eq!(
            named(Status::is_terminal),
            ["ACTIVE", "FAILED", "CANCELED", "ROLLED_BACK"]
        );
        assert_eq!(
            named(Status::is_cancellable),
            ["REQUESTED", "AWAITING_APPROVAL", "APPROVED", "IN_PROGRESS"]
        );
        assert_eq!(named(Status::awaits_execution), ["REQUESTED", "APPROVED"]);
        assert_eq!(
            named(Status::has_started_execution),
            ["IN_PROGRESS", "ACTIVE", "FAILED", "CANCELED", "ROLLED_BACK"]
        );
    }
}
