-- V018 upgrades a database that recorded V017 before compact scopes, immutable-review screening, and
-- projection-watermark maintenance existed. It is additive and preserves every immutable requirement,
-- decision, evidence, and audit fact.
-- On a database created from scratch this statement does nothing, because V017 already declares the
-- table. It carries no REFERENCES clause, like every other table in this schema: Aurora DSQL does not
-- support them. See V017's comment for what guards these references instead.
CREATE TABLE IF NOT EXISTS deployment_approval_principal_organization_scopes
(
    principal_id UUID NOT NULL,
    organization_id UUID NOT NULL,
    valid_after TIMESTAMPTZ NOT NULL,
    PRIMARY KEY
(
    principal_id,
    organization_id
)
    );
CREATE INDEX IF NOT EXISTS deployment_approval_principal_organization_scopes_organization
    ON deployment_approval_principal_organization_scopes (organization_id, principal_id);
-- A database that recorded V017 before these discovery fields existed gains them here without a
-- table-wide rewrite. On a database created from scratch these statements do nothing, because V017
-- already declares every one of these columns.
ALTER TABLE deployment_approval_requirements
    ADD COLUMN IF NOT EXISTS organization_id UUID;
ALTER TABLE deployment_approval_requirements
    ADD COLUMN IF NOT EXISTS project_id UUID;
ALTER TABLE deployment_approval_requirements
    ADD COLUMN IF NOT EXISTS requested_at TIMESTAMPTZ;
ALTER TABLE deployment_approval_requirements
    ADD COLUMN IF NOT EXISTS required_approvers INTEGER;
ALTER TABLE deployment_approval_requirements DROP CONSTRAINT IF EXISTS deployment_approval_requirements_required_approvers_check;
ALTER TABLE deployment_approval_requirements
    ADD CONSTRAINT deployment_approval_requirements_required_approvers_check
        CHECK (required_approvers IS NULL OR required_approvers BETWEEN 0 AND 2);
-- Aurora DSQL rejects partial indexes outright (0A000 WHERE not supported for CREATE INDEX), so these
-- are full indexes, matching V017's declarations of the same names.
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_global_inbox
    ON deployment_approval_requirements (requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_organization_inbox
    ON deployment_approval_requirements (organization_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_project_inbox
    ON deployment_approval_requirements (project_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_zero_handoff
    ON deployment_approval_requirements (requested_at ASC, id ASC);
-- Also a no-op on a database created from scratch, and without REFERENCES for the same reason as the
-- sibling table above.
CREATE TABLE IF NOT EXISTS deployment_approval_principal_project_scopes
(
    principal_id UUID NOT NULL,
    project_id UUID NOT NULL,
    valid_after TIMESTAMPTZ NOT NULL,
    PRIMARY KEY
(
    principal_id,
    project_id
)
    );
CREATE INDEX IF NOT EXISTS deployment_approval_principal_project_scopes_project
    ON deployment_approval_principal_project_scopes (project_id, principal_id);

-- Decision rows recorded before this migration stay immutable: these constraints are NOT VALID, so
-- they never revalidate existing rows and apply the code-only review-text policy only to later
-- appends. The policy is spelled out inline here and in V017's CHECK constraints rather than shared
-- through a function, because Aurora DSQL rejects CREATE FUNCTION outright; changing one means
-- changing the others by hand.
-- The statements carry no "IF NOT EXISTS" guard: Aurora DSQL rejects anonymous plpgsql blocks
-- outright, and `hive_persistence::migrator` runs each migration file at most once per database, so
-- these constraints can never pre-exist.
ALTER TABLE deployment_approval_decisions
    ADD CONSTRAINT deployment_approval_decisions_m14_comment_check CHECK (
        comment IS NULL OR (length(btrim(comment)) BETWEEN 1 AND 2000 AND btrim(comment) IN (
            'REVIEWED_CHANGE_SCOPE', 'AUTHORIZATION_GRANTED', 'UNACCEPTABLE_CHANGE_SCOPE', 'CHANGE_SCOPE_NOT_APPROVED'))
        ) NOT VALID;
ALTER TABLE deployment_approval_decisions
    ADD CONSTRAINT deployment_approval_decisions_m14_rejection_reason_check CHECK (
        rejection_reason IS NULL OR
        (length(btrim(rejection_reason)) BETWEEN 1 AND 2000 AND btrim(rejection_reason) IN (
            'REVIEWED_CHANGE_SCOPE', 'AUTHORIZATION_GRANTED', 'UNACCEPTABLE_CHANGE_SCOPE', 'CHANGE_SCOPE_NOT_APPROVED'))
        ) NOT VALID;
ALTER TABLE deployment_approval_decisions
    ADD CONSTRAINT deployment_approval_decisions_m14_approval_reason_check CHECK (
        decision <> 'APPROVE' OR rejection_reason IS NULL
        ) NOT VALID;

-- No trigger bumps the deployment projection watermark on an audit append; Aurora DSQL rejects CREATE
-- TRIGGER outright. `hive_persistence::deployment::writes::audit` calls `touch_projection`
-- unconditionally for every action, not only the APPROVAL_-prefixed ones, and the three approval
-- inserts that bypass that helper (`block_approval_execution`, `reconcile_pending`, and the SATISFIED
-- branch of `automatic_approval_handoff`) each call `touch_projection` explicitly. An audit append
-- added anywhere else must do the same.
