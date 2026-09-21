-- Approval decisions are recorded separately from the frozen request, policy, plan, and evidence
-- facts. Requirement state may advance once; decision and invalidation records never change or
-- disappear.
-- The review-text safety rule is spelled out inline in the two CHECK constraints below rather than
-- factored into a shared function: Aurora DSQL rejects CREATE FUNCTION outright, even a LANGUAGE sql
-- IMMUTABLE one. Changing one of those constraints means changing the other by hand.
--
-- deployment_id/organization_id/project_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::deployment::writes::insert_approval_requirement` derives organization_id and
-- project_id from the just-created deployments row rather than from caller-supplied values, so both
-- columns are consistent with that row by construction; a missing deployment yields no row and the
-- follow-up existence check raises SQLSTATE 23503, the code a real FK violation would have used.
CREATE TABLE IF NOT EXISTS deployment_approval_requirements
(
    id
    UUID
    PRIMARY
    KEY,
    deployment_id
    UUID
    NOT
    NULL
    UNIQUE,
    revision BIGINT NOT NULL CHECK
(
    revision >
    0
),
    organization_id UUID NOT NULL,
    project_id UUID NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL,
    required_approvers INTEGER NOT NULL CHECK
(
    required_approvers
    BETWEEN
    0
    AND
    2
),
    status TEXT NOT NULL CHECK
(
    status
    IN
(
    'PENDING',
    'SATISFIED',
    'REJECTED',
    'EXPIRED',
    'INVALIDATED'
)),
    expires_at TIMESTAMPTZ NOT NULL,
    satisfied_at TIMESTAMPTZ NULL,
    rejected_at TIMESTAMPTZ NULL,
    invalidated_at TIMESTAMPTZ NULL,
    invalidation_code TEXT NULL CHECK
(
    invalidation_code
    IS
    NULL
    OR
    invalidation_code
    IN
(
    'APPROVAL_EVIDENCE_MISSING',
    'APPROVAL_EVIDENCE_EXPIRED',
    'APPROVAL_EVIDENCE_MISMATCH',
    'APPROVAL_REQUIREMENT_EXPIRED',
    'TERMINAL_LIFECYCLE'
)),
    -- JSONB (a JSON array of UUID strings), not UUID[]: Aurora DSQL does not support array types at
    -- all (confirmed against the real hive-dsql-verification cluster: "datatype text[] not supported",
    -- reproduced the same way for uuid[]). `raw_requirement` in `hive_persistence::deployment::rows`
    -- and `transition_requirement` in `hive_persistence::deployment::approval` serialize it as a JSON
    -- array rather than a SQL array.
    satisfied_participants JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK
(
(
    status =
    'SATISFIED'
) =
(
    satisfied_at
    IS
    NOT
    NULL
)),
    CHECK
(
(
    status =
    'REJECTED'
) =
(
    rejected_at
    IS
    NOT
    NULL
)),
    CHECK
(
(
    status =
    'INVALIDATED'
) =
(
    invalidated_at
    IS
    NOT
    NULL
)),
    CHECK
(
    jsonb_array_length
(
    satisfied_participants
) <= 2)
    );
-- Aurora DSQL rejects partial indexes outright (0A000 WHERE not supported for CREATE INDEX); every
-- index below this point in this file is full rather than partial for that reason alone - each is a
-- read-path performance index, not a constraint, so indexing rows the query's own WHERE clause
-- already excludes costs nothing but index size.
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_pending
    ON deployment_approval_requirements (status, expires_at, id);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_expiry
    ON deployment_approval_requirements (expires_at, id);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_satisfied_handoff
    ON deployment_approval_requirements (deployment_id);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_deployment_requested
    ON deployment_approval_requirements (deployment_id, id);
-- These indexes cover the requirement relation, which starts empty here, so no migration-time index
-- build runs over retained deployment history.
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_global_inbox
    ON deployment_approval_requirements (requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_organization_inbox
    ON deployment_approval_requirements (organization_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_project_inbox
    ON deployment_approval_requirements (project_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_zero_handoff
    ON deployment_approval_requirements (requested_at ASC, id ASC);
-- No SQL function bumps the deployment projection watermark: Aurora DSQL rejects CREATE FUNCTION
-- outright. `hive_persistence::deployment::writes::touch_projection` bumps it, called directly from
-- every write path that changes approval state.
-- Inbox discovery uses compact authority scopes. Organization roles retain one organization row,
-- and project roles retain one project row. Request creation and project creation therefore never
-- expand an authority grant across every project in an organization.
-- principal_id/organization_id/project_id have no FOREIGN KEY on either table below: Aurora DSQL does
-- not support them. `refresh_organization_scope` and `refresh_project_scope` in
-- `hive_persistence::administration::scopes` only ever write a (principal, organization/project) pair
-- resolved from a live organization_memberships/project_memberships row in the same query, so both
-- tables are consistent with real membership state by construction.
CREATE TABLE IF NOT EXISTS deployment_approval_principal_organization_scopes
(
    principal_id
    UUID
    NOT
    NULL,
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
CREATE TABLE IF NOT EXISTS deployment_approval_principal_project_scopes
(
    principal_id
    UUID
    NOT
    NULL,
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
CREATE INDEX IF NOT EXISTS project_memberships_project_active
    ON project_memberships (project_id, principal_id);
-- This transient queue records only an execution handoff deferred until a compatible worker exists.
-- It contains no approval decision or evidence fact and disappears once the outbox owns delivery.
-- deployment_id has no FOREIGN KEY: Aurora DSQL rejects REFERENCES outright, and every writer
-- (`automatic_approval_handoff` and `block_approval_execution` in
-- `hive_persistence::deployment::approval`, plus the outbox dead-letter handler in
-- `hive_persistence::deployment::worker`) only inserts or deletes a row keyed by a deployment id it
-- read or locked earlier in the same transaction.
CREATE TABLE IF NOT EXISTS deployment_approval_handoff_releases
(
    deployment_id UUID PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
CREATE INDEX IF NOT EXISTS deployment_approval_handoff_releases_ordered
    ON deployment_approval_handoff_releases (created_at ASC, deployment_id ASC);
-- deployment_approval_requirements rows are never deleted, by convention rather than by constraint:
-- Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule), so nothing in the
-- schema blocks a DELETE. `hive_persistence::deployment::approval::transition_requirement` is the only
-- writer that changes an existing row, and it only UPDATEs.

-- approval_requirement_id/actor_principal_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::deployment::mutations::record_approval_decision` confirms the requirement exists
-- via a FOR UPDATE read (`raw_requirement`) earlier in the same transaction, and actor_principal_id is
-- the deciding principal, guaranteed to exist transitively: every capability check gating a decision
-- resolves through an organization_membership/project_membership/platform_role_assignment row, and
-- nothing ever deletes a principal.
CREATE TABLE IF NOT EXISTS deployment_approval_decisions
(
    id
    UUID
    PRIMARY
    KEY,
    approval_requirement_id
    UUID
    NOT
    NULL,
    actor_principal_id UUID NOT NULL,
    decision TEXT NOT NULL CHECK
(
    decision
    IN
(
    'APPROVE',
    'REJECT'
)),
    comment TEXT NULL CHECK
(
    comment
    IS
    NULL
    OR
    length (
    btrim
(
    comment
)) BETWEEN 1 AND 2000
    AND (comment IS NULL OR btrim(comment) IN ('REVIEWED_CHANGE_SCOPE', 'AUTHORIZATION_GRANTED',
      'UNACCEPTABLE_CHANGE_SCOPE', 'CHANGE_SCOPE_NOT_APPROVED'))
    AND comment !~* '[A-Za-z0-9_.-]*(api[_ .-]?key|password|passwd|secret|token|credential|authorization|bearer|private[_ .-]?key|aws[_ .-]?secret[_ .-]?access[_ .-]?key)[A-Za-z0-9_.-]*[[:space:]]*(=|:)[[:space:]]*[^[:space:]]+'
    AND comment !~* '\b(sk|pk)_(live|test)_[A-Za-z0-9_]+\b|\bAKIA[0-9A-Z]{16}\b|eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}|(ghp|github_pat|xox[baprs]|ya29)[_.-][A-Za-z0-9_-]+|-----BEGIN [A-Z ]+PRIVATE KEY-----|[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}|(^|[^[:alnum:]_])[+]?[0-9][0-9 .()-]{7,}[0-9]([^[:alnum:]_]|$)|\b[0-9]{1,5}[[:space:]]+[A-Z][A-Za-z .''-]{2,}[[:space:]]+(street|st|avenue|ave|road|rd|boulevard|blvd|lane|ln)\b'),
    rejection_reason TEXT NULL CHECK
(
    rejection_reason
    IS
    NULL
    OR
    length (
    btrim
(
    rejection_reason
)) BETWEEN 1 AND 2000
    AND (rejection_reason IS NULL OR btrim(rejection_reason) IN ('REVIEWED_CHANGE_SCOPE', 'AUTHORIZATION_GRANTED',
      'UNACCEPTABLE_CHANGE_SCOPE', 'CHANGE_SCOPE_NOT_APPROVED'))
    AND rejection_reason !~* '[A-Za-z0-9_.-]*(api[_ .-]?key|password|passwd|secret|token|credential|authorization|bearer|private[_ .-]?key|aws[_ .-]?secret[_ .-]?access[_ .-]?key)[A-Za-z0-9_.-]*[[:space:]]*(=|:)[[:space:]]*[^[:space:]]+'
    AND rejection_reason !~* '\b(sk|pk)_(live|test)_[A-Za-z0-9_]+\b|\bAKIA[0-9A-Z]{16}\b|eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}|(ghp|github_pat|xox[baprs]|ya29)[_.-][A-Za-z0-9_-]+|-----BEGIN [A-Z ]+PRIVATE KEY-----|[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}|(^|[^[:alnum:]_])[+]?[0-9][0-9 .()-]{7,}[0-9]([^[:alnum:]_]|$)|\b[0-9]{1,5}[[:space:]]+[A-Z][A-Za-z .''-]{2,}[[:space:]]+(street|st|avenue|ave|road|rd|boulevard|blvd|lane|ln)\b'),
    eligibility_checked_at TIMESTAMPTZ NOT NULL,
    decided_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK
(
(
    decision =
    'REJECT'
) =
(
    rejection_reason
    IS
    NOT
    NULL
    AND
    length (
    btrim
(
    rejection_reason
)) > 0)),
    CHECK
(
    decision
    <>
    'REJECT'
    OR
    comment
    IS
    NULL
),
    CHECK
(
    decision
    <>
    'APPROVE'
    OR
    rejection_reason
    IS
    NULL
),
    UNIQUE
(
    approval_requirement_id,
    actor_principal_id
)
    );
