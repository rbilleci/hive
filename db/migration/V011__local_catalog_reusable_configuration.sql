-- M11 is intentionally local: a Git fixture projection, PostgreSQL metadata, and no secret material.
-- catalog_releases, catalog_projection_heads, catalog_environments, and catalog_definitions below have
-- no Java write path at all, active or test fixture: grep confirms zero INSERT/UPDATE statements
-- against any of the four anywhere under service/src/main/java. They are populated exclusively by the
-- idempotent seed script db/seed/local-catalog-configuration.sql, so removing their FOREIGN KEY and
-- CREATE RULE declarations (Aurora DSQL supports neither) needs no Java-side change.
CREATE TABLE IF NOT EXISTS catalog_releases
(
    id
    TEXT
    PRIMARY
    KEY,
    source
    TEXT
    NOT
    NULL
    CHECK
(
    source
    LIKE
    'git:%'
),
    source_digest CHAR
(
    64
) NOT NULL CHECK
(
    source_digest
    ~
    '^[0-9a-f]{64}$'
),
    released_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Nullable, never FALSE: Aurora DSQL rejects the partial index a plain boolean would need
    -- (`UNIQUE (current) WHERE current`, 0A000 WHERE not supported for CREATE INDEX). TRUE for the
    -- one current release, NULL for every other, since a full unique index never treats two NULLs as
    -- conflicting - reproducing "at most one current release" without a WHERE clause. No Java write
    -- path exists for this table (see this file's header comment), so nothing but the seed fixture,
    -- which already only ever inserts TRUE, is affected.
    current BOOLEAN NULL
    );
CREATE UNIQUE INDEX IF NOT EXISTS catalog_releases_one_current ON catalog_releases (current);
-- The local projection head is the only pointer and a Git migration advances it without mutating a release.
CREATE TABLE IF NOT EXISTS catalog_projection_heads
(
    id
    TEXT
    PRIMARY
    KEY
    CHECK
(
    id =
    'local'
),
    release_id TEXT NOT NULL
    );
CREATE TABLE IF NOT EXISTS catalog_environments
(
    release_id
    TEXT
    NOT
    NULL,
    environment TEXT NOT NULL CHECK
(
    environment
    IN
(
    'DEVELOPMENT',
    'STAGING',
    'PRODUCTION'
)),
    PRIMARY KEY
(
    release_id,
    environment
)
    );
CREATE TABLE IF NOT EXISTS catalog_definitions
(
    release_id
    TEXT
    NOT
    NULL,
    definition_kind TEXT NOT NULL CHECK
(
    definition_kind
    IN
(
    'model',
    'tool'
)),
    identity TEXT NOT NULL CHECK
(
    identity
    ~
    '^[a-z][a-z0-9-]{0,62}$'
),
    version TEXT NOT NULL CHECK
(
    version
    ~
    '^[a-z][a-z0-9-]{0,62}$'
),
    display_name TEXT NOT NULL,
    content_digest CHAR
(
    64
) NOT NULL CHECK
(
    content_digest
    ~
    '^[0-9a-f]{64}$'
),
    available_environments JSONB NOT NULL,
    PRIMARY KEY
(
    release_id,
    definition_kind,
    identity,
    version
)
    );

-- project_id has no FOREIGN KEY: Aurora DSQL does not support them. createResource() confirms the
-- project exists (activeProject()) before every reusable_resources INSERT, so removing the constraint
-- needs no new Java-side check.
CREATE TABLE IF NOT EXISTS reusable_resources
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL,
    resource_kind TEXT NOT NULL CHECK
(
    resource_kind
    IN
(
    'PROMPT',
    'POLICY',
    'MODEL_PROFILE'
)),
    name TEXT NOT NULL CHECK
(
    length (
    btrim
(
    name
)) BETWEEN 2 AND 81),
    identity TEXT NOT NULL CHECK
(
    identity
    ~
    '^[a-z][a-z0-9-]{0,80}$'
    AND
    identity !~ '--'
    AND
    identity !~ '-$'
),
    current_draft_revision BIGINT NOT NULL CHECK
(
    current_draft_revision >
    0
),
    current_published_version BIGINT NULL CHECK
(
    current_published_version >
    0
),
    lifecycle_status TEXT NOT NULL CHECK
(
    lifecycle_status
    IN
(
    'ACTIVE',
    'ARCHIVED'
)),
    UNIQUE
(
    project_id,
    resource_kind,
    name
)
    );
ALTER TABLE reusable_resources
    ADD COLUMN IF NOT EXISTS identity TEXT;
