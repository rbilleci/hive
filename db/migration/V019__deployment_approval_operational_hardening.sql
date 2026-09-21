-- V019 applies operational repairs to a database that has already recorded V017 and V018. Every
-- retained projection stays derived; immutable deployment, requirement, decision, evidence, audit, and
-- outbox facts are preserved.

-- principal_id/organization_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::administration::scopes::refresh_organization_membership_scope` only ever writes a
-- (principal, organization) pair passed to it from `add_membership` or `end_membership`, both of which
-- confirm the principal and organization exist earlier in the same transaction; those checks are the
-- only referential guards.
CREATE TABLE IF NOT EXISTS deployment_approval_principal_organization_membership_scopes
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
CREATE INDEX IF NOT EXISTS deployment_approval_principal_organization_membership_scopes_organization
    ON deployment_approval_principal_organization_membership_scopes (organization_id, principal_id);

-- No trigger maintains this scope cache; Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION
-- outright. `hive_persistence::administration::scopes::refresh_organization_membership_scope`, called
-- from `refresh_membership_scope`, is its only writer, and a membership write that skips it leaves
-- the cache stale. See V017's comment for the rest of the scope caches.
