-- Resolves a prior active rollback target by project, agent, immutable environment, and chronology.
-- Widened to a full index: Aurora DSQL rejects partial indexes outright (0A000 WHERE not supported
-- for CREATE INDEX). Read-path performance only, not a constraint.
CREATE INDEX IF NOT EXISTS deployments_active_rollback_predecessor
    ON deployments (project_id, agent_id, environment_definition_version_id, requested_at DESC, id DESC);
