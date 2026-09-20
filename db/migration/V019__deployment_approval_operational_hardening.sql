-- V019 applies operational M14 repairs to databases that have already recorded V017/V018.
-- Every retained projection remains derived; immutable deployment, requirement, decision,
-- evidence, audit, and outbox facts are preserved.

-- principal_id/organization_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- refreshOrganizationMembershipScope() (PostgresAdministrationRepository.java) only ever writes a
-- (principal, organization) pair it was called with from addMembership()/endMembership(), both of
-- which already confirm the principal/organization exist earlier in the same transaction (addMembership's
-- principals/table(scope) checks; endMembership's membership() lookup). Removing the constraints
-- needs no new Java-side check.
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

-- deployment_approval_membership_scope_upgrade_progress and deployment_approval_upgrade_membership_scopes_page()
-- below are removed outright, not ported: see PostgresDeploymentRepository.reconcileApprovalUpgrade()'s
-- comment for the shared reasoning -- both existed to catch up organization_memberships rows a live
-- database could have accumulated before this migration ran, which cannot happen in this greenfield
-- rewrite.

-- This V019 introduces deployment_approval_principal_organization_membership_scopes and its own
-- refresh function/trigger redeclarations; all removed the same way as V017/V018's -- see V017's
-- removal comment for the Java port (PostgresAdministrationRepository.java's
-- refreshOrganizationMembershipScope(), called from refreshMembershipScope()).

-- deployment_approval_upgrade_membership_scopes_page() and this file's redeclaration of
-- deployment_approval_read_ready() are both removed, not ported: see this file's own comment above and
-- PostgresDeploymentRepository.reconcileApprovalUpgrade()'s comment for the shared reasoning.
-- V019 redeclares deployment_approval_visible_requirements() (adds a direct-project-membership
-- fallback branch and a JOIN LATERAL final recheck over every candidate) -- removed here too, not
-- ported as SQL: V034 holds the true final redefinition (confirmed by grepping every migration file
-- for the last declaration), and the Java port lives at
-- PostgresDeploymentRepository.approvalInboxRequirementIds() -- see that method's comment and V034's
-- own removal comment for the full reasoning.

-- deployment_approval_automatic_handoff()'s V019 redefinition is removed, not ported as SQL: V022/V024
-- redeclare it and V024 holds its true final form -- see V017's removal comment.

-- deployment_approval_outbox_claim_gate_trigger (this file's own trigger-binding site, superseding
-- V017's) and its function are removed, not ported: see V017's removal comment.
