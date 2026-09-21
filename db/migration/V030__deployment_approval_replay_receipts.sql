-- Each immutable decision has one durable transport-recovery audit fact for its request key.
-- decision_id has no FOREIGN KEY: Aurora DSQL rejects REFERENCES outright.
-- `record_approval_decision`'s replay branch inserts a receipt only for a decision it read in the
-- same transaction.
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

-- Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, so nothing in the database freezes
-- this table. `record_approval_decision` is its only writer -- a single INSERT ... ON CONFLICT DO
-- NOTHING, never an UPDATE or DELETE.
