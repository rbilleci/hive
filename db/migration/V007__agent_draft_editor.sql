-- agent_id has no FOREIGN KEY: Aurora DSQL does not support them. ensureDraft() is only ever called
-- with a target whose agent existence was just confirmed in the same transaction (a fresh INSERT in
-- createDraft(), or visibleTarget()'s JOIN against agents in every other path), so removing the
-- constraint needs no new Java-side check. No DELETE FROM agents exists in this codebase, so the
-- removed ON DELETE CASCADE was never exercised.
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

-- project_id and principal_id have no FOREIGN KEY: Aurora DSQL does not support them. Neither column
-- has any Java write path at all (no INSERT INTO agent_draft_editor_roles anywhere under
-- service/src/main/java; AgentDraftEditorRoleEntity is a read-only @Immutable JPA mapping), so
-- removing the constraints needs no new Java-side check.
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
-- legacyAudit() is only ever called with an agent whose existence command() already confirmed via
-- visibleTarget() earlier in the same transaction, and with the calling principal, whose existence is
-- guaranteed transitively: every capability check gating a call to legacyAudit() resolves through an
-- organization_membership/project_membership/platform_role_assignment row, and
-- PostgresAdministrationRepository.addMembership already confirms a principal exists before creating
-- one (see V001's comment) — nothing in this codebase ever deletes a principal, so that guarantee
-- holds for the lifetime of the row. Removing the constraints needs no new Java-side check. No
-- DELETE FROM agents exists in this codebase, so the removed ON DELETE CASCADE was never exercised.
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
