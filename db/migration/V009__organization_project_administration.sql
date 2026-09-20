-- Aurora DSQL rejects any constraint (NOT NULL, DEFAULT, or CHECK) inline on ADD COLUMN outright
-- (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN with
-- constraint not supported", even for a bare DEFAULT with no CHECK), and has no ALTER COLUMN ...
-- SET NOT NULL at all ("unsupported ALTER TABLE ALTER COLUMN ... SET NOT NULL statement"). Every
-- column below is added bare, then a default and a NOT NULL-equivalent CHECK are attached as
-- separate statements - see DatabaseMigrator.runStatement()'s comment for how the CHECK statements
-- reach Aurora DSQL's required NOT VALID + VALIDATE CONSTRAINT form automatically.
ALTER TABLE organizations
    ADD COLUMN IF NOT EXISTS revision BIGINT;
ALTER TABLE organizations ALTER COLUMN revision SET DEFAULT 1;
UPDATE organizations SET revision = 1 WHERE revision IS NULL;
ALTER TABLE organizations ADD CONSTRAINT organizations_revision_not_null CHECK (revision IS NOT NULL);
ALTER TABLE organizations ADD CONSTRAINT organizations_revision_check CHECK (revision > 0);

ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS revision BIGINT,
    ADD COLUMN IF NOT EXISTS description TEXT;
ALTER TABLE projects ALTER COLUMN revision SET DEFAULT 1;
ALTER TABLE projects ALTER COLUMN description SET DEFAULT '';
UPDATE projects SET revision = 1 WHERE revision IS NULL;
UPDATE projects SET description = '' WHERE description IS NULL;
ALTER TABLE projects ADD CONSTRAINT projects_revision_not_null CHECK (revision IS NOT NULL);
ALTER TABLE projects ADD CONSTRAINT projects_revision_check CHECK (revision > 0);
ALTER TABLE projects ADD CONSTRAINT projects_description_not_null CHECK (description IS NOT NULL);

ALTER TABLE organization_memberships
    ADD COLUMN IF NOT EXISTS revision BIGINT;
ALTER TABLE organization_memberships ALTER COLUMN revision SET DEFAULT 1;
UPDATE organization_memberships SET revision = 1 WHERE revision IS NULL;
ALTER TABLE organization_memberships ADD CONSTRAINT organization_memberships_revision_not_null CHECK (revision IS NOT NULL);
ALTER TABLE organization_memberships ADD CONSTRAINT organization_memberships_revision_check CHECK (revision > 0);

ALTER TABLE principals
    ADD COLUMN IF NOT EXISTS email TEXT,
    ADD COLUMN IF NOT EXISTS last_seen_at TIMESTAMPTZ;
ALTER TABLE principals ALTER COLUMN email SET DEFAULT '';
UPDATE principals SET email = '' WHERE email IS NULL;
ALTER TABLE principals ADD CONSTRAINT principals_email_not_null CHECK (email IS NOT NULL);

-- No FOREIGN KEY anywhere below in this migration: Aurora DSQL does not support them. Every write path
-- in PostgresAdministrationRepository already confirms the referenced row exists before inserting here
-- (see each table's specific note), so none of these removals need new Java-side checks.
CREATE TABLE IF NOT EXISTS organization_membership_roles
(
    membership_id
    UUID
    NOT
    NULL,
    role_code TEXT NOT NULL CHECK
(
    role_code
    IN
(
    'ORGANIZATION_MEMBER',
    'ORGANIZATION_ADMIN',
    'AUDITOR'
)),
    PRIMARY KEY
(
    membership_id,
    role_code
)
    );

CREATE TABLE IF NOT EXISTS project_memberships
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL,
    principal_id UUID NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ NULL,
    revision BIGINT NOT NULL CHECK
(
    revision >
    0
),
    -- Same DSQL partial-index rejection and same fix as organization_memberships.active_marker
    -- (V001): TRUE only while active, NULL once ended, so a full unique index on
    -- (project_id, principal_id, active_marker) reproduces "at most one active membership per
    -- (project, principal)" without needing a WHERE clause. See addMembership()/endMembership().
    active_marker BOOLEAN NULL
    );
CREATE UNIQUE INDEX IF NOT EXISTS project_memberships_one_active
    ON project_memberships (project_id, principal_id, active_marker);
CREATE INDEX IF NOT EXISTS project_memberships_principal_active
    ON project_memberships (principal_id, project_id);

CREATE TABLE IF NOT EXISTS project_membership_roles
(
    membership_id
    UUID
    NOT
    NULL,
    role_code TEXT NOT NULL CHECK
(
    role_code
    IN
(
    'PROJECT_ADMIN',
    'AGENT_DEVELOPER',
    'OPERATOR',
    'DEPLOYMENT_APPROVER',
    'AUDITOR'
)),
    PRIMARY KEY
(
    membership_id,
    role_code
)
    );

-- No ON DELETE CASCADE to replicate: nothing in this codebase ever deletes a principal (confirmed via
-- grep for DELETE FROM principals -- no hits anywhere in service/src/main/java).
CREATE TABLE IF NOT EXISTS platform_role_assignments
(
    principal_id
    UUID
    NOT
    NULL,
    role_code TEXT NOT NULL CHECK
(
    role_code =
    'PLATFORM_ADMIN'
),
    PRIMARY KEY
(
    principal_id,
    role_code
)
    );

CREATE TABLE IF NOT EXISTS project_budget_policies
(
    project_id
    UUID
    PRIMARY
    KEY,
    current_revision BIGINT NOT NULL DEFAULT 0 CHECK
(
    current_revision
    >=
    0
)
    );
CREATE TABLE IF NOT EXISTS project_budget_policy_versions
(
    project_id
    UUID
    NOT
    NULL,
    revision BIGINT NOT NULL CHECK
(
    revision >
    0
),
    currency CHAR
(
    3
) NOT NULL,
    monthly_limit_cents INTEGER NOT NULL CHECK
(
    monthly_limit_cents >
    0
),
    warning_threshold_cents INTEGER NOT NULL CHECK
(
    warning_threshold_cents >
    0
    AND
    warning_threshold_cents <
    monthly_limit_cents
),
    change_reason TEXT NOT NULL CHECK
(
    length (
    btrim
(
    change_reason
)) > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY
(
    project_id,
    revision
)
    );

CREATE TABLE IF NOT EXISTS frozen_spend_import_batches
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL,
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL CHECK
(
    period_end >
    period_start
),
    currency CHAR
(
    3
) NOT NULL,
    state TEXT NOT NULL CHECK
(
    state
    IN
(
    'COMPLETE',
    'FAILED',
    'INCOMPLETE'
)),
    amount_cents INTEGER NULL,
    includes_estimates BOOLEAN NOT NULL DEFAULT FALSE,
    data_as_of TIMESTAMPTZ NULL,
    completed_at TIMESTAMPTZ NULL,
    imported_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
CREATE INDEX IF NOT EXISTS frozen_spend_import_batches_current_period
    ON frozen_spend_import_batches (project_id, period_start DESC, imported_at DESC);

CREATE TABLE IF NOT EXISTS project_settings_connections
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL,
    display_name TEXT NOT NULL CHECK
(
    length (
    btrim
(
    display_name
)) > 0),
    definition_version TEXT NOT NULL CHECK
(
    length (
    btrim
(
    definition_version
)) > 0),
    environment TEXT NOT NULL CHECK
(
    environment
    IN
(
    'DEVELOPMENT',
    'STAGING',
    'PRODUCTION'
)),
    credential_status TEXT NOT NULL CHECK
(
    credential_status
    IN
(
    'UNBOUND',
    'REDACTED_BOUND'
)),
    lifecycle_status TEXT NOT NULL CHECK
(
    lifecycle_status
    IN
(
    'ACTIVE',
    'DISABLED',
    'ARCHIVED'
)),
    revision BIGINT NOT NULL CHECK
(
    revision >
    0
),
    UNIQUE
(
    project_id,
    display_name
)
    );
CREATE INDEX IF NOT EXISTS project_settings_connections_project
    ON project_settings_connections (project_id, display_name, id);

CREATE TABLE IF NOT EXISTS project_approval_policies
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL
    UNIQUE,
    current_revision BIGINT NOT NULL CHECK
(
    current_revision >
    0
)
    );
