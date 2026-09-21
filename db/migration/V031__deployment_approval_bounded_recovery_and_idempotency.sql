-- No archive-history or replay-receipt catch-up runs anywhere: `record_project_archive_event` writes
-- the archive event in the same transaction as the lifecycle_status update it follows, and
-- `record_approval_decision` writes the replay receipt in the same transaction as the
-- APPROVAL_REPLAYED audit event, so neither relation can lag behind the audit trail.

-- Each decision carries the normalized request semantics that own its retry key. Both columns are
-- nullable: a decision without an expected revision fails closed for a request-key replay, since the
-- revision it was made against is unknown.
ALTER TABLE deployment_approval_decisions
    ADD COLUMN IF NOT EXISTS request_expected_revision BIGINT NULL;
ALTER TABLE deployment_approval_decisions
    ADD COLUMN IF NOT EXISTS request_fingerprint CHAR (64) NULL;
-- Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, so no database default derives
-- either column. `insert_decision` is this table's only writer and supplies both on every insert: a
-- non-negative, previously-read expected revision, and `decision_request_fingerprint`, a SHA-256
-- digest rendered as exactly 64 lowercase hex characters.

-- Approval-inbox scope is decided in application code for the same reason; see V034.
