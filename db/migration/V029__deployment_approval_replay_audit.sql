-- A replay records transport recovery without changing the immutable decision it returns.
ALTER TABLE deployment_audit_events DROP CONSTRAINT IF EXISTS deployment_audit_events_action_check;
ALTER TABLE deployment_audit_events
    ADD CONSTRAINT deployment_audit_events_action_check CHECK (
    action IN ('REQUESTED', 'CANCELED', 'EXECUTION_STARTED', 'EXECUTION_SUCCEEDED', 'EXECUTION_FAILED', 'OUTBOX_DEAD_LETTERED', 'OUTBOX_LEASE_RECLAIMED',
    'APPROVAL_RECORDED', 'APPROVAL_REPLAYED', 'APPROVAL_SATISFIED', 'APPROVAL_REJECTED', 'APPROVAL_EXPIRED', 'APPROVAL_INVALIDATED', 'APPROVAL_EXECUTION_BLOCKED')
    );
