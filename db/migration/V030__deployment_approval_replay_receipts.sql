-- Each immutable decision has one durable transport-recovery audit fact for its request key.
-- The FK on decision_id is removed, not ported: Aurora DSQL rejects REFERENCES outright, and
-- auditApprovalReplay() (PostgresDeploymentRepository.java) only ever inserts a receipt for a
-- decision it just confirmed exists in the same transaction (the ON CONFLICT DO NOTHING guard right
-- before it reads the decision's own idempotency row).
CREATE TABLE IF NOT EXISTS deployment_approval_replay_receipts
(
    decision_id UUID NOT NULL,
    request_id UUID NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY
(
    decision_id,
    request_id
)
    );

-- deployment_approval_replay_receipts_no_update/_no_delete and their shared function are removed, not
-- ported: Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright, and auditApprovalReplay() is
-- this table's only writer -- a single, unconditional INSERT, never an UPDATE or DELETE.
