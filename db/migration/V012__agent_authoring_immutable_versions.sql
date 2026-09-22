-- This table holds immutable version facts and references only. Publication never deploys an agent.
-- agent_id, catalog_release_id, and published_by have no FOREIGN KEY: Aurora DSQL does not support
-- them. `hive_persistence::agent::draft::publish_draft` confirms the agent exists earlier in the same
-- transaction, reads catalog_release_id from a live catalog_releases query just before this INSERT (so
-- that row is guaranteed to exist), and published_by is the calling principal, guaranteed to exist
-- transitively the way V007's agent_draft_audit_events comment explains.
CREATE TABLE IF NOT EXISTS agent_versions
(
    id
    UUID
    PRIMARY
    KEY,
    agent_id
    UUID
    NOT
    NULL,
    version_number BIGINT NOT NULL CHECK
(
    version_number >
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
    -- JSONB (a JSON array of strings), not TEXT[]: Aurora DSQL does not support array types at all
    -- (confirmed against the real hive-dsql-verification cluster: "datatype text[] not supported").
    -- Readers and writers serialize it as a JSON array rather than a SQL array.
    dependency_versions JSONB NOT NULL DEFAULT '[]'::jsonb,
    catalog_release_id TEXT NOT NULL,
    catalog_release_digest CHAR
(
    64
) NOT NULL CHECK
(
    catalog_release_digest
    ~
    '^[0-9a-f]{64}$'
),
    published_by UUID NOT NULL,
    published_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE
(
    agent_id,
    version_number
),
    UNIQUE
(
    agent_id,
    content_digest
)
    );
-- agent_versions is append-only by convention, not by constraint: Aurora DSQL rejects CREATE RULE
-- outright (0A000 unsupported statement: Rule), so nothing in the schema blocks an UPDATE or DELETE.
-- `hive_persistence::agent::draft` only ever INSERTs into this table.
CREATE INDEX IF NOT EXISTS agent_versions_agent_number ON agent_versions (agent_id, version_number DESC);

-- agent_id, project_id, and actor_principal_id have no FOREIGN KEY: Aurora DSQL does not support
-- them. `hive_persistence::agent::draft::authoring_audit` is only ever called with an agent/project
-- pair confirmed earlier in the same transaction (a loaded agent row, or a fresh INSERT in
-- `create_draft`), and actor_principal_id is the calling principal, guaranteed to exist the way V007's
-- agent_draft_audit_events comment explains.
CREATE TABLE IF NOT EXISTS agent_authoring_audit_events
(
    id
    UUID
    PRIMARY
    KEY,
    agent_id
    UUID
    NOT
    NULL,
    project_id UUID NOT NULL,
    actor_principal_id UUID NOT NULL,
    action TEXT NOT NULL CHECK
(
    action
    IN
(
    'CREATED',
    'SAVED',
    'VALIDATED',
    'PUBLISHED'
)),
    revision BIGINT NULL CHECK
(
    revision >
    0
),
    -- version_id has no FOREIGN KEY either: the one call site that sets it (`publish_draft`) passes
    -- the id of an agent_versions row inserted earlier in the same transaction, so it is always either
    -- NULL or guaranteed to exist.
    version_id UUID NULL,
    content_digest CHAR
(
    64
) NOT NULL CHECK
(
    content_digest
    ~
    '^[0-9a-f]{64}$'
),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
-- agent_authoring_audit_events is append-only by convention, not by constraint: Aurora DSQL rejects
-- CREATE RULE outright (0A000 unsupported statement: Rule), so nothing in the schema blocks an UPDATE
-- or DELETE. `hive_persistence::agent::draft` only ever INSERTs into this table.
CREATE INDEX IF NOT EXISTS agent_authoring_audit_events_agent_time ON agent_authoring_audit_events (agent_id, occurred_at DESC);
