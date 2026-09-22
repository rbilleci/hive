-- Archive-history recovery matches an immutable administration event by project and timestamp.
-- This non-partial index covers processed history as well as pending reconciliation events.
CREATE INDEX IF NOT EXISTS deployment_approval_project_archive_events_history_lookup
    ON deployment_approval_project_archive_events (project_id, archived_at);
