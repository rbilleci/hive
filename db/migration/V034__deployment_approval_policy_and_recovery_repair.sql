-- V034 enforces the evaluation-only expiry projection and routes every approval-inbox
-- authorization crossing through the canonical P-10 capability relation.

-- Harmless no-op: deployment_policy_snapshots_no_update is never created (Aurora DSQL rejects CREATE
-- RULE outright; see V014's comment), so there is nothing here for this DROP to find.
-- deployment_policy_snapshot_evaluation_expiry() and its trigger are removed, not ported: Aurora DSQL
-- rejects CREATE TRIGGER/CREATE FUNCTION outright, and this BEFORE INSERT normalization can never
-- change what insertPolicySnapshot() writes. DeploymentCompiler already computes
-- evaluationRequirementExpiresAt() as null whenever EVALUATION_PASSED is not in the required-evidence
-- set (the identical condition this trigger checked), so insertPolicySnapshot()'s explicit
-- evaluation_requirement_expires_at value already matches what the trigger would have forced it to.
-- deployment_policy_snapshots_no_update is not restored here either, for the same reason.

-- deployment_approval_read_ready()'s final redefinition -- removed, not ported: Aurora DSQL rejects
-- CREATE FUNCTION outright, and every Java caller (the former approvalReadReady()) is removed along
-- with it -- see PostgresDeploymentRepository.reconcileApprovalUpgrade()'s comment for the shared
-- reasoning on why every progress table this predicate joined is itself dead on a greenfield database.

CREATE INDEX IF NOT EXISTS projects_organization_approval_scope
    ON projects (organization_id, id);

-- A recovery state transition enqueues its immutable audit obligation in the same transaction.
-- Workers retry only this durable relation after an audit-store fault, including after restart.
-- outbox_event_id and deployment_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- enqueueFailedDeliveryAudit() is only ever called with an event/deployment already loaded in the same
-- transaction as the outbox delivery failure it's recording, so removing the constraints needs no new
-- Java-side check.
CREATE TABLE IF NOT EXISTS deployment_outbox_delivery_audit_repairs
(
    outbox_event_id
    UUID
    NOT
    NULL,
    delivery_attempt INTEGER NOT NULL CHECK
(
    delivery_attempt >
    0
),
    deployment_id UUID NOT NULL,
    action TEXT NOT NULL CHECK
(
    action
    IN
(
    'OUTBOX_DELIVERY_RETRIED',
    'OUTBOX_DEAD_LETTERED'
)),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    recorded_at TIMESTAMPTZ NULL,
    PRIMARY KEY
(
    outbox_event_id,
    delivery_attempt,
    action
)
    );
-- Widened to a full index: Aurora DSQL rejects CREATE INDEX ... WHERE outright.
CREATE INDEX IF NOT EXISTS deployment_outbox_delivery_audit_repairs_pending
    ON deployment_outbox_delivery_audit_repairs (recorded_at, created_at, outbox_event_id, delivery_attempt);
-- deployment_outbox_delivery_audit_repair_transition() and its trigger are removed, not ported: Aurora
-- DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright, and repairPendingDeliveryAudits()'s own
-- conditional UPDATE (`SET recorded_at = CURRENT_TIMESTAMP WHERE ... AND recorded_at IS NULL`, the only
-- UPDATE against this table) already structurally can't violate what this trigger guarded: it never
-- touches outbox_event_id/delivery_attempt/deployment_id/action/created_at, and its own WHERE clause
-- already rejects a row that's already recorded -- see that method's own comment on why it needs no
-- FOR UPDATE either.
-- Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule); see V040's comment
-- for the parallel reasoning on the trigger-based audit guards it removed. Nothing in this codebase
-- ever DELETEs from this table.

-- deployment_approval_has_inbox_scope()'s final redefinition is removed here, not ported as SQL:
-- Aurora DSQL rejects CREATE FUNCTION outright. Ported to
-- PostgresDeploymentRepository.hasApprovalInboxScope() -- a single LANGUAGE sql statement (not
-- plpgsql), so the Java port is the identical query, wrapped in a "params" CTE binding
-- authority_principal/scoped_organization once instead of repeating positional JDBC binds at each of
-- their original references. deployment_approval_is_platform_administrator()/
-- effective_deployment_approval_capabilities() are no longer kept as SQL either (see V017's and this
-- file's own removal comments below) -- hasApprovalInboxScope() now calls
-- capabilities.isPlatformAdministrator()/capabilities.deploymentApprovalCapabilities() directly,
-- once per candidate project returned by its own query, matching the original's per-candidate calls.

-- deployment_approval_visible_requirements() holds its true final declaration here (confirmed by
-- grepping every migration file for the last declaration -- V017/V018/V019/V032 each redeclared it
-- earlier and are already removed, citing this file). Removed, not ported as SQL: Aurora DSQL rejects
-- CREATE FUNCTION outright. Ported to PostgresDeploymentRepository.approvalInboxRequirementIds().
-- That port is not a direct transliteration of this body: every branch below carries the identical
-- capability recheck (EXISTS(effective_deployment_approval_capabilities(...) WHERE = VIEW)) applied
-- INSIDE the branch, before that branch's own "LIMIT (SELECT rows FROM limits)" -- so a row failing
-- the recheck never consumes one of that branch's limited slots. The Java port instead fetches first
-- and rechecks once afterward over the union of all six branches, which changes results only for a
-- branch where the recheck can actually reject a row reaching it. That is true for the three
-- scope-cache-backed branches (deployment_approval_principal_organization_scopes;
-- deployment_approval_principal_organization_membership_scopes joined to
-- deployment_approval_principal_project_scopes; deployment_approval_principal_project_scopes alone),
-- since each proves only that a maintenance-job-populated cache row is present and unexpired, not that
-- the role grant behind it is still active -- so the Java port fetches those three unlimited rather
-- than reproducing a per-branch LIMIT that could silently crowd out a still-viewable requirement
-- behind a stale cache row (see approvalInboxRequirementIds()'s own comment for the full reasoning).
-- It is NOT true for the platform-admin branch (its WHERE already requires platform.administrator,
-- which alone satisfies effective_deployment_approval_capabilities()'s VIEW grant) or for the two
-- direct-membership fallback branches immediately below it (organization_memberships and
-- project_memberships respectively; each WHERE clause already requires the identical active-
-- membership-plus-role-code condition the recheck's organization_view/project_view branches check) --
-- for those three the recheck is tautologically true given the branch's own WHERE clause, so the Java
-- port keeps a per-branch LIMIT for them, identical to this declaration's shape.
