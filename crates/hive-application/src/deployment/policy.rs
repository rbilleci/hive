//! The application-owned, non-substitutable P-05 plan selector for one locked decision
//! crossing. Pure — no I/O.

use hive_domain::deployment::{
    ApprovalDecisionCommand, ApprovalDecisionFacts, ApprovalDecisionPlan, ApprovalDecisionProblem,
    ApprovalRequirementStatus,
};

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
    if facts.evidence_issue.is_some() && !facts.waiting_for_evaluation {
        return ApprovalDecisionPlan::refuse(ApprovalDecisionProblem::of(
            facts.evidence_issue.clone().unwrap(),
        ));
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
