-- request_key carries the retry identity of the immutable decision it sits on. The column is
-- nullable, so a reader must tolerate a decision that has none.
-- Aurora DSQL rejects any constraint inline on ADD COLUMN outright, including a bare DEFAULT with no
-- NOT NULL/CHECK ("ALTER TABLE ADD COLUMN with constraint not supported"). The column is added bare
-- and the default follows as a separate statement.
ALTER TABLE deployment_approval_decisions
    ADD COLUMN IF NOT EXISTS request_key UUID;
ALTER TABLE deployment_approval_decisions ALTER COLUMN request_key SET DEFAULT gen_random_uuid();
-- Aurora DSQL rejects partial indexes outright (0A000 WHERE not supported for CREATE INDEX); widened
-- to a full index (read-path performance only, not a constraint).
CREATE INDEX IF NOT EXISTS deployment_approval_decisions_retry
    ON deployment_approval_decisions (approval_requirement_id, actor_principal_id, request_key);

-- Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, including CONSTRAINT TRIGGER, so
-- no deferred commit gate re-checks approval-execution eligibility between a worker's own check and
-- its commit. Nothing replaces it, because every path that can invalidate eligibility
-- (`block_approval_execution`, `reconcile_pending`'s cancel branch,
-- `invalidate_pending_approvals_for_archived_project`) updates the same deployments row the
-- IN_PROGRESS transition writes, and Aurora DSQL's optimistic concurrency control rejects the second
-- of two concurrent writers to one row with SQLSTATE 40001: the loser rolls back, which closes that
-- race.
