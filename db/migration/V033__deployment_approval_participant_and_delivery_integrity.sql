-- Bounded delivery recovery adds OUTBOX_DELIVERY_RETRIED to the audit action set.
ALTER TABLE deployment_audit_events DROP CONSTRAINT IF EXISTS deployment_audit_events_action_check;
ALTER TABLE deployment_audit_events
    ADD CONSTRAINT deployment_audit_events_action_check CHECK (
    action IN ('REQUESTED', 'CANCELED', 'EXECUTION_STARTED', 'EXECUTION_SUCCEEDED', 'EXECUTION_FAILED', 'OUTBOX_DEAD_LETTERED', 'OUTBOX_LEASE_RECLAIMED',
    'OUTBOX_DELIVERY_RETRIED', 'APPROVAL_RECORDED', 'APPROVAL_REPLAYED', 'APPROVAL_SATISFIED', 'APPROVAL_REJECTED', 'APPROVAL_EXPIRED',
    'APPROVAL_INVALIDATED', 'APPROVAL_EXECUTION_BLOCKED')
    );

-- Nothing in the database checks that a satisfied requirement's recorded participants match its
-- decisions; the invariant holds by construction. `qualifying_approvers` reads its result straight
-- from deployment_approval_decisions where decision = 'APPROVE', so every participant it returns has
-- a matching decision row, and that table's UNIQUE (approval_requirement_id, actor_principal_id)
-- makes a duplicate participant impossible. The participant count equals required_approvers exactly,
-- never more: `record_approval_decision` accepts a decision only while the requirement is PENDING,
-- and accepting one moves the requirement to a terminal state, so the count crosses the threshold at
-- most once.

-- Approval-execution eligibility, the commit-time re-check, and handoff blocking are application
-- code, since Aurora DSQL rejects CREATE FUNCTION and CREATE TRIGGER outright:
-- `approval_execution_eligible` and `block_approval_execution`, whose callers pass the archive actor
-- as an argument rather than through a session setting. V020 covers why the commit-time re-check
-- needs no replacement.
