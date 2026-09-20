-- Project administration records an immutable archive boundary. Deployment-owned maintenance
-- consumes that event and terminalizes only pending approval cycles in separate deployment work.
-- project_id/actor_principal_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- PostgresAdministrationRepository.recordProjectArchiveEvent() is this table's only writer, called from
-- lifecycle() immediately after the projects row it reads project_id from is updated in the same
-- transaction, with actor_principal_id the caller's own required, non-null actor parameter.
CREATE TABLE IF NOT EXISTS deployment_approval_project_archive_events
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL,
    actor_principal_id UUID NULL,
    archived_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    processed_at TIMESTAMPTZ NULL
    );
-- deployment_approval_project_archive_events_pending's original `UNIQUE (project_id) WHERE
-- processed_at IS NULL` is removed, not widened: Aurora DSQL rejects partial indexes outright, and
-- V027 (DROP INDEX IF EXISTS .../CREATE INDEX ... under this same name) unconditionally replaces it
-- with a plain (non-unique) index moments later in the same migration sequence, before any Java code
-- (confirmed: no reference to this index name anywhere in service/src/main/java) ever depends on the
-- interim uniqueness. Declaring it correctly here would be dead work, not a shortcut.
-- Widened to a full index, same reason (0A000 WHERE not supported for CREATE INDEX).
CREATE INDEX IF NOT EXISTS deployment_approval_project_archive_events_delivery
    ON deployment_approval_project_archive_events (archived_at, id);

-- deployment_approval_project_archive_discovery_progress and deployment_approval_project_archive_discovery_page()
-- are removed outright, not ported: see PostgresDeploymentRepository.reconcileApprovalUpgrade()'s
-- comment for the shared reasoning -- both existed to discover a project already ARCHIVED by a
-- predecessor writer that predates this trigger, which cannot happen in this greenfield rewrite, where
-- PostgresAdministrationRepository.lifecycle() is the only writer of projects.lifecycle_status and
-- always records an archive event itself (see below).
--
-- deployment_approval_project_archive_requested_trigger/_requested() are removed, not left as SQL:
-- ported to PostgresAdministrationRepository.recordProjectArchiveEvent(), called directly from
-- lifecycle() -- see that method's own comment for the full reasoning, including why its ON CONFLICT
-- clause was dropped rather than carried over unchanged.
--
-- deployment_approval_project_archive_pending() is removed, not ported as SQL: already a full Java
-- port, PostgresDeploymentRepository.approvalProjectArchivePending().
--
-- This V023 redeclaration of deployment_approval_reconcile_project_archives_page() is removed, not
-- ported as SQL: ported to PostgresDeploymentRepository.reconcileProjectArchives(), from V032's later,
-- final redefinition, not this one -- see that method's own comment.

-- deployment_approval_execution_commit_eligible() and deployment_approval_execution_eligible() are
-- removed here, not ported as SQL: see V020's/V017's removal comments.