UPDATE reusable_resources
SET identity = btrim(lower(regexp_replace(btrim(name), '[^a-zA-Z0-9]+', '-', 'g')), '-')
WHERE identity IS NULL;
-- Aurora DSQL has no ALTER COLUMN ... SET NOT NULL at all (confirmed against the real
-- hive-dsql-verification cluster: "unsupported ALTER TABLE ALTER COLUMN ... SET NOT NULL
-- statement"); expressed as a CHECK instead - see DatabaseMigrator.runStatement()'s comment for how
-- it reaches Aurora DSQL's required NOT VALID + VALIDATE CONSTRAINT form automatically.
ALTER TABLE reusable_resources
    ADD CONSTRAINT reusable_resources_identity_nn CHECK (identity IS NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS reusable_resources_project_kind_identity ON reusable_resources (project_id, resource_kind, identity);
-- resource_id has no FOREIGN KEY: Aurora DSQL does not support them. insertDraft() is only ever called
-- with a resource whose existence was just confirmed in the same transaction (a fresh INSERT in
-- createResource(), or lockedResource() in updateDraft()), so removing the constraint needs no new
-- Java-side check.
CREATE TABLE IF NOT EXISTS reusable_resource_drafts
(
    resource_id
    UUID
    NOT
    NULL,
    revision BIGINT NOT NULL CHECK
(
    revision >
    0
),
    content TEXT NOT NULL CHECK
(
    length
(
    content
) <= 16000),
    canonical_document JSONB NOT NULL,
    content_digest CHAR
(
    64
) NOT NULL CHECK
(
    content_digest
    ~
    '^[0-9a-f]{64}$'
),
    dependencies JSONB NOT NULL DEFAULT '[]'::jsonb,
    validation_status TEXT NOT NULL CHECK
(
    validation_status
    IN
(
    'UNVALIDATED',
    'VALID',
    'INVALID'
)),
    diagnostics JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY
(
    resource_id,
    revision
)
    );
-- resource_id and published_by have no FOREIGN KEY: Aurora DSQL does not support them. publish()
-- confirms the resource exists via lockedResource() earlier in the same transaction, and published_by
-- is the calling principal, guaranteed to exist transitively the same way V007's
-- agent_draft_audit_events comment (Agent authoring domain step) explains. Removing the constraints
-- needs no new Java-side check.
CREATE TABLE IF NOT EXISTS reusable_resource_versions
(
    resource_id
    UUID
    NOT
    NULL,
    version BIGINT NOT NULL CHECK
(
    version >
    0
),
    canonical_document JSONB NOT NULL,
    content_digest CHAR
(
    64
) NOT NULL CHECK
(
    content_digest
    ~
    '^[0-9a-f]{64}$'
),
    dependencies JSONB NOT NULL DEFAULT '[]'::jsonb,
    published_by UUID NOT NULL,
    published_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY
(
    resource_id,
    version
),
    UNIQUE
(
    resource_id,
    content_digest
)
    );
-- Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule); see V040's comment
-- for the parallel reasoning on the trigger-based audit guards it removed. PostgresConfigurationRepository
-- only ever INSERTs into this table.

-- project_id has no FOREIGN KEY: Aurora DSQL does not support them. createMcpServer() and
-- saveLegacyTool() both confirm the project exists (activeProject()) before their INSERTs, so removing
-- the constraint needs no new Java-side check.
CREATE TABLE IF NOT EXISTS project_tool_connections
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL,
    name TEXT NOT NULL CHECK
(
    length (
    btrim
(
    name
)) BETWEEN 2 AND 81),
    definition_identity TEXT NOT NULL CHECK
(
    definition_identity
    ~
    '^[a-z][a-z0-9-]{0,62}$'
),
    definition_version TEXT NOT NULL CHECK
(
    definition_version
    ~
    '^[a-z][a-z0-9-]{0,62}$'
),
    environment TEXT NOT NULL CHECK
(
    environment
    IN
(
    'DEVELOPMENT',
    'STAGING',
    'PRODUCTION'
)),
    redacted_secret_reference TEXT NOT NULL CHECK
(
    redacted_secret_reference
    ~
    '^redacted://[a-z0-9/_-]{3,180}$'
),
    lifecycle_status TEXT NOT NULL CHECK
(
    lifecycle_status
    IN
(
    'ACTIVE',
    'DISABLED',
    'ARCHIVED'
)),
    rotation_summary TEXT NOT NULL CHECK
(
    length
(
    rotation_summary
) <= 240),
    revision BIGINT NOT NULL CHECK
(
    revision >
    0
),
    UNIQUE
(
    project_id,
    name
)
    );
-- actor_principal_id and project_id have no FOREIGN KEY: Aurora DSQL does not support them. audit()
-- is only ever called with a project already validated earlier in the same transaction (activeProject()
-- or lockedResource()/lockedTool()), and actor_principal_id is the calling principal, guaranteed to
-- exist transitively the same way V007's agent_draft_audit_events comment (Agent authoring domain
-- step) explains. Removing the constraints needs no new Java-side check.
CREATE TABLE IF NOT EXISTS configuration_audit_events
(
    id
    UUID
    PRIMARY
    KEY,
    actor_principal_id
    UUID
    NOT
    NULL,
    project_id UUID NOT NULL,
    action TEXT NOT NULL,
    subject_id UUID NOT NULL,
    content_digest CHAR
(
    64
) NOT NULL CHECK
(
    content_digest
    ~
    '^[0-9a-f]{64}$'
),
    detail TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
-- Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule); see V040's comment
-- for the parallel reasoning on the trigger-based audit guards it removed. PostgresConfigurationRepository
-- only ever INSERTs into this table.
