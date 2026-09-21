-- organization_id has no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::administration::mutations::create_project` confirms the organization row exists
-- before this INSERT; that check is the only referential guard.
CREATE TABLE IF NOT EXISTS projects
(
    id
    UUID
    PRIMARY
    KEY,
    organization_id
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
    'ARCHIVED'
))
    );

CREATE UNIQUE INDEX IF NOT EXISTS projects_organization_slug_case_insensitive
    ON projects (organization_id, lower (slug));

CREATE INDEX IF NOT EXISTS projects_organization_lifecycle_display_name
    ON projects (organization_id, lifecycle_status, display_name);
