-- Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, so no database guard serializes a
-- deployment request against a concurrent project archive. `deploy` closes that race itself: its
-- `active_project` check takes a FOR SHARE lock on the projects row before `insert_deployment` runs
-- and holds it for the rest of the transaction. Because that lock makes the race impossible, nothing
-- writes deployments.project_lifecycle_revision (V027) and it stays NULL for every new deployment --
-- exactly the fallback branch the archive-boundary comparisons in V027 handle, so their timestamp
-- comparison stays precise.

-- The approval requirement, outbox, and execution-worker gates are application code for the same
-- reason, as are handoff selection and archive reconciliation: `ensure_requirement`,
-- `automatic_approval_handoff`, `compatible_approval_handoff_deployments`,
-- `reconcile_project_archives`.

-- Nothing in the database backfills, validates, or upgrades deployment_plan_review_facts:
-- `insert_plan_review` writes it directly, for every plan, from already-validated values.
