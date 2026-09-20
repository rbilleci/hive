-- M14 stores the retry identity with each immutable decision. Existing facts remain readable
-- without a retry key; every new repository decision supplies one and the database indexes it.
-- Aurora DSQL rejects any constraint inline on ADD COLUMN outright, including a bare DEFAULT with no
-- NOT NULL/CHECK (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN
-- with constraint not supported"). Column added bare; the default follows as a separate statement.
ALTER TABLE deployment_approval_decisions
    ADD COLUMN IF NOT EXISTS request_key UUID;
ALTER TABLE deployment_approval_decisions ALTER COLUMN request_key SET DEFAULT gen_random_uuid();
-- Aurora DSQL rejects partial indexes outright (0A000 WHERE not supported for CREATE INDEX); widened
-- to a full index (read-path performance only, not a constraint).
CREATE INDEX IF NOT EXISTS deployment_approval_decisions_retry
    ON deployment_approval_decisions (approval_requirement_id, actor_principal_id, request_key);

-- deployment_approval_execution_commit_gate_trigger (a DEFERRED CONSTRAINT TRIGGER, AFTER UPDATE OF
-- lifecycle_status ON deployments WHEN NEW.lifecycle_status = 'IN_PROGRESS') and its two functions
-- (this one and deployment_approval_execution_commit_eligible(), whose true final form is V033's) are
-- removed, not ported: Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright, including
-- CONSTRAINT TRIGGER. Its purpose -- catching a state change between the worker's own eligibility
-- check and this transaction's commit -- needs no Java replacement: every code path capable of
-- invalidating eligibility in this domain (blockApprovalExecution(), reconcilePending()'s cancel
-- branch, invalidatePendingApprovalsForArchivedProject()) itself performs an UPDATE to the identical
-- deployments row the IN_PROGRESS transition also writes, and DSQL's own optimistic concurrency
-- control already rejects two concurrent writers to the same row with SQLSTATE 40001 (empirically
-- confirmed this domain step, the same mechanism m14-approval-transition's advisory lock removal
-- already relies on) -- whichever transaction commits second loses and rolls back, closing the exact
-- race this deferred re-check existed to catch.
