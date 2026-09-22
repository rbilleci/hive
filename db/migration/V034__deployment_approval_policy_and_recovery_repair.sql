-- Aurora DSQL rejects CREATE TRIGGER, CREATE FUNCTION, and CREATE RULE outright, so nothing in the
-- database normalizes or freezes deployment_policy_snapshots. The compiler already leaves
-- evaluation_requirement_expires_at NULL whenever EVALUATION_PASSED is absent from the
-- required-evidence set, and `insert_policy_snapshot` writes that value through unchanged.

CREATE INDEX IF NOT EXISTS projects_organization_approval_scope
    ON projects (organization_id, id);

-- A recovery state transition enqueues its immutable audit obligation in the same transaction.
-- Workers retry only this durable relation after an audit-store fault, including after restart.
-- outbox_event_id and deployment_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- `enqueue_failed_delivery_audit` is only ever called with an event and deployment already loaded in
-- the same transaction as the outbox delivery failure it records, so no application-side check
-- replaces the constraints.
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
-- Aurora DSQL rejects CREATE TRIGGER, CREATE FUNCTION, and CREATE RULE (0A000 unsupported statement:
-- Rule) outright, so nothing in the database constrains how a repair row transitions.
-- `repair_pending_delivery_audits` holds the invariant instead: its conditional
-- "SET recorded_at = CURRENT_TIMESTAMP WHERE ... AND recorded_at IS NULL" is the only UPDATE against
-- this table, it never touches outbox_event_id/delivery_attempt/deployment_id/action/created_at, and
-- its WHERE clause skips an already-recorded row. Nothing DELETEs from this table.

-- Aurora DSQL rejects CREATE FUNCTION outright, so approval-inbox scope and requirement visibility
-- are computed in application code, from `is_platform_administrator` and
-- `deployment_approval_capabilities`.
--
-- A reader building such a query must know one property of the scope caches:
-- deployment_approval_principal_organization_scopes,
-- deployment_approval_principal_organization_membership_scopes, and
-- deployment_approval_principal_project_scopes are populated by a maintenance job, so a present,
-- unexpired row there proves only that the cache was written -- not that the role grant behind it is
-- still active. A candidate drawn from those caches must be rechecked against the capability
-- relation, and it must not be drawn under a per-branch LIMIT, or a stale cache row can crowd out a
-- still-viewable requirement. Candidates drawn from platform-administrator status or from an active
-- organization_memberships/project_memberships row satisfy the recheck by their own selection
-- criteria, so those may be limited.
