-- Project archive owns a terminal approval-cycle boundary. It preserves every immutable request,
-- decision, evidence, and audit fact while preventing a restored project from reviving pending work.
ALTER TABLE deployment_approval_requirements
DROP
CONSTRAINT IF EXISTS deployment_approval_requirements_invalidation_code_check;
ALTER TABLE deployment_approval_requirements
    ADD CONSTRAINT deployment_approval_requirements_invalidation_code_check CHECK (invalidation_code IS NULL OR
                                                                                   invalidation_code IN (
                                                                                                         'APPROVAL_EVIDENCE_MISSING',
                                                                                                         'APPROVAL_EVIDENCE_EXPIRED',
                                                                                                         'APPROVAL_EVIDENCE_MISMATCH',
                                                                                                         'APPROVAL_REQUIREMENT_EXPIRED',
                                                                                                         'TERMINAL_LIFECYCLE',
                                                                                                         'PROJECT_ARCHIVED'));

-- deployment_approval_project_archive_handoff() and its trigger are removed, not ported as SQL:
-- Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright. Another Deployment/approval-domain
-- trigger physically bound to one of Administration's own tables (projects, on UPDATE OF
-- lifecycle_status), for the same reason the scope-cache triggers are -- ported to Java in
-- PostgresAdministrationRepository.java's invalidatePendingApprovalsForArchivedProject(), called
-- from lifecycle() exactly when archive && "PROJECT".equals(scope), matching this trigger's own
-- OLD.lifecycle_status = 'ACTIVE' AND NEW.lifecycle_status = 'ARCHIVED' condition (lifecycle()'s
-- precondition checks already guarantee the OLD status was ACTIVE before an archive is allowed to
-- proceed). The port is literal: the same candidate selection, idempotent re-check, timeline-sequence
-- claim, audit fact, runtime-health update, and deployment lifecycle update, in the same order --
-- including deployment_approval_touch_projection()'s projection_revision bump as a second, separate
-- increment on top of the deployments UPDATE's own, which this trigger already did. No approval
-- advisory lock to preserve or replace: the trigger's own comment explains it deliberately never took
-- one, to avoid inverting the advisory -> deployment -> project lock order decision transactions use.

-- deployment_approval_automatic_handoff()'s V022 redefinition is removed, not ported as SQL: V024
-- redeclares it and holds its true final form -- see V017's removal comment.

-- deployment_approval_compatible_handoff_candidates() is removed here, not ported as SQL: see V017's
-- removal comment for the Java port.
