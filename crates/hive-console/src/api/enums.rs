//! The schema's enums as Rust enums. cynic checks every variant against `schema/hive.graphql`, so a
//! value the server adds or removes fails the build instead of reaching a page as unknown text.
//! `as_str` is the GraphQL spelling, which is also what the pages display.

use crate::graphql::schema;

macro_rules! wire_enum {
    ($name:ident { $($variant:ident = $wire:literal),+ $(,)? }) => {
        #[derive(cynic::Enum, Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $(#[cynic(rename = $wire)] $variant),+ }

        impl $name {
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $wire),+ } }
            /// The variant spelled `wire`, for a value that arrives from a form control.
            #[allow(dead_code)] // only the enums a form control edits use it
            pub fn from_wire(wire: &str) -> Option<Self> { match wire { $($wire => Some(Self::$variant),)+ _ => None } }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { formatter.write_str(self.as_str()) }
        }
    };
}

wire_enum!(ApprovalDecisionValue { Approve = "APPROVE", Reject = "REJECT" });
wire_enum!(ApprovalEvidenceKind { PlanValidated = "PLAN_VALIDATED", ChangeSummaryReady = "CHANGE_SUMMARY_READY", EvaluationPassed = "EVALUATION_PASSED" });
wire_enum!(ApprovalEvidenceState { Valid = "VALID", Missing = "MISSING", Expired = "EXPIRED", Revoked = "REVOKED", Failed = "FAILED", Mismatch = "MISMATCH" });
wire_enum!(ApprovalRequirementStatus { Pending = "PENDING", Satisfied = "SATISFIED", Rejected = "REJECTED", Expired = "EXPIRED", Invalidated = "INVALIDATED" });
wire_enum!(DeploymentAttemptStatus { Queued = "QUEUED", Running = "RUNNING", Succeeded = "SUCCEEDED", Failed = "FAILED", Canceled = "CANCELED" });
wire_enum!(DeploymentLifecycleStatus { Requested = "REQUESTED", AwaitingApproval = "AWAITING_APPROVAL", Approved = "APPROVED", InProgress = "IN_PROGRESS", Active = "ACTIVE", Failed = "FAILED", Canceled = "CANCELED", RolledBack = "ROLLED_BACK" });
wire_enum!(DeploymentRiskLevel { Low = "LOW", Medium = "MEDIUM", High = "HIGH" });
wire_enum!(DeploymentRuntimeHealthStatus { NotObserved = "NOT_OBSERVED", Starting = "STARTING", Healthy = "HEALTHY", Unhealthy = "UNHEALTHY", Canceled = "CANCELED" });
wire_enum!(DeploymentStrategy { Replace = "REPLACE", Rolling = "ROLLING", BlueGreen = "BLUE_GREEN", Canary = "CANARY" });
wire_enum!(EvaluationOutcomeCategory { Passed = "PASSED", CaseFailed = "CASE_FAILED", TargetFailed = "TARGET_FAILED", RunnerFailed = "RUNNER_FAILED", Canceled = "CANCELED" });
wire_enum!(EvaluationRunStatus { Queued = "QUEUED", Running = "RUNNING", Completed = "COMPLETED", Failed = "FAILED", Canceled = "CANCELED" });
wire_enum!(EvaluationTargetKind { AgentVersion = "AGENT_VERSION", Deployment = "DEPLOYMENT" });
wire_enum!(LogicalEnvironmentClass { Development = "DEVELOPMENT", Staging = "STAGING", Production = "PRODUCTION" });
