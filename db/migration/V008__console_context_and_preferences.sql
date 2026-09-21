-- principal_id, organization_id, and project_id have no FOREIGN KEY: Aurora DSQL does not support
-- them. Only the seed scripts and test fixtures write this table; the application reads it and never
-- inserts, updates, or deletes a row, so no application-side check replaces the constraints.
CREATE TABLE IF NOT EXISTS console_role_assignments
(
    id
    UUID
    PRIMARY
    KEY,
    principal_id
    UUID
    NOT
    NULL,
    organization_id UUID NULL,
    project_id UUID NULL,
    role_code TEXT NOT NULL CHECK
(
    role_code
    IN
(
    'ORGANIZATION_MEMBER',
    'PROJECT_ADMIN',
    'AGENT_DEVELOPER'
)),
    CHECK
(
(
    organization_id
    IS
    NOT
    NULL
    AND
    project_id
    IS
    NULL
) OR
(
    organization_id
    IS
    NULL
    AND
    project_id
    IS
    NOT
    NULL
)),
    UNIQUE
(
    principal_id,
    organization_id,
    project_id,
    role_code
)
    );

CREATE INDEX IF NOT EXISTS console_role_assignments_principal_scope
    ON console_role_assignments (principal_id, organization_id, project_id);

-- principal_id has no FOREIGN KEY: Aurora DSQL does not support them.
-- `update_preferences` in `hive_persistence::console` confirms the principal exists before persisting
-- a new row; that check is the only referential guard.
CREATE TABLE IF NOT EXISTS principal_display_preferences
(
    principal_id
    UUID
    PRIMARY
    KEY,
    color_scheme TEXT NOT NULL CHECK
(
    color_scheme
    IN
(
    'SYSTEM',
    'LIGHT',
    'DARK'
)),
    density TEXT NOT NULL CHECK
(
    density
    IN
(
    'COMFORTABLE',
    'COMPACT'
)),
    revision BIGINT NOT NULL CHECK
(
    revision >
    0
),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
