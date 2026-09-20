-- V033 validates the durable human participants that authorize a satisfied requirement and
-- records bounded recovery when a claimed local-worker delivery fails inside its transaction.
ALTER TABLE deployment_audit_events DROP CONSTRAINT IF EXISTS deployment_audit_events_action_check;
ALTER TABLE deployment_audit_events
    ADD CONSTRAINT deployment_audit_events_action_check CHECK (
    action IN ('REQUESTED', 'CANCELED', 'EXECUTION_STARTED', 'EXECUTION_SUCCEEDED', 'EXECUTION_FAILED', 'OUTBOX_DEAD_LETTERED', 'OUTBOX_LEASE_RECLAIMED',
    'OUTBOX_DELIVERY_RETRIED', 'APPROVAL_RECORDED', 'APPROVAL_REPLAYED', 'APPROVAL_SATISFIED', 'APPROVAL_REJECTED', 'APPROVAL_EXPIRED',
    'APPROVAL_INVALIDATED', 'APPROVAL_EXECUTION_BLOCKED')
    );

-- This is deployment_approval_requirement_transition()'s final redefinition -- see V017's removal
-- comment for the full port. Its one guard not present in V017's original (the satisfied-participants
-- consistency check below) is upheld by construction, not by new Java logic: qualifyingApprovers()
-- (PostgresDeploymentRepository.java) derives its return value entirely from a
-- "SELECT actor_principal_id FROM deployment_approval_decisions WHERE approval_requirement_id = ? AND
-- decision = 'APPROVE'" query, so every participant it returns already has a matching decision row by
-- construction, and deployment_approval_decisions' own UNIQUE (approval_requirement_id,
-- actor_principal_id) constraint makes a duplicate participant structurally impossible. The
-- cardinality-equals-required_approvers check holds by induction on ApprovalDecisionPolicy's
-- satisfaction rule (qualifyingApprovers + 1 >= requiredApprovers): recordApprovalDecision() only
-- reaches a decision once the requirement is confirmed PENDING, and a decision transitions the
-- requirement to a terminal state immediately upon acceptance, so no later decision can be recorded
-- against an already-satisfied requirement -- the count can only cross the threshold once, at which
-- point it equals requiredApprovers exactly, never more.

-- deployment_approval_execution_eligible()'s final redefinition is removed here, not ported as SQL:
-- see V017's removal comment for the Java port
-- (PostgresDeploymentRepository.approvalExecutionEligible()).

-- deployment_approval_execution_commit_gate_trigger's final redefinition (this file's own trigger-
-- binding site is V020's -- this is a function-body-only redeclaration, along with its
-- execution_commit_eligible dependency) is removed, not ported: see V020's removal comment.

-- deployment_approval_block_invalid_handoff()'s final redefinition is removed here, not ported as SQL:
-- Aurora DSQL rejects CREATE FUNCTION outright. Ported to
-- PostgresDeploymentRepository.blockApprovalExecution() -- see V017's removal comment, its original
-- declaration site, and reconcileProjectArchives()'s own comment for how that Java port's callers
-- supply the archive actor directly instead of through the hive.m14_archive_actor session GUC this SQL
-- form read.
