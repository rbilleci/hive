-- agent_id has no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::agent::draft::ensure_draft` is only ever called with an agent row loaded or
-- inserted earlier in the same transaction; that is the only referential guard. Nothing ever deletes
-- an agent, so no cascade is needed either.
CREATE TABLE IF NOT EXISTS agent_drafts
(
    agent_id
    UUID
    PRIMARY
    KEY,
    document JSONB NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1 CHECK
(
    revision >
    0
),
    validation_status TEXT NOT NULL DEFAULT 'NOT_VALIDATED'
    CHECK
(
    validation_status
    IN
(
    'VALID',
    'INVALID',
    'NOT_VALIDATED'
)),
    validation_diagnostics JSONB NOT NULL DEFAULT '[]'::jsonb,
    validated_at TIMESTAMPTZ NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );

-- project_id and principal_id have no FOREIGN KEY: Aurora DSQL does not support them. Only the seed
-- scripts write this table; the application reads it and never inserts, updates, or deletes a row, so
-- no application-side check replaces the constraints.
CREATE TABLE IF NOT EXISTS agent_draft_editor_roles
(
    project_id
    UUID
    NOT
    NULL,
    principal_id UUID NOT NULL,
    role_code TEXT NOT NULL CHECK
(
    role_code
    IN
(
    'PROJECT_ADMIN',
    'AGENT_DEVELOPER'
)),
    PRIMARY KEY
(
    project_id,
    principal_id,
    role_code
)
    );

-- agent_id and principal_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::agent::draft::legacy_audit` is only ever called with an agent row confirmed
-- earlier in the same transaction, and with the calling principal. A principal's existence is
-- guaranteed transitively: every capability check gating a write resolves through an
-- organization_membership/project_membership/platform_role_assignment row, and
-- `hive_persistence::administration::mutations::add_membership` confirms a principal exists before
-- creating one (see V001's comment); nothing ever deletes a principal, so that guarantee holds for the
-- lifetime of the row. Nothing ever deletes an agent either, so no cascade is needed.
CREATE TABLE IF NOT EXISTS agent_draft_audit_events
(
    id
    UUID
    PRIMARY
    KEY,
    agent_id
    UUID
    NOT
    NULL,
    principal_id UUID NOT NULL,
    action TEXT NOT NULL CHECK
(
    action
    IN
(
    'UPDATED',
    'VALIDATED'
)),
    revision BIGINT NOT NULL CHECK
(
    revision >
    0
),
    content_digest TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );

CREATE INDEX IF NOT EXISTS agent_draft_editor_roles_lookup
    ON agent_draft_editor_roles (project_id, principal_id);

CREATE INDEX IF NOT EXISTS agent_draft_audit_events_agent_revision
    ON agent_draft_audit_events (agent_id, revision);
