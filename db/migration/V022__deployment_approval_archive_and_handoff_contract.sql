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

-- Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, so archiving a project does not
-- cascade into the approval domain inside the database. `invalidate_pending_approvals_for_archived_project`
-- does that work instead, called from `lifecycle` when a PROJECT scope moves from ACTIVE to ARCHIVED:
-- candidate selection, an idempotent re-check, a timeline-sequence claim, an audit fact, a
-- runtime-health update, and the deployment lifecycle update, followed by `touch_projection` as a
-- second projection_revision increment on top of the deployments UPDATE's own.

-- Approval handoff is application code for the same reason: `automatic_approval_handoff` and
-- `compatible_approval_handoff_deployments`.
