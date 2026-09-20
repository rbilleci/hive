-- M14 records immutable local approval decisions separately from the frozen request, policy, plan,
-- and evidence facts written by M13. Requirement state may advance once; decision and invalidation
-- records never change or disappear.
-- deployment_approval_review_text_safe() has no FOREIGN KEY-adjacent role: Aurora DSQL rejects
-- CREATE FUNCTION outright, including this LANGUAGE sql IMMUTABLE helper, which was referenced only
-- from two CHECK constraints below (never called from Java). Its one-line body is inlined into both
-- constraints verbatim instead of ported to Java, since CHECK constraints are fully DSQL-compatible
-- and inlining changes no enforcement point or behavior, only where the expression's text lives.
--
-- deployment_id/organization_id/project_id have no FOREIGN KEY either: Aurora DSQL does not support
-- them. insertApprovalRequirement() (PostgresDeploymentRepository.java) INSERTs organization_id/
-- project_id from a correlated SELECT against the just-created deployments row in the same
-- statement (INSERT ... SELECT ... FROM deployments WHERE deployment.id = ?), not from caller-
-- supplied values, so both columns are guaranteed consistent with that row by construction; if the
-- row were somehow missing, the SELECT returns zero rows and the follow-up existence check throws
-- SQLSTATE 23503, the same code a real FK violation would have used. Removing the constraints needs
-- no new Java-side check.
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
    -- reproduced the same way for uuid[]). See PostgresDeploymentRepository's
    -- rawRequirementRow()/transitionRequirement() for the Java-side (de)serialization this requires.
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
-- WHERE clause below this point in this file is widened to a full index for that reason alone - each
-- is a read-path performance index, not a constraint, so indexing rows the query's own WHERE clause
-- already excludes changes nothing but index size.
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_pending
    ON deployment_approval_requirements (status, expires_at, id);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_expiry
    ON deployment_approval_requirements (expires_at, id);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_satisfied_handoff
    ON deployment_approval_requirements (deployment_id);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_deployment_requested
    ON deployment_approval_requirements (deployment_id, id);
