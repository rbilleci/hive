-- deployment_approval_project_archive_actor_attributions, its guard/immutability triggers and
-- functions, deployment_approval_record_project_archive_actor(), and
-- deployment_approval_archive_actor_attribution_backfill_progress/_page() are removed outright, not
-- ported: this whole relation existed to attribute an archive event a retained M13 administration
-- writer recorded without the M14 trigger's session GUC (an event.actor_principal_id left NULL).
-- PostgresAdministrationRepository.recordProjectArchiveEvent() -- this rewrite's only writer of
-- deployment_approval_project_archive_events -- always supplies the caller's own required, non-null
-- actor parameter directly, so no row it ever creates has a NULL actor_principal_id for this fallback
-- to attribute. deployment_approval_record_project_archive_actor()'s own first check
-- (archive_actor IS NOT NULL THEN RETURN archive_actor) already made every one of its remaining callers
-- equivalent to reading event.actor_principal_id directly -- see reconcileProjectArchives()'s and
-- ensureRequirement()'s own comments (PostgresDeploymentRepository.java) for where each did exactly that.

-- This V032 redeclaration of deployment_approval_ensure_requirement() is removed, not ported as SQL:
-- ported to PostgresDeploymentRepository.ensureRequirement()/invalidateArchivedApprovalRequirement() --
-- see those methods' own comments.

-- This V032 redeclaration of deployment_approval_reconcile_project_archives_page() is removed, not
-- ported as SQL: ported to PostgresDeploymentRepository.reconcileProjectArchives() -- see that method's
-- own comment.

-- deployment_approval_project_role_assignment_guard_trigger (ON project_membership_roles) and
-- deployment_approval_project_membership_authority_guard_trigger (ON project_memberships), and both
-- of their functions, are removed, not ported: Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION
-- outright, and the invariant both enforce -- only a platform administrator may add, remove, or move
-- DEPLOYMENT_APPROVER -- is already structurally guaranteed by
-- PostgresAdministrationRepository.approvalRoleTransitionAllowed(), called and enforced (returning
-- forbidden() on failure) before every write that could reach either trigger's guarded condition:
-- addMembership()/replaceMembership() before their own replaceRoles() call (covering every INSERT/
-- UPDATE/DELETE of a DEPLOYMENT_APPROVER role row), and endMembership() before its own ended_at UPDATE
-- (the only Java path that ever updates project_memberships, and only that column -- project_id/
-- principal_id are never updated by any Java writer, so those two guarded columns can never change at
-- all). markApprovalRoleAssignment() already sets the identical hive.m14_approval_role_assignment_actor
-- session GUC these triggers read, immediately after each of the same three checks, matching the
-- "administration repository writes the transaction-local assertion only after its P-10 check"
-- comment above -- pre-existing Java infrastructure that already anticipated this removal.

-- This V032 redeclaration of effective_deployment_approval_capabilities() (project-key-first EXISTS
-- clauses, avoiding the V017 relation's per-principal materialization) is removed, not ported as SQL:
-- confirmed identical in body to V017's original declaration (see V017's own removal comment) -- ported
-- to PostgresEffectiveCapabilityEvaluator.deploymentApprovalCapabilities(), which computes the same
-- platform_administrator/organization_view/project_view/project_decide facts from
-- authorityAssignments() plus isPlatformAdministrator() instead of re-querying role assignments per call.

-- This V032 redeclaration of deployment_approval_visible_requirements() (adds a fifth,
-- capability-recheck-only branch over deployment_approval_principal_project_scopes, drops the JOIN
-- LATERAL final recheck V019 added) is removed here too, not ported as SQL: V034 holds the true final
-- redefinition (confirmed by grepping every migration file for the last declaration), and the Java port
-- lives at PostgresDeploymentRepository.approvalInboxRequirementIds() -- see that method's comment and
-- V034's own removal comment for the full reasoning.
