-- V027 orders archive boundaries by the locked project lifecycle revision. Retained timestamp-only
-- rows remain readable, while requests written after a restore cannot inherit an older archive.

ALTER TABLE deployments
    ADD COLUMN IF NOT EXISTS project_lifecycle_revision BIGINT NULL;
ALTER TABLE deployment_approval_project_archive_events
    ADD COLUMN IF NOT EXISTS archived_project_revision BIGINT NULL;

-- Both widened to full indexes: Aurora DSQL rejects partial indexes outright (0A000 WHERE not
-- supported for CREATE INDEX). Read-path performance only, not a constraint, for both.
DROP INDEX IF EXISTS deployment_approval_project_archive_events_pending;
CREATE INDEX IF NOT EXISTS deployment_approval_project_archive_events_pending
    ON deployment_approval_project_archive_events (project_id, archived_at, id);
CREATE INDEX IF NOT EXISTS deployment_approval_project_archive_events_revision
    ON deployment_approval_project_archive_events (project_id, archived_project_revision);

-- deployment_approval_archive_event_boundary() and deployment_approval_archive_boundary() hold their
-- true final declarations here (confirmed by grepping every migration file for the last declaration of
-- each) -- both removed, not ported as SQL. Both are ported to Java as inlined query fragments, not as
-- standalone reusable methods, matching this codebase's existing hasApprovalInboxScope() precedent for
-- functions only ever consulted inline within a larger query:
--   * deployment_approval_archive_boundary(candidate_deployment) ->
--     PostgresDeploymentRepository.deploymentArchiveBoundary(Connection, UUID), used via the existing
--     exists() helper.
--   * deployment_approval_archive_event_boundary(candidate_deployment, candidate_archive_event) ->
--     inlined directly into both call sites inside
--     PostgresDeploymentRepository.reconcileProjectArchives() (the candidate-fetch query and the
--     stillPending check), each as a JOIN ... ON archive_event.id = ? AND archive_event.project_id =
--     deployment.project_id with the full revision/timestamp boundary condition carried over unchanged.
-- Both Java ports keep the project_lifecycle_revision branch verbatim even though V024's removal
-- comment already establishes deployments.project_lifecycle_revision is never populated by any Java
-- writer (the FOR SHARE lock in PostgresDeploymentRepository.deploy()'s activeProject() check already
-- prevents the race this revision comparison exists to catch) -- this is a literal port, not a
-- redesign, so the presently-unreachable branch is preserved rather than pruned.

-- deployment_approval_legacy_archive_insert_guard()'s final redefinition (this file's own trigger-
-- binding site is V024's -- this is a function-body-only redeclaration) is removed, not ported: see
-- V024's removal comment.

-- deployment_approval_project_lifecycle_revision_immutable_trigger and its function are removed, not
-- ported: Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright, and
-- deployments.project_lifecycle_revision is never written by any Java path (see V024's removal
-- comment) -- with no writer, this immutability guard can never fire.

-- deployment_approval_archive_event_immutable_trigger and its function are removed, not ported: Aurora
-- DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright, and the two invariants it enforced already hold
-- by construction under this rewrite's single writer pair: PostgresAdministrationRepository.
-- recordProjectArchiveEvent() only ever INSERTs (id/project_id/actor_principal_id/archived_project_revision
-- are set once, at creation, and never touched again by any Java code), and
-- PostgresDeploymentRepository.reconcileProjectArchives() is this table's only UPDATE anywhere in this
-- codebase -- a single "SET processed_at = CURRENT_TIMESTAMP WHERE id = ?" that already only fires
-- from within reconciliation and already only ever transitions processed_at from NULL to non-NULL
-- (its own WHERE clause and event-selection query, both already gating on processed_at IS NULL,
-- structurally can't re-fire for an already-processed row), so no Java-side check needed to preserve
-- either guard this trigger enforced.
-- deployment_approval_project_archive_events_no_delete's redeclaration here is removed, not restored:
-- Aurora DSQL rejects CREATE RULE outright, and no DELETE FROM deployment_approval_project_archive_events
-- exists anywhere in this codebase -- recordProjectArchiveEvent() only INSERTs and
-- reconcileProjectArchives() only UPDATEs (see immediately above), so this table is already append-only
-- by construction.

-- deployment_approval_project_archive_requested_trigger/_requested()'s final redefinition (this
-- function-body-only redeclaration; V023 holds the trigger-binding site) is removed, not ported as SQL:
-- ported to PostgresAdministrationRepository.recordProjectArchiveEvent() -- see V023's removal comment
-- for the Java port and that method's own comment for why this later, revision-aware redefinition (not
-- V023's original) is the one it was actually ported from.

-- deployment_approval_ensure_requirement()'s V027 redefinition is removed, not ported as SQL: V032
-- redeclares it and holds its true final form -- see V017's removal comment.

-- deployment_approval_reconcile_project_archives_page()'s V027 redefinition is removed, not ported as
-- SQL: V032 redeclares it and holds its true final form -- see PostgresDeploymentRepository.
-- reconcileProjectArchives()'s own comment for the Java port.
