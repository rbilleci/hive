-- agent_id has no FOREIGN KEY: Aurora DSQL does not support them. Only the seed scripts write this
-- table; the application reads it and never inserts, updates, or deletes a row, so no application-side
-- check replaces the constraint.
CREATE TABLE IF NOT EXISTS agent_operational_summaries
(
    agent_id
    UUID
    PRIMARY
    KEY,
    draft_validation_status TEXT NOT NULL DEFAULT 'NOT_VALIDATED'
    CHECK
(
    draft_validation_status
    IN
(
    'VALID',
    'INVALID',
    'NOT_VALIDATED'
)),
    draft_error_count INTEGER NOT NULL DEFAULT 0 CHECK
(
    draft_error_count
    >=
    0
),
    draft_warning_count INTEGER NOT NULL DEFAULT 0 CHECK
(
    draft_warning_count
    >=
    0
),
    draft_validated_at TIMESTAMPTZ NULL,
    published_version TEXT NULL,
    published_at TIMESTAMPTZ NULL,
    alias_target_count INTEGER NOT NULL DEFAULT 0 CHECK
(
    alias_target_count
    >=
    0
),
    active_alias_target_count INTEGER NOT NULL DEFAULT 0
    CHECK
(
    active_alias_target_count
    >=
    0
    AND
    active_alias_target_count
    <=
    alias_target_count
),
    deployment_status TEXT NOT NULL DEFAULT 'NOT_DEPLOYED'
    CHECK
(
    deployment_status
    IN
(
    'ACTIVE',
    'DEGRADED',
    'FAILED',
    'NOT_DEPLOYED'
)),
    deployment_observed_at TIMESTAMPTZ NULL,
    evaluation_outcome TEXT NOT NULL DEFAULT 'NO_EVALUATION'
    CHECK
(
    evaluation_outcome
    IN
(
    'PASSED',
    'FAILED',
    'INCONCLUSIVE',
    'NO_EVALUATION'
)),
    evaluation_completed_at TIMESTAMPTZ NULL,
    runtime_health TEXT NOT NULL DEFAULT 'UNKNOWN'
    CHECK
(
    runtime_health
    IN
(
    'HEALTHY',
    'DEGRADED',
    'UNHEALTHY',
    'UNKNOWN'
)),
    runtime_observed_at TIMESTAMPTZ NULL
    );

CREATE
OR REPLACE VIEW agent_operational_view_projection AS
SELECT agent.id                                                   AS agent_id,
       agent.project_id,
       project.organization_id,
       agent.slug,
       agent.display_name,
       agent.lifecycle_status,
       COALESCE(summary.draft_validation_status, 'NOT_VALIDATED') AS draft_validation_status,
       COALESCE(summary.draft_error_count, 0)                     AS draft_error_count,
       COALESCE(summary.draft_warning_count, 0)                   AS draft_warning_count,
       summary.draft_validated_at,
       CASE WHEN summary.published_version IS NULL THEN 'NO_PUBLISHED_VERSION' ELSE 'PUBLISHED' END
                                                                  AS published_version_status,
       summary.published_version,
       summary.published_at,
       COALESCE(summary.alias_target_count, 0)                    AS alias_target_count,
       COALESCE(summary.active_alias_target_count, 0)             AS active_alias_target_count,
       COALESCE(summary.deployment_status, 'NOT_DEPLOYED')        AS deployment_status,
       summary.deployment_observed_at,
       COALESCE(summary.evaluation_outcome, 'NO_EVALUATION')      AS evaluation_outcome,
       summary.evaluation_completed_at,
       COALESCE(summary.runtime_health, 'UNKNOWN')                AS runtime_health,
       summary.runtime_observed_at,
       CASE
           WHEN summary.runtime_observed_at IS NULL THEN 'UNKNOWN'
           WHEN summary.runtime_observed_at < CURRENT_TIMESTAMP - INTERVAL '5 minutes' THEN 'STALE'
    ELSE 'FRESH'
END
AS runtime_freshness
FROM agents agent
INNER JOIN projects project ON project.id = agent.project_id
LEFT JOIN agent_operational_summaries summary ON summary.agent_id = agent.id;
