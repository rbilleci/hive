-- Archive boundaries order by the locked project lifecycle revision where one is present, and fall
-- back to the archive timestamp where it is not, so a request written after a restore cannot inherit
-- an older archive.
ALTER TABLE deployments
    ADD COLUMN IF NOT EXISTS project_lifecycle_revision BIGINT NULL;
ALTER TABLE deployment_approval_project_archive_events
    ADD COLUMN IF NOT EXISTS archived_project_revision BIGINT NULL;

-- Both widened to full indexes: Aurora DSQL rejects partial indexes outright (0A000 WHERE not
-- supported for CREATE INDEX). Read-path performance only, not a constraint, for both.
DROP INDEX IF EXISTS deployment_approval_project_archive_events_pending;
CREATE INDEX IF NOT EXISTS deployment_approval_project_archive_events_pending
    ON deployment_approval_project_archive_events (project_id, archived_at, id);
CREATE INDEX IF NOT EXISTS deployment_approval_project_archive_events_revision
    ON deployment_approval_project_archive_events (project_id, archived_project_revision);

-- Aurora DSQL rejects CREATE FUNCTION outright, so the two archive-boundary predicates live in
-- application code, inline inside the queries that consult them:
--   * the deployment boundary in `deployment_archive_boundary`.
--   * the deployment/archive-event boundary at both of its call sites in
--     `reconcile_project_archives` (the candidate fetch and the still-pending re-check), each as a
--     join on archive_event.id and archive_event.project_id = deployment.project_id carrying the full
--     revision-or-timestamp condition.
-- Both keep the project_lifecycle_revision branch even though deployments.project_lifecycle_revision
-- stays NULL for every row this system writes (`deploy`'s FOR SHARE lock on the projects row already
-- prevents the race the revision comparison guards); the branch stays so a row carrying a revision
-- still compares correctly.

-- Aurora DSQL rejects CREATE TRIGGER, CREATE FUNCTION, and CREATE RULE outright, so nothing in the
-- database freezes these columns or makes deployment_approval_project_archive_events append-only.
-- The invariants hold through its two writers instead: `record_project_archive_event` only INSERTs,
-- setting id/project_id/actor_principal_id/archived_project_revision once and never again, and
-- `reconcile_project_archives` is the only UPDATE -- a single "SET processed_at = CURRENT_TIMESTAMP
-- WHERE id = ?" whose WHERE clause and event-selection query both gate on processed_at IS NULL, so
-- processed_at only ever moves from NULL to non-NULL. Nothing DELETEs from this table.

-- The approval requirement predicate and archive reconciliation page are application code for the
-- same reason: `ensure_requirement` and `reconcile_project_archives`.