CREATE TABLE IF NOT EXISTS project_approval_policy_versions
(
    policy_id
    UUID
    NOT
    NULL,
    revision BIGINT NOT NULL CHECK
(
    revision >
    0
),
    digest CHAR
(
    64
) NOT NULL,
    matrix JSONB NOT NULL,
    change_reason TEXT NOT NULL CHECK
(
    length (
    btrim
(
    change_reason
)) > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY
(
    policy_id,
    revision
)
    );

CREATE TABLE IF NOT EXISTS administration_audit_events
(
    id
    UUID
    PRIMARY
    KEY,
    actor_principal_id
    UUID
    NOT
    NULL,
    scope_type TEXT NOT NULL CHECK
(
    scope_type
    IN
(
    'ORGANIZATION',
    'PROJECT'
)),
    scope_id UUID NOT NULL,
    action TEXT NOT NULL,
    reason TEXT NULL,
    before_digest CHAR
(
    64
) NULL,
    after_digest CHAR
(
    64
) NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    facts JSONB NOT NULL DEFAULT '{}'::jsonb
    );
CREATE INDEX IF NOT EXISTS administration_audit_events_scope_time
    ON administration_audit_events (scope_type, scope_id, occurred_at DESC);

-- Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule). This table's
-- immutability already has no Java-side equivalent to port for the same reason V040 documents for the
-- trigger-based guards it removed: PostgresAdministrationRepository only ever INSERTs here.
