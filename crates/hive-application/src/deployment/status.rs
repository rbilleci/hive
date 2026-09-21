//! The deployment domain's closed value sets: the two lifecycle state machines, the strategy, the
//! risk level, the attempt status, and the two evidence vocabularies.
//!
//! Persistence converts each stored column enum into the type here once, at the row-mapping
//! boundary (`hive_persistence::status`), and renders `as_str` once at the GraphQL edge. Nothing
//! between the two holds one of these values as a `String`, so every comparison names a variant and
//! every predicate is an exhaustive `match` that a new variant forces a caller to classify.

use std::str::FromStr;

/// The `deployments.lifecycle_status` state machine.
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

impl std::fmt::Display for DeploymentLifecycleStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `deployment_approval_requirements.status`.
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

/// `deployments.strategy`. The only value set here a caller names in free text: it arrives as a
/// GraphQL argument, so `FromStr` is the one boundary that turns text into a strategy and the
/// compiler refuses a request whose strategy does not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeploymentStrategy {
    Replace,
    Rolling,
    BlueGreen,
    Canary,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unrecognized deployment strategy `{0}`")]
pub struct UnknownDeploymentStrategy(pub String);

impl DeploymentStrategy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "REPLACE",
            Self::Rolling => "ROLLING",
            Self::BlueGreen => "BLUE_GREEN",
            Self::Canary => "CANARY",
        }
    }
}

impl FromStr for DeploymentStrategy {
    type Err = UnknownDeploymentStrategy;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "REPLACE" => Ok(Self::Replace),
            "ROLLING" => Ok(Self::Rolling),
            "BLUE_GREEN" => Ok(Self::BlueGreen),
            "CANARY" => Ok(Self::Canary),
            other => Err(UnknownDeploymentStrategy(other.to_string())),
        }
    }
}

impl std::fmt::Display for DeploymentStrategy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `deployment_policy_snapshots.risk`, the P-05 risk the compiler derives and freezes. It is half
/// of the policy-matrix cell key, so its rendering is load-bearing beyond the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeploymentRiskLevel {
    Low,
    Medium,
    High,
}

impl DeploymentRiskLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        }
    }
}

impl std::fmt::Display for DeploymentRiskLevel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `deployment_attempts.status`, one execution attempt's own outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeploymentAttemptStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
}

impl DeploymentAttemptStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "QUEUED",
            Self::Running => "RUNNING",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
            Self::Canceled => "CANCELED",
        }
    }
}

impl std::fmt::Display for DeploymentAttemptStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The state of one evidence snapshot against the deployment's frozen policy. No column holds it:
/// persistence decides it from the snapshot, its recorded invalidations and the clock, and
/// `Missing` names a required kind that has no snapshot at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalEvidenceState {
    Valid,
    Missing,
    Expired,
    Revoked,
    Failed,
    Mismatch,
}

impl ApprovalEvidenceState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "VALID",
            Self::Missing => "MISSING",
            Self::Expired => "EXPIRED",
            Self::Revoked => "REVOKED",
            Self::Failed => "FAILED",
            Self::Mismatch => "MISMATCH",
        }
    }
}

impl std::fmt::Display for ApprovalEvidenceState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a frozen approval cycle's required evidence does not hold. It is both the refusal code the
/// decision planner reports and the `invalidation_code` a reconciliation writes, so `as_str` is the
/// wire spelling of a `Problem.code` and of the stored column alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalEvidenceIssue {
    /// A required kind has no snapshot.
    Missing,
    /// A snapshot exists for every required kind, and one of them is past its expiry.
    Expired,
    /// A snapshot exists and has not expired, but it was frozen against a different cycle.
    Mismatch,
}

impl ApprovalEvidenceIssue {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "APPROVAL_EVIDENCE_MISSING",
            Self::Expired => "APPROVAL_EVIDENCE_EXPIRED",
            Self::Mismatch => "APPROVAL_EVIDENCE_MISMATCH",
        }
    }
}

impl std::fmt::Display for ApprovalEvidenceIssue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod lifecycle_status_tests {
    use super::DeploymentLifecycleStatus as Status;
    use super::{
        ApprovalEvidenceIssue, ApprovalEvidenceState, DeploymentAttemptStatus, DeploymentRiskLevel,
        DeploymentStrategy,
    };

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
    fn every_strategy_round_trips_through_its_column_value() {
        for strategy in [
            DeploymentStrategy::Replace,
            DeploymentStrategy::Rolling,
            DeploymentStrategy::BlueGreen,
            DeploymentStrategy::Canary,
        ] {
            assert_eq!(
                strategy.as_str().parse::<DeploymentStrategy>(),
                Ok(strategy)
            );
        }
        assert!("RECREATE".parse::<DeploymentStrategy>().is_err());
        assert!("replace".parse::<DeploymentStrategy>().is_err());
    }

    /// The wire spelling of every value set that has no parse back: a rename here is a wire change.
    #[test]
    fn the_write_only_value_sets_spell_their_wire_vocabulary() {
        assert_eq!(
            [
                DeploymentRiskLevel::Low,
                DeploymentRiskLevel::Medium,
                DeploymentRiskLevel::High
            ]
            .map(DeploymentRiskLevel::as_str),
            ["LOW", "MEDIUM", "HIGH"]
        );
        assert_eq!(
            [
                DeploymentAttemptStatus::Queued,
                DeploymentAttemptStatus::Running,
                DeploymentAttemptStatus::Succeeded,
                DeploymentAttemptStatus::Failed,
                DeploymentAttemptStatus::Canceled,
            ]
            .map(DeploymentAttemptStatus::as_str),
            ["QUEUED", "RUNNING", "SUCCEEDED", "FAILED", "CANCELED"]
        );
        assert_eq!(
            [
                ApprovalEvidenceState::Valid,
                ApprovalEvidenceState::Missing,
                ApprovalEvidenceState::Expired,
                ApprovalEvidenceState::Revoked,
                ApprovalEvidenceState::Failed,
                ApprovalEvidenceState::Mismatch,
            ]
            .map(ApprovalEvidenceState::as_str),
            ["VALID", "MISSING", "EXPIRED", "REVOKED", "FAILED", "MISMATCH"]
        );
        assert_eq!(
            [
                ApprovalEvidenceIssue::Missing,
                ApprovalEvidenceIssue::Expired,
                ApprovalEvidenceIssue::Mismatch,
            ]
            .map(ApprovalEvidenceIssue::as_str),
            [
                "APPROVAL_EVIDENCE_MISSING",
                "APPROVAL_EVIDENCE_EXPIRED",
                "APPROVAL_EVIDENCE_MISMATCH"
            ]
        );
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