CREATE INDEX IF NOT EXISTS deployment_approval_decisions_requirement_time
    ON deployment_approval_decisions (approval_requirement_id, decided_at, id);
-- deployment_approval_decisions is append-only by convention, not by constraint: Aurora DSQL rejects
-- CREATE RULE outright, so nothing in the schema blocks an UPDATE or DELETE.
-- `insert_decision` in `hive_persistence::deployment::queries` is this table's only writer, a single
-- unconditional INSERT.

-- Failure or revocation facts append here without rewriting captured evidence.
-- evidence_snapshot_id has no FOREIGN KEY: Aurora DSQL does not support them. Nothing writes this
-- table at all: `hive_persistence::deployment::approval` and the evidence-status queries read it, and
-- no code path or migration inserts a row, so no application-side check replaces the constraint.
CREATE TABLE IF NOT EXISTS deployment_evidence_invalidations
(
    id
    UUID
    PRIMARY
    KEY,
    evidence_snapshot_id
    UUID
    NOT
    NULL,
    kind TEXT NOT NULL CHECK
(
    kind
    IN
(
    'REVOKED',
    'FAILED'
)),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE
(
    evidence_snapshot_id,
    kind
)
    );
CREATE INDEX IF NOT EXISTS deployment_evidence_invalidations_snapshot
    ON deployment_evidence_invalidations (evidence_snapshot_id);
