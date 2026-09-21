//! The application-owned, non-substitutable P-05 plan selector for one locked decision crossing,
//! with the command it answers, the locked server facts it reads and the plan it returns. Pure —
//! no I/O, and no fact here is browser-provided.

use super::models::ApprovalDecisionProblem;
use super::status::ApprovalRequirementStatus;
use uuid::Uuid;

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
    pub evidence_issue: Option<super::status::ApprovalEvidenceIssue>,
    pub waiting_for_evaluation: bool,
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

fn blank(value: Option<&str>) -> bool {
    value.map(str::trim).unwrap_or("").is_empty()
}

pub fn decide(
    command: &ApprovalDecisionCommand,
    facts: &ApprovalDecisionFacts,
) -> ApprovalDecisionPlan {
    if !facts.visible {
        return ApprovalDecisionPlan::refuse(ApprovalDecisionProblem::unavailable());
    }
    if facts.revision != command.expected_revision {
        return ApprovalDecisionPlan::refuse(ApprovalDecisionProblem::conflict(
            facts.requirement_id,
            command.expected_revision,
            facts.revision,
        ));
    }
    if facts.status != ApprovalRequirementStatus::Pending {
        return ApprovalDecisionPlan::refuse(requirement_state(facts));
    }
    if facts.expired {
        return ApprovalDecisionPlan::refuse(ApprovalDecisionProblem::of(
            "APPROVAL_REQUIREMENT_EXPIRED",
        ));
    }
    if let Some(issue) = facts.evidence_issue {
        if !facts.waiting_for_evaluation {
            return ApprovalDecisionPlan::refuse(ApprovalDecisionProblem::of(issue.as_str()));
        }
    }
    if facts.required_approvers == 0 {
        return ApprovalDecisionPlan::refuse(ApprovalDecisionProblem::of(
            "APPROVAL_REQUIREMENT_NOT_PENDING",
        ));
    }
    if !facts.eligible {
        return ApprovalDecisionPlan::refuse(ApprovalDecisionProblem::of("APPROVER_INELIGIBLE"));
    }
    if command.principal_id == facts.requester_id {
        return ApprovalDecisionPlan::refuse(ApprovalDecisionProblem::of(
            "SELF_APPROVAL_FORBIDDEN",
        ));
    }
    let value = command.value.as_str();
    if value != "APPROVE" && value != "REJECT" {
        return ApprovalDecisionPlan::refuse(ApprovalDecisionProblem::of(
            "APPROVAL_REQUIREMENT_NOT_PENDING",
        ));
    }
    if value == "REJECT" && blank(command.rejection_reason.as_deref()) {
        return ApprovalDecisionPlan::refuse(ApprovalDecisionProblem::of(
            "REJECTION_REASON_REQUIRED",
        ));
    }
    if facts.duplicate {
        return ApprovalDecisionPlan::refuse(ApprovalDecisionProblem::of("DUPLICATE_APPROVER"));
    }
    ApprovalDecisionPlan::record(
        value == "REJECT",
        facts.qualifying_approvers + 1 >= facts.required_approvers,
    )
}

fn requirement_state(facts: &ApprovalDecisionFacts) -> ApprovalDecisionProblem {
    if facts.status == ApprovalRequirementStatus::Expired {
        return ApprovalDecisionProblem::of("APPROVAL_REQUIREMENT_EXPIRED");
    }
    if facts.status == ApprovalRequirementStatus::Invalidated {
        if let Some(code) = &facts.invalidation_code {
            return ApprovalDecisionProblem::of(code.clone());
        }
    }
    ApprovalDecisionProblem::of("APPROVAL_REQUIREMENT_NOT_PENDING")
}

/// The application-owned, non-substitutable P-05 plan selector for one locked decision crossing.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApprovalDecisionPlanner;

impl ApprovalDecisionPlanner {
    pub fn plan(
        &self,
        command: &ApprovalDecisionCommand,
        facts: &ApprovalDecisionFacts,
    ) -> ApprovalDecisionPlan {
        decide(command, facts)
    }
}
