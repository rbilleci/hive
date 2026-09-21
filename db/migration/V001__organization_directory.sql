CREATE TABLE IF NOT EXISTS principals
(
    id
    UUID
    PRIMARY
    KEY,
    subject
    TEXT
    NOT
    NULL
    UNIQUE,
    display_name
    TEXT
    NOT
    NULL
);

CREATE TABLE IF NOT EXISTS organizations
(
    id
    UUID
    PRIMARY
    KEY,
    slug
    TEXT
    NOT
    NULL
    UNIQUE,
    display_name
    TEXT
    NOT
    NULL,
    lifecycle_status
    TEXT
    NOT
    NULL
    CHECK (
    lifecycle_status
    IN
(
    'ACTIVE',
    'ARCHIVED'
))
    );

-- organization_id and principal_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::administration::mutations::add_membership` confirms both the organization row and
-- the principal row exist before every INSERT here; that check is the only referential guard.
CREATE TABLE IF NOT EXISTS organization_memberships
(
    id
    UUID
    PRIMARY
    KEY,
    organization_id
    UUID
    NOT
    NULL,
    principal_id UUID NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ NULL,
    -- Aurora DSQL rejects partial indexes outright (0A000 WHERE not supported for CREATE INDEX,
    -- confirmed against the real hive-dsql-verification cluster), so the "at most one active
    -- membership per (organization, principal)" invariant below cannot live in a
    -- `UNIQUE (...) WHERE ended_at IS NULL` index. TRUE only while active, NULL once ended (never
    -- FALSE) makes a full, non-partial unique index equivalent: a standard SQL unique constraint
    -- never treats two NULLs as conflicting, so ended memberships (this column NULL) never collide
    -- with each other, while active memberships (this column TRUE) still collide correctly on
    -- (organization_id, principal_id, active_marker).
    -- `hive_persistence::administration::mutations::add_membership` sets it TRUE on insert;
    -- `end_membership` nulls it out in the same UPDATE that sets ended_at. project_memberships
    -- carries the same column for the same reason.
    active_marker BOOLEAN NULL
    );

CREATE INDEX IF NOT EXISTS organization_memberships_active_selector
    ON organization_memberships (principal_id, organization_id);

CREATE UNIQUE INDEX IF NOT EXISTS organization_memberships_one_active
    ON organization_memberships (organization_id, principal_id, active_marker);

CREATE INDEX IF NOT EXISTS organizations_selector_order
    ON organizations (display_name, id);