-- Nothing writes deployment_evidence_invalidations, and Aurora DSQL rejects CREATE RULE outright, so
-- the table has no immutability guard and no operation to guard.

-- The approval authorization layer is not SQL: Aurora DSQL rejects CREATE FUNCTION outright, even
-- LANGUAGE sql ones. `hive_persistence::capability` decides a principal's approval capabilities
-- (`authority_assignments`, `deployment_approval_capabilities`, `is_platform_administrator`), and
-- `hive_persistence::authority` supplies the matching row-visibility condition for each approval
-- table. Approval inbox discovery generates candidates and rechecks each one against that same
-- capability, so the scope caches below never widen visibility on their own.

-- The scope caches below are maintained by application code, not by triggers on the membership
-- tables: Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright. `refresh_membership_scope` and
-- `refresh_role_scope` in `hive_persistence::administration::scopes` (with their
-- `refresh_organization_scope`/`refresh_project_scope`/`refresh_organization_membership_scope`/
-- `refresh_organization_project_scopes` helpers) run in the same write paths that change
-- organization_memberships, organization_membership_roles, project_memberships, and
-- project_membership_roles. A membership write that skips them leaves the cache stale. No lock guards
-- a refresh: two overlapping refreshes for the same (principal, organization) key conflict cleanly at
-- commit with SQLSTATE 40001 under DSQL's whole-write-set validation, and refreshes for different
-- principals never touch a common row.

