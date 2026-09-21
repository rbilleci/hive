-- Project administration records an immutable archive boundary. Deployment-owned maintenance
-- consumes that event and terminalizes only pending approval cycles in separate deployment work.
-- project_id/actor_principal_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- `record_project_archive_event` is this table's only writer, called from `lifecycle` immediately
-- after the projects row it reads project_id from is updated in the same transaction, with
-- actor_principal_id the caller's own required, non-null actor argument.
CREATE TABLE IF NOT EXISTS deployment_approval_project_archive_events
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL,
    actor_principal_id UUID NULL,
    archived_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    processed_at TIMESTAMPTZ NULL
    );
-- Aurora DSQL rejects partial indexes outright (0A000 WHERE not supported for CREATE INDEX), so this
-- is a full index: read-path performance only, not a constraint. No index here or in V027 enforces
-- "at most one unprocessed event per project".
CREATE INDEX IF NOT EXISTS deployment_approval_project_archive_events_delivery
    ON deployment_approval_project_archive_events (archived_at, id);

-- Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, so no database machinery
-- discovers, enqueues, or drains these events. `lifecycle` is the only writer of
-- projects.lifecycle_status and records the archive event itself in the same transaction;
-- `approval_project_archive_pending` and `reconcile_project_archives` consume the events.
