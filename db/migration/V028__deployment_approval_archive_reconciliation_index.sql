-- Archive reconciliation only transitions live approval cycles. Retained terminal deployment
-- history remains readable without entering an archive-page candidate scan.
-- Widened to a full index: Aurora DSQL rejects partial indexes outright (0A000 WHERE not supported
-- for CREATE INDEX). Read-path performance only, not a constraint.
CREATE INDEX IF NOT EXISTS deployments_approval_archive_reconciliation_candidates
    ON deployments (project_id, id);