-- Requirement state advances only through
-- `hive_persistence::deployment::approval::transition_requirement`, and no trigger enforces that. Its
-- UPDATE never touches the frozen columns (organization_id/project_id/requested_at/
-- required_approvers/deployment_id/expires_at/created_at), always sets revision = revision + 1, and
-- requires status = 'PENDING' on an unexpired requirement; an unmet condition reports zero rows
-- affected, which the caller turns into a refusal. The target status is always one of the four
-- terminal literals SATISFIED/REJECTED/EXPIRED/INVALIDATED, never a caller-supplied value.

ALTER TABLE deployment_audit_events DROP CONSTRAINT IF EXISTS deployment_audit_events_action_check;
ALTER TABLE deployment_audit_events
    ADD CONSTRAINT deployment_audit_events_action_check CHECK (
    action IN ('REQUESTED', 'CANCELED', 'EXECUTION_STARTED', 'EXECUTION_SUCCEEDED', 'EXECUTION_FAILED', 'OUTBOX_DEAD_LETTERED', 'OUTBOX_LEASE_RECLAIMED',
    'APPROVAL_RECORDED', 'APPROVAL_SATISFIED', 'APPROVAL_REJECTED', 'APPROVAL_EXPIRED', 'APPROVAL_INVALIDATED', 'APPROVAL_EXECUTION_BLOCKED')
    );

