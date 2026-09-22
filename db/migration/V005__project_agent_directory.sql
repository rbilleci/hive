-- project_id has no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::capability::active_project` confirms the project exists before every agents
-- INSERT; that check is the only referential guard. Nothing ever deletes a project, so no cascade is
-- needed either.
CREATE TABLE IF NOT EXISTS agents
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL,
    slug TEXT NOT NULL,
    display_name TEXT NOT NULL,
    lifecycle_status TEXT NOT NULL CHECK
(
    lifecycle_status
    IN
(
    'ACTIVE',
    'DEPRECATED',
    'ARCHIVED'
))
    );

CREATE UNIQUE INDEX IF NOT EXISTS agents_project_slug_case_insensitive
    ON agents (project_id, LOWER (slug));

CREATE INDEX IF NOT EXISTS agents_directory_keyset
    ON agents (project_id, LOWER (display_name), id);

CREATE INDEX IF NOT EXISTS agents_directory_lifecycle_keyset
    ON agents (project_id, lifecycle_status, LOWER (display_name), id);
