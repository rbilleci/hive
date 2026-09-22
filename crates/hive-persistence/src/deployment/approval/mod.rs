//! The approval-requirement lifecycle machinery `deploy`, `recovery` and the outbox worker's
//! `execute` share, in four parts: `requirements` writes the requirement row and the deployment
//! lifecycle moves that follow it, `evidence` decides whether the frozen cycle's required evidence
//! holds, `handoff` decides whether an approved deployment may execute and queues it, and
//! `reconciliation` terminalizes what the frozen facts can no longer satisfy — including the
//! scheduled expiry and project-archive entry points the maintenance loop calls.
//!
//! `clock_timestamp()` has no sea-query builder. Every one of its uses here is a comparison
//! against a stored instant (an expiry, an observation), never a stored value, so the same wall
//! clock is taken from the service and bound as a value. `CURRENT_TIMESTAMP` — the transaction's
//! own start, which every write here stores — stays `Expr::current_timestamp()`.

mod evidence;
mod handoff;
mod reconciliation;
mod requirements;

pub use evidence::{approval_evidence_issue, waiting_for_evaluation};
pub use handoff::{
    approval_execution_eligible, approved_approval_handoff, automatic_approval_handoff,
    compatible_approval_worker, defer_incompatible_approval_handoff,
    defer_pending_approval_handoff,
};
pub(crate) use handoff::{approval_maintenance_failed, release_compatible_approval_handoffs};
pub(crate) use reconciliation::reconcile_expired_approval_requirements;
pub use reconciliation::{
    block_approval_execution, deployment_archive_boundary, reconcile_approval_expiry,
    reconcile_approval_upgrade, reconcile_pending,
};
pub use requirements::{
    approve_deployment_for_execution, cancel_rejected_deployment, invalidate_pending_requirement,
    requirement_expired, transition_requirement,
};

use sea_orm::prelude::DateTimeWithTimeZone;
use uuid::Uuid;

/// `clock_timestamp()` — see this module's doc comment.
fn wall_clock() -> DateTimeWithTimeZone {
    chrono::Utc::now().fixed_offset()
}

/// `jsonb` array of principal ids, as `satisfied_participants` holds it.
fn participants_json(participants: &[Uuid]) -> serde_json::Value {
    serde_json::Value::Array(
        participants
            .iter()
            .map(|id| serde_json::Value::String(id.to_string()))
            .collect(),
    )
}
