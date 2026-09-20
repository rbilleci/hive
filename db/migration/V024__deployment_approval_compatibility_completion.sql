-- V024 completes the rolling M13/M14 boundary. It keeps zero-approver M13 requests executable
-- in REQUESTED, serializes late legacy writes with project archive boundaries, and lets retained
-- M13 history remain readable while approval-only review facts catch up.

-- This V024 declaration of deployment_approval_archive_boundary() (timestamp-only, pre-dates the
-- revision-aware branch discussed below) is removed, not ported as SQL: V027 holds the true final
-- redefinition (adds the project_lifecycle_revision comparison), and the Java port lives at
-- PostgresDeploymentRepository.deploymentArchiveBoundary(Connection, UUID) -- see V027's own removal
-- comment for the full reasoning.

-- deployment_approval_legacy_archive_insert_guard_trigger (this file's own trigger-binding site --
-- V027 only redefines the function, matching its own later, revision-aware form) and its function are
-- removed, not ported: Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright, and the exact
-- race this FOR SHARE guard closes -- a concurrent archive landing between a deployment request's own
-- project-active precondition check and its INSERT -- is already closed by
-- PostgresDeploymentRepository.deploy()'s own activeProject(connection, projectId, true) check, which
-- takes the identical FOR SHARE lock on the same projects row before insertDeployment() runs, held for
-- the rest of that transaction. deployments.project_lifecycle_revision (V027) is consequently never
-- populated by any Java writer and stays NULL for every new deployment, which is exactly the
-- deployment_approval_archive_boundary()/_archive_event_boundary() fallback branch already designed
-- to handle (their revision comparison exists only for the race this lock now independently
-- prevents, so the timestamp comparison remains precise for every future deployment; not ported here
-- either, since it belongs to the compatibility/backfill machinery, out of scope in this domain step).

-- deployment_approval_ensure_requirement()'s V024 redefinition is removed, not ported as SQL: V027/
-- V032 redeclare it and V032 holds its true final form -- see V017's removal comment.

-- An old writer still submits its M13 outbox row after policy/evidence inserts. A retained archive
-- boundary turns that row into a no-op so the canceled history commits for audit rather than
-- rolling the old request back and losing the reason.
-- deployment_approval_outbox_gate_trigger and deployment_approval_execution_worker_gate_trigger
-- (this file's own trigger-binding sites, superseding V017's) and both of their functions are
-- removed, not ported: see V017's removal comments for each.
-- deployment_approval_automatic_handoff()'s final redefinition -- removed here, not ported as SQL: see
-- V017's removal comment for the Java port (PostgresDeploymentRepository.automaticApprovalHandoff()).

-- deployment_approval_compatible_handoff_candidates()'s final redefinition is removed here, not
-- ported as SQL: see V017's removal comment for the Java port
-- (PostgresDeploymentRepository.compatibleApprovalHandoffDeployments()).

-- This V024 redeclaration of deployment_approval_reconcile_project_archives_page() is removed, not
-- ported as SQL: see V023's removal comment -- ported from V032's later, final redefinition instead.
--
-- deployment_approval_review_backfill_failures, deployment_approval_record_review_facts(), and this
-- file's deployment_approval_review_facts_page() (the actual final redefinition of the function V021
-- also declared, not V021's) are all removed outright, not ported: see
-- PostgresDeploymentRepository.reconcileApprovalUpgrade()'s comment for the shared reasoning -- all
-- three existed to validate and upgrade deployment_plan_versions rows a live database could have
-- accumulated before this migration ran (including malformed pre-M14 JSON shapes), which cannot happen
-- in this greenfield rewrite -- insertPlanReview() always writes deployment_plan_review_facts directly
-- from known-good, Java-constructed data.

-- deployment_approval_plan_review_compatibility_trigger (this file's own trigger-binding site,
-- superseding V021's) and its function are removed, not ported: see V021's removal comment.
