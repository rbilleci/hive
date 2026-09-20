-- project_id has no FOREIGN KEY: Aurora DSQL does not support them. No Java code path anywhere in this
-- codebase INSERTs, UPDATEs, or DELETEs this table -- it has no write path at all, active or test
-- fixture -- so removing the constraint needs no new Java-side check.
CREATE TABLE IF NOT EXISTS project_dashboard_metrics
(
    project_id
    UUID
    PRIMARY
    KEY,
    active_agents INTEGER NOT NULL DEFAULT 0 CHECK
(
    active_agents
    >=
    0
),
    active_deployments INTEGER NOT NULL DEFAULT 0 CHECK
(
    active_deployments
    >=
    0
),
    failed_deployments INTEGER NOT NULL DEFAULT 0 CHECK
(
    failed_deployments
    >=
    0
),
    pending_approvals INTEGER NOT NULL DEFAULT 0 CHECK
(
    pending_approvals
    >=
    0
),
    unhealthy_resources INTEGER NOT NULL DEFAULT 0 CHECK
(
    unhealthy_resources
    >=
    0
),
    current_period_cost_cents INTEGER NULL CHECK
(
    current_period_cost_cents
    >=
    0
),
    cost_availability TEXT NOT NULL DEFAULT 'UNKNOWN' CHECK
(
    cost_availability
    IN
(
    'AVAILABLE',
    'UNAVAILABLE',
    'UNKNOWN'
)),
    cost_period_start TIMESTAMPTZ NULL,
    cost_period_end TIMESTAMPTZ NULL,
    cost_currency TEXT NULL CHECK
(
    cost_currency
    IS
    NULL
    OR
    cost_currency
    ~
    '^[A-Z]{3}$'
),
    cost_data_as_of TIMESTAMPTZ NULL,
    CHECK
(
    cost_availability
    <>
    'AVAILABLE'
    OR
(
    current_period_cost_cents
    IS
    NOT
    NULL
    AND
    cost_period_start
    IS
    NOT
    NULL
    AND
    cost_period_end
    IS
    NOT
    NULL
    AND
    cost_period_end >
    cost_period_start
    AND
    cost_currency
    IS
    NOT
    NULL
    AND
    cost_data_as_of
    IS
    NOT
    NULL
)
    )
    );

-- Aurora DSQL rejects any constraint (NOT NULL, DEFAULT, or CHECK) inline on ADD COLUMN outright
-- (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN with
-- constraint not supported", even for a bare DEFAULT with no CHECK), and has no ALTER COLUMN ...
-- SET NOT NULL at all ("unsupported ALTER TABLE ALTER COLUMN ... SET NOT NULL statement"). Every
-- column below is added bare, then a default and a NOT NULL-equivalent CHECK are attached as
-- separate statements - see DatabaseMigrator.runStatement()'s comment for how the CHECK statements
-- reach Aurora DSQL's required NOT VALID + VALIDATE CONSTRAINT form automatically.
ALTER TABLE project_dashboard_metrics
    ADD COLUMN IF NOT EXISTS cost_availability TEXT,
    ADD COLUMN IF NOT EXISTS cost_period_start TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS cost_period_end TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS cost_currency TEXT,
    ADD COLUMN IF NOT EXISTS cost_data_as_of TIMESTAMPTZ;

ALTER TABLE project_dashboard_metrics ALTER COLUMN cost_availability SET DEFAULT 'UNKNOWN';

UPDATE project_dashboard_metrics SET cost_availability = 'UNKNOWN' WHERE cost_availability IS NULL;

ALTER TABLE project_dashboard_metrics
    ADD CONSTRAINT project_dashboard_metrics_cost_availability_not_null CHECK (cost_availability IS NOT NULL);

ALTER TABLE project_dashboard_metrics
    ADD CONSTRAINT project_dashboard_metrics_cost_availability_check CHECK (cost_availability IN ('AVAILABLE', 'UNAVAILABLE', 'UNKNOWN'));

ALTER TABLE project_dashboard_metrics
    ADD CONSTRAINT project_dashboard_metrics_cost_currency_check CHECK (cost_currency IS NULL OR cost_currency ~ '^[A-Z]{3}$');

CREATE
OR REPLACE VIEW project_dashboard_projection AS
SELECT project.id                                                                                   AS project_id,
       project.organization_id,
       project.slug,
       project.display_name,
       project.lifecycle_status,
       COALESCE(metrics.active_agents, 0)                                                           AS active_agents,
       COALESCE(metrics.active_deployments, 0)                                                      AS active_deployments,
       COALESCE(metrics.failed_deployments, 0)                                                      AS failed_deployments,
       COALESCE(metrics.pending_approvals, 0)                                                       AS pending_approvals,
       COALESCE(metrics.unhealthy_resources, 0)                                                     AS unhealthy_resources,
       COALESCE(metrics.cost_availability, 'UNKNOWN')                                               AS cost_availability,
       CASE
           WHEN metrics.cost_availability = 'AVAILABLE'
               THEN metrics.current_period_cost_cents END                                           AS current_period_cost_cents,
       CASE
           WHEN metrics.cost_availability = 'AVAILABLE'
               THEN metrics.cost_period_start END                                                   AS cost_period_start,
       CASE WHEN metrics.cost_availability = 'AVAILABLE' THEN metrics.cost_period_end END           AS cost_period_end,
       CASE WHEN metrics.cost_availability = 'AVAILABLE' THEN metrics.cost_currency END             AS cost_currency,
       CASE WHEN metrics.cost_availability = 'AVAILABLE' THEN metrics.cost_data_as_of END           AS cost_data_as_of
FROM projects project
         LEFT JOIN project_dashboard_metrics metrics ON metrics.project_id = project.id;