-- approval_execution_compatible defaults to FALSE. A worker that can execute an approved handoff
-- writes TRUE before it claims one, and `hive_persistence::deployment::approval::worker_ready` holds
-- every handoff back until some fresh READY heartbeat carries TRUE.
-- Aurora DSQL rejects any constraint (NOT NULL, DEFAULT, or CHECK) inline on ADD COLUMN outright
-- (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN with
-- constraint not supported"). Column added bare; default and NOT NULL follow as separate statements
-- - `hive_persistence::migrator::run_add_check_constraint` rewrites that CHECK statement into Aurora
-- DSQL's required NOT VALID + VALIDATE CONSTRAINT form automatically.
ALTER TABLE deployment_worker_heartbeats
    ADD COLUMN IF NOT EXISTS approval_execution_compatible BOOLEAN;
ALTER TABLE deployment_worker_heartbeats ALTER COLUMN approval_execution_compatible SET DEFAULT FALSE;
UPDATE deployment_worker_heartbeats SET approval_execution_compatible = FALSE WHERE approval_execution_compatible IS NULL;
ALTER TABLE deployment_worker_heartbeats
    ADD CONSTRAINT deployment_worker_heartbeats_approval_exec_compat_nn CHECK (approval_execution_compatible IS NOT NULL);

-- The request compiler freezes this digest together with the risk level, so verifying the immutable
-- snapshot never reparses a canonical agent document.
ALTER TABLE deployment_policy_snapshots
    ADD COLUMN IF NOT EXISTS risk_verification_digest TEXT;
-- Nothing in the schema fills risk_verification_digest in: Aurora DSQL rejects CREATE TRIGGER and
-- CREATE FUNCTION outright. `hive_persistence::deployment::writes::insert_policy_snapshot` sets it
-- explicitly from `risk_verification_digest` in `hive_application::deployment::compiler`, which
-- digests risk and binding_digest together, so the column is never NULL by the time the INSERT runs.
-- Aurora DSQL also rejects CREATE RULE, so nothing blocks an UPDATE of deployment_policy_snapshots
-- either; that table is only ever INSERTed.

-- Aurora DSQL rejects CREATE FUNCTION outright, even LANGUAGE sql ones, so none of the approval rules
-- live in the schema. `hive_persistence::deployment::approval` holds all of them: `worker_ready` and
-- `evidence_ready` gate a handoff, `approval_execution_eligible` gates execution,
-- `approval_evidence_issue` and `waiting_for_evaluation` classify evidence, `reconcile_pending` and
-- `reconcile_expired_approval_requirements` sweep expired requirements, `block_approval_execution`
-- and `record_terminal_invalidation` close a requirement out, `ensure_requirement` creates one, and
-- `automatic_approval_handoff` releases an approved deployment to the outbox. A writer that reaches
-- these tables without going through that module enforces none of it. `reconcile_pending` takes no
-- lock: Aurora DSQL rejects advisory locks, and its optimistic concurrency control raises SQLSTATE
-- 40001 on a conflicting concurrent writer instead.

-- No table here has a trigger; Aurora DSQL rejects CREATE TRIGGER outright. The rules a trigger would
-- carry are ordering obligations on the write paths instead:
--   * Every INSERT into deployment_policy_snapshots is followed, in the same transaction, by
--     `hive_persistence::deployment::writes::insert_approval_requirement`.
--   * Every INSERT of EVALUATION_PASSED evidence is followed, in the same transaction, by
--     `hive_persistence::deployment::writes::touch_projection` and then
--     `hive_persistence::deployment::approval::automatic_approval_handoff`, in that order.
--     `hive_persistence::evaluation::worker` is the only write path that inserts that evidence kind.
--   * `automatic_approval_handoff` is the only writer of an EXECUTE_DEPLOYMENT outbox event, and
--     `compatible_approval_worker` rechecks the claimant after locking the deployment, so an
--     incompatible worker cannot deliver one.
--   * A deployment moves to IN_PROGRESS only after `active_project`, `approval_execution_eligible`,
--     and `compatible_approval_worker` all pass in the same transaction.
--   * Any transition into IN_PROGRESS/ACTIVE/FAILED/CANCELED/ROLLED_BACK invalidates a still-PENDING
--     requirement and clears its deployment_approval_handoff_releases row:
--     `hive_persistence::deployment::mutations::cancel`, `block_approval_execution`,
--     `reconcile_pending`,
--     `hive_persistence::administration::scopes::invalidate_pending_approvals_for_archived_project`,
--     and the outbox dead-letter handler each do so inline. An APPROVED transition needs no cascade.
--   * Nothing writes deployment_evidence_invalidations, so no handoff reacts to one.
-- Requirements are created at the read, decision, and worker handoff boundaries rather than by a
-- migration-time sweep, so no migration replays deployment history under the schema lock.