-- These indexes cover the immutable M14 requirement relation, which starts empty at V017. They
-- avoid a migration-time index build over retained M13 deployment history.
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_global_inbox
    ON deployment_approval_requirements (requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_organization_inbox
    ON deployment_approval_requirements (organization_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_project_inbox
    ON deployment_approval_requirements (project_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_zero_handoff
    ON deployment_approval_requirements (requested_at ASC, id ASC);
-- deployment_approval_touch_projection()'s final redefinition (V018) is removed, not ported as SQL:
-- Aurora DSQL rejects CREATE FUNCTION outright. Ported to PostgresDeploymentRepository.touchProjection()
-- -- every one of this domain step's Java call sites already uses that method directly instead of a raw
-- SQL PERFORM, so the SQL declaration has no remaining caller once the functions that used to PERFORM
-- it (worker_ready/evidence_ready/reconcile_pending/block_invalid_handoff/ensure_requirement/
-- automatic_handoff, all below) are themselves removed for the same reason -- see each removal's own
-- comment.
-- Inbox discovery uses compact authority scopes. Organization roles retain one organization row,
-- and project roles retain one project row. Request creation and project creation therefore never
-- expand an authority grant across every project in an organization.
-- principal_id/organization_id/project_id have no FOREIGN KEY on either table below: Aurora DSQL does
-- not support them. refreshOrganizationScope()/refreshProjectScope() (PostgresAdministrationRepository
-- .java) only ever write a (principal, organization/project) pair resolved from a live
-- organization_memberships/project_memberships row in the same query, so both are guaranteed
-- consistent with real membership state by construction; removing the constraints needs no new
-- Java-side check.
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
-- This transient queue records only an execution handoff delayed for an M14-compatible worker.
-- It contains no approval decision or evidence fact and can disappear once the outbox owns delivery.
-- The FK on deployment_id is removed, not ported: Aurora DSQL rejects REFERENCES outright, and every
-- writer (automaticApprovalHandoff(), blockApprovalExecution(), the outbox dead-letter handler) only
-- ever inserts or deletes a row keyed by a deploymentId it already read or locked earlier in the same
-- transaction.
CREATE TABLE IF NOT EXISTS deployment_approval_handoff_releases
(
    deployment_id UUID PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
CREATE INDEX IF NOT EXISTS deployment_approval_handoff_releases_ordered
    ON deployment_approval_handoff_releases (created_at ASC, deployment_id ASC);
-- deployment_approval_compatibility_progress is removed outright, not ported, along with the
-- deployment_approval_compatibility_backfill_page() function and deployment_approval_read_ready()
-- machinery below that depended on it: this "singleton checkpoint" existed to walk deployments a live
-- database could have accumulated before this migration ran into compliance -- structurally impossible
-- in this greenfield rewrite, where every deployment is written, from the start, by the write path this
-- migration already establishes. See PostgresDeploymentRepository.reconcileApprovalUpgrade()'s own
-- comment for the full reasoning shared by every "_progress"/"_backfill_page"/"_upgrade_*" object this
-- domain step removes for the same reason.
-- Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule); see V040's comment
-- for the parallel reasoning on the trigger-based audit guards it removed. No DELETE FROM
-- deployment_approval_requirements exists anywhere in this codebase -- transitionRequirement() only
-- ever UPDATEs it.

-- approval_requirement_id/actor_principal_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- recordApprovalDecision() (PostgresDeploymentRepository.java) confirms the requirement exists via
-- rawRequirement(..., true) FOR UPDATE earlier in the same transaction before this INSERT, and
-- actor_principal_id is the deciding principal, whose existence is guaranteed transitively the same
-- way every other domain step in this rewrite established: every capability check gating a decision
-- resolves through an organization_membership/project_membership/platform_role_assignment row, and
-- nothing in this codebase ever deletes a principal. Removing the constraints needs no new
-- Java-side check.
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
-- deployment_approval_decisions_no_update/_no_delete are removed, not ported: Aurora DSQL rejects
-- CREATE RULE outright, and insertDecision() (PostgresDeploymentRepository.java) is this table's only
-- writer -- a single, unconditional INSERT, never an UPDATE or DELETE.

-- Later local evaluation behavior can append failure or revocation facts without rewriting captured evidence.
-- evidence_snapshot_id has no FOREIGN KEY: Aurora DSQL does not support them. No INSERT INTO
-- deployment_evidence_invalidations exists anywhere in this codebase (grep confirms zero hits across
-- both service/src/main/java and every migration file) -- this table is read (by the evidence-issue
-- classification in PostgresDeploymentApprovalEvidenceIssue.java and the evidence-status query above)
-- but never written, so removing the constraint needs no new Java-side check.
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
-- Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule); see V040's comment
-- for the parallel reasoning on the trigger-based audit guards it removed. Nothing writes to this
-- table at all currently (see above), so there is no application-level operation to guard either way.

-- The P-10 authorization/capability layer this domain step declared
-- (deployment_approval_is_platform_administrator, effective_deployment_role_assignments,
-- effective_project_deployment_roles, deployment_approval_authority_assignments,
-- deployment_approval_project_capabilities,
-- effective_deployment_approval_capabilities -- all LANGUAGE sql) is removed, not ported as SQL: Aurora
-- DSQL rejects CREATE FUNCTION outright, even LANGUAGE sql ones. Live callers ported to Java in
-- PostgresEffectiveCapabilityEvaluator.java (authorityAssignments(), combining the first four into one
-- query since effective_deployment_role_assignments had no other caller, plus
-- deploymentApprovalCapabilities(), porting effective_deployment_approval_capabilities()'s final form --
-- V032 only redeclares it identically) and PostgresDeploymentRepository.java (deployment_approval_
-- visible_requirements(), the sole caller of deployment_approval_authority_assignments() beyond the
-- functions removed alongside it). effective_project_deployment_roles() and deployment_approval_project_
-- capabilities() had no live caller anywhere in the schema or this codebase (confirmed by grep) and are
-- deleted outright: the former was already fully superseded by PostgresEffectiveCapabilityEvaluator.
-- deploymentCapabilities() (ported from V037's effective_deployment_capabilities(), a separate,
-- already-removed function), the latter never had one.

-- deployment_approval_refresh_organization_scope()/_refresh_project_scope()/
-- _refresh_organization_project_scopes(), the two deployment_approval_scope_organization_anchor(s)
-- lock functions, and the four organization_memberships/organization_membership_roles/
-- project_memberships/project_membership_roles trigger bindings that called them are removed, not
-- ported as SQL: Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright. Ported to Java instead,
-- landed in PostgresAdministrationRepository.java (refreshMembershipScope()/refreshRoleScope() and
-- their four refreshOrganizationScope()/refreshProjectScope()/refreshOrganizationMembershipScope()/
-- refreshOrganizationProjectScopes() helpers) rather than PostgresDeploymentRepository.java, since
-- these triggers are Deployment/approval-domain logic bound to Administration's own tables -- the
-- same cross-domain shape the Evaluation domain step resolved by landing its derived-projection port
-- in the source domains' write paths. The m14-approval-scope-organization advisory lock is deleted
-- outright, not replaced: confirmed empirically against the real hive-dsql-verification cluster (a
-- throwaway probe matching this exact DELETE-then-INSERT shape) that two overlapping refreshes for
-- the same (principal, organization) key still conflict cleanly at commit (SQLSTATE 40001) with no
-- lock at all, and two refreshes for different principals never touch a common row in the first
-- place -- the same whole-write-set OCC validation already proven for m14-approval-transition.

-- V018 owns retained-authority discovery backfill in resumable pages. New scope writes already
-- flow through the triggers above, so startup does not scan every retained membership under the
-- schema migration lock.

-- This V017 declaration of deployment_approval_visible_requirements() (3-branch, sharing one
-- authority_scopes CTE built from deployment_approval_authority_assignments()) is removed, not ported
-- as SQL: it is superseded before this domain-closure step even began -- V018/V019/V032/V034 each
-- redeclare it, and V034 holds the true final, structurally different (6-branch, no shared
-- authority_scopes CTE, one inline capability recheck per branch) redefinition (confirmed by grepping
-- every migration file for the last declaration). The Java port lives at
-- PostgresDeploymentRepository.approvalInboxRequirementIds(), built from V034's body, not this one --
-- see that method's comment and V034's own removal comment for the full reasoning.

-- deployment_approval_requirement_transition() and its trigger are removed, not ported as SQL: Aurora
-- DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright. Every guard this BEFORE UPDATE trigger
-- enforced (its final form is V033's redefinition, not this one -- see that migration's comment for
-- the port) is already structurally guaranteed by transitionRequirement()
-- (PostgresDeploymentRepository.java): its UPDATE's SET clause never touches the frozen-fact columns
-- at all (organization_id/project_id/requested_at/required_approvers/deployment_id/expires_at/
-- created_at), unconditionally sets revision = revision + 1, and its WHERE clause already requires
-- status = 'PENDING' AND (status NOT IN ('SATISFIED','REJECTED') OR expires_at > clock_timestamp())
-- -- an unmet WHERE clause reports 0 rows affected, which the caller already turns into the same
-- refusal the trigger's RAISE EXCEPTION produced. status is always one of the four terminal literals
-- ("SATISFIED"/"REJECTED"/"EXPIRED"/"INVALIDATED"), since transitionRequirement() is private and every
-- call site passes a compile-time literal, never a caller-supplied value.

ALTER TABLE deployment_audit_events DROP CONSTRAINT IF EXISTS deployment_audit_events_action_check;
ALTER TABLE deployment_audit_events
    ADD CONSTRAINT deployment_audit_events_action_check CHECK (
    action IN ('REQUESTED', 'CANCELED', 'EXECUTION_STARTED', 'EXECUTION_SUCCEEDED', 'EXECUTION_FAILED', 'OUTBOX_DEAD_LETTERED', 'OUTBOX_LEASE_RECLAIMED',
    'APPROVAL_RECORDED', 'APPROVAL_SATISFIED', 'APPROVAL_REJECTED', 'APPROVAL_EXPIRED', 'APPROVAL_INVALIDATED', 'APPROVAL_EXECUTION_BLOCKED')
    );

-- An M13 worker writes the legacy heartbeat shape and therefore keeps the default false value.
-- An M14 worker receives a process-unique identifier and writes true before it claims any approved
-- handoff. The database claim gate below makes that protocol boundary authoritative during rollout.
-- Aurora DSQL rejects any constraint (NOT NULL, DEFAULT, or CHECK) inline on ADD COLUMN outright
-- (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN with
-- constraint not supported"). Column added bare; default and NOT NULL follow as separate statements
-- - see DatabaseMigrator.runStatement()'s comment for how the CHECK statement reaches Aurora DSQL's
-- required NOT VALID + VALIDATE CONSTRAINT form automatically.
ALTER TABLE deployment_worker_heartbeats
    ADD COLUMN IF NOT EXISTS approval_execution_compatible BOOLEAN;
ALTER TABLE deployment_worker_heartbeats ALTER COLUMN approval_execution_compatible SET DEFAULT FALSE;
UPDATE deployment_worker_heartbeats SET approval_execution_compatible = FALSE WHERE approval_execution_compatible IS NULL;
ALTER TABLE deployment_worker_heartbeats
    ADD CONSTRAINT deployment_worker_heartbeats_approval_exec_compat_nn CHECK (approval_execution_compatible IS NOT NULL);

-- The request compiler freezes this digest with the P-05 risk. The database verifies the
-- immutable snapshot without reparsing canonical agent documents in a persistence adapter.
ALTER TABLE deployment_policy_snapshots
    ADD COLUMN IF NOT EXISTS risk_verification_digest TEXT;
-- deployment_policy_snapshot_risk_verification_digest() and its trigger are removed, not ported: Aurora
-- DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright, and this BEFORE INSERT ... IF NEW.column IS
-- NULL fill-in guard can never fire in the first place. insertPolicySnapshot() already sets
-- risk_verification_digest explicitly from DeploymentCompiler.riskVerificationDigest(risk,
-- bindingDigest) -- digest(risk + "|" + bindingDigest) -- the identical formula this trigger used, so
-- the column is never NULL by the time the INSERT runs.
--
-- deployment_policy_snapshots_no_update (this file's own original declaration site -- V014/V015/V016/
-- V034 only ever DROP or reference it, never create it) is likewise removed, not ported: Aurora DSQL
-- rejects CREATE RULE outright, and PostgresDeploymentRepository never UPDATEs deployment_policy_
-- snapshots after insertPolicySnapshot()'s INSERT -- there is no application-level operation left to
-- guard.

-- deployment_approval_worker_ready()'s only declaration (never redeclared) is removed, not ported
-- as SQL: Aurora DSQL rejects CREATE FUNCTION outright. Ported to
-- PostgresDeploymentRepository.workerReady(), called directly by automaticApprovalHandoff() instead of
-- through a raw SQL PERFORM.

-- deployment_approval_evidence_ready()'s only declaration (never redeclared) is removed, not ported as
-- SQL: Aurora DSQL rejects CREATE FUNCTION outright. Ported to
-- PostgresDeploymentRepository.evidenceReady(), called directly by every Java site this domain step
-- owns instead of through a raw SQL call.


-- deployment_approval_execution_eligible() is removed here, not ported as SQL: Aurora DSQL rejects
-- CREATE FUNCTION outright. This is its original, simplest form; V021/V023/V024 redeclare it and
-- V033 holds its true final form -- see V033's own removal comment for the Java port
-- (PostgresDeploymentRepository.approvalExecutionEligible()) and why the SQL declaration itself,
-- unlike several of this file's other now-Java-owned functions, has no remaining out-of-scope caller
-- and so is deleted outright rather than kept alive for the compatibility/backfill machinery.

-- deployment_approval_evidence_issue()'s only declaration (never redeclared) is removed, not ported
-- as SQL: Aurora DSQL rejects CREATE FUNCTION outright. Ported to
-- PostgresDeploymentApprovalEvidenceIssue's own Java port (see that class's comment), called by
-- PostgresDeploymentRepository directly instead of through a raw SQL call.

-- deployment_approval_waiting_for_evaluation()'s only declaration (never redeclared) is removed, not
-- ported as SQL: Aurora DSQL rejects CREATE FUNCTION outright. Ported alongside
-- deployment_approval_evidence_issue() in PostgresDeploymentApprovalEvidenceIssue -- see that class's
-- comment.

-- deployment_approval_reconcile_pending()'s only declaration (never redeclared) is removed, not ported
-- as SQL: Aurora DSQL rejects CREATE FUNCTION outright, including the pg_advisory_xact_lock() call this
-- form used (Aurora DSQL also rejects advisory locks). Ported to
-- PostgresDeploymentRepository.reconcilePending(), which relies on DSQL's own optimistic concurrency
-- control (SQLSTATE 40001 on a concurrent conflicting writer) instead of the advisory lock, the same
-- substitution this domain step's other m14-approval-transition ports already made.

-- deployment_approval_reconcile_expired_pending() is removed here, not ported as SQL: Aurora DSQL
-- rejects CREATE FUNCTION outright, and unlike the singular deployment_approval_reconcile_pending()
-- it wraps, this bounded-page enumerator has no caller anywhere -- SQL or Java -- to preserve; it
-- predates PostgresDeploymentRepository's own equivalent claim-page shape
-- (expiredApprovalRequirementDeployments()/reconcileApprovalRequirement()/
-- releaseApprovalMaintenanceClaim(), wired through reconcileExpiredApprovalRequirements()), which
-- already covers the maintenance sweep this function was never actually being called to perform.

-- deployment_approval_block_invalid_handoff()'s original form here is removed, not ported as SQL: V025
-- and V033 redeclare it, and V033 holds its true final form -- see that migration's own removal comment
-- for the Java port (PostgresDeploymentRepository.blockApprovalExecution()) and why the SQL declaration
-- itself is deleted outright rather than kept alive for the compatibility/backfill machinery, now that
-- every SQL caller (deployment_approval_reconcile_pending()/_ensure_requirement()/_automatic_handoff(),
-- all removed alongside it) is gone too.

-- deployment_approval_record_terminal_invalidation()'s original form here is removed, not ported as
-- SQL: V018 redeclares it and holds its true final form -- see that migration's own removal comment for
-- the Java port (PostgresDeploymentRepository.recordTerminalInvalidation()).

-- deployment_approval_ensure_requirement()'s original form here is removed, not ported as SQL: V018/
-- V024/V027/V032 redeclare it and V032 holds its true final form -- see that migration's own removal
-- comment for the Java port (PostgresDeploymentRepository.ensureRequirement()).


-- deployment_approval_compatibility_backfill_page() is removed outright, not ported: its own loop
-- condition (a deployment with no deployment_approval_requirements row) can never match in this
-- greenfield rewrite, where insertApprovalRequirement() (PostgresDeploymentRepository.java) creates
-- that row synchronously for every deployment from the start -- see the removal comment on
-- deployment_approval_compatibility_progress above for the shared reasoning.

-- deployment_approval_automatic_handoff()'s original form here is removed, not ported as SQL: V019/
-- V022/V024 redeclare it and V024 holds its true final form -- see that migration's own removal comment
-- for the Java port (PostgresDeploymentRepository.automaticApprovalHandoff()).


-- deployment_approval_requirement_compatibility_trigger (AFTER INSERT ON deployment_policy_snapshots)
-- and its function are removed, not ported: Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION
-- outright, and this "an older M13 writer can still create a deployment" guard has no such writer to
-- guard against in this rewrite -- insertApprovalRequirement() (PostgresDeploymentRepository.java) is
-- the only INSERT INTO deployment_policy_snapshots's insertPolicySnapshot() caller, and both of its
-- own call sites already call insertApprovalRequirement() immediately afterward, unconditionally, in
-- the same transaction.

-- deployment_approval_evaluation_handoff_trigger (AFTER INSERT ON deployment_evidence_snapshots WHEN
-- NEW.evidence_kind = 'EVALUATION_PASSED'; V018 holds its true final form, adding an unconditional
-- deployment_approval_touch_projection() call this V017 form does not have) and its function are
-- removed, not ported as SQL: ported to Java instead, at PostgresEvaluationRepository's own INSERT
-- INTO deployment_evidence_snapshots (the one Java write path that ever inserts that evidence kind --
-- "M16 owns insertion of EVALUATION_PASSED"), which now calls
-- PostgresDeploymentRepository.touchProjection() then .automaticApprovalHandoff() immediately
-- afterward, in that order, matching V018's own two PERFORM calls.

-- deployment_approval_evidence_invalidation_handoff_trigger (AFTER INSERT ON
-- deployment_evidence_invalidations) and its function are removed, not ported: this table has no
-- writer anywhere, Java or SQL (see deployment_evidence_invalidations' own FK/RULE removal comment
-- above), so this trigger can never fire.

-- deployment_approval_compatible_handoff_candidates() is removed here, not ported as SQL: ported to
-- PostgresDeploymentRepository.compatibleApprovalHandoffDeployments(), matching the pre-existing
-- expiredApprovalRequirementDeployments()'s own claim-page shape. Unlike several of this file's other
-- now-Java-owned functions, this one has no remaining out-of-scope caller, so the SQL declaration is
-- deleted outright.

-- deployment_approval_outbox_claim_gate_trigger (BEFORE UPDATE ON deployment_outbox_events) and its
-- function are removed, not ported: PostgresDeploymentRepository.compatibleApprovalWorker() --
-- "Rechecks the exact claimant after locking the deployment so a pre-approval M13 claim cannot
-- execute it" -- already gates both call sites that could otherwise deliver an approval-affected
-- EXECUTE_DEPLOYMENT event through an incompatible worker, before any state-changing write, making
-- this trigger's revert-on-UPDATE path unreachable in the Java-only system.

-- deployment_approval_outbox_gate_trigger (BEFORE INSERT ON deployment_outbox_events) and its
-- function are removed, not ported: PostgresDeploymentRepository.automaticApprovalHandoff() (the
-- literal Java port of the only function that ever inserts an EXECUTE_DEPLOYMENT event) is now the
-- one and only writer of that event type, and its own condition for the insert is the literal,
-- faithful translation this trigger's validation existed to double-check against other, no-longer-
-- possible writers ("a legacy M13 transaction", "a V015 M13 writer").

-- deployment_approval_execution_worker_gate_trigger (BEFORE UPDATE OF lifecycle_status ON
-- deployments, guarding the REQUESTED/APPROVED -> IN_PROGRESS transition) and its function are
-- removed, not ported: PostgresDeploymentRepository's execute()-shaped worker method already checks
-- the identical three conditions -- activeProject(), approvalExecutionEligible(), and
-- compatibleApprovalWorker() -- immediately before performing this exact UPDATE, in the same
-- transaction.

-- deployment_approval_lifecycle_handoff_trigger (AFTER UPDATE OF lifecycle_status ON deployments,
-- firing on any transition into IN_PROGRESS/ACTIVE/FAILED/CANCELED/ROLLED_BACK) and its function are
-- removed, not ported as one shared trigger: every Java site that performs such a transition already
-- invalidates any still-PENDING requirement and clears deployment_approval_handoff_releases inline --
-- cancel() (invalidatePendingRequirement()), blockApprovalExecution() and reconcilePending()'s cancel
-- branches (both already porting deployment_approval_block_invalid_handoff()/_reconcile_pending()'s
-- own identical cascade), PostgresAdministrationRepository.invalidatePendingApprovalsForArchivedProject(),
-- and the outbox dead-letter handler, which this domain step's port extends with the same two calls
-- for the one terminalization site (a poisoned event reaching FAILED) that previously relied on this
-- trigger alone. approveDeploymentForExecution()'s APPROVED transition is not in this trigger's own
-- watched status list, so it never needed this cascade to begin with.

-- The migration does not replay retained deployment history while it holds its schema lock.
-- Valid legacy automatic requests continue through the bounded compatibility worker; incomplete
-- cycles remain pending and cannot run a retained execution event.
-- Compatibility requirements are created lazily by the read, decision, and worker handoff
-- boundaries. This avoids replaying retained history while either local process holds startup.
