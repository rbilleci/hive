-- V031 replaces the historical V026 startup scan for databases that already recorded V026 and
-- closes replay recovery before the approval surface becomes readable.

-- deployment_approval_archive_history_progress/_page() are removed, not ported: see
-- PostgresDeploymentRepository.reconcileApprovalUpgrade()'s comment for the shared reasoning -- this
-- V031 redeclaration retargeted the same dead backfill at deployment_approval_project_archive_events'
-- own history instead of predecessor administration_audit_events rows, but the loop condition (a
-- project-archived audit event with no corresponding archive event yet) still can never match in this
-- greenfield rewrite, where PostgresAdministrationRepository.recordProjectArchiveEvent() always writes
-- the archive event synchronously in the same transaction as the lifecycle_status update it follows.

-- deployment_approval_replay_receipt_backfill_progress/_page() are removed, not ported: same reasoning
-- -- deployment_approval_replay_receipts (V030) is written synchronously by
-- PostgresDeploymentRepository.insertDecision() for every APPROVAL_REPLAYED decision from the start, so
-- no deployment_audit_events row this page could ever scan lacks its receipt already.

-- Every decision written after this contract captures the normalized request semantics that own
-- its retry key. Legacy rows remain immutable but fail closed for a request-key replay because
-- their original expected revision was not retained.
ALTER TABLE deployment_approval_decisions
    ADD COLUMN IF NOT EXISTS request_expected_revision BIGINT NULL;
ALTER TABLE deployment_approval_decisions
    ADD COLUMN IF NOT EXISTS request_fingerprint CHAR (64) NULL;
-- deployment_approval_decision_request_fingerprint_trigger and its function are removed, not ported:
-- Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright, and insertDecision()
-- (PostgresDeploymentRepository.java) is this table's only writer -- it always supplies
-- command.expectedRevision() (a non-negative, previously-read revision counter) and
-- decisionRequestFingerprint(command) (HexFormat.of().formatHex() of a SHA-256 digest, always exactly
-- 64 lowercase hex characters by construction) on every insert.

-- deployment_approval_has_inbox_scope() is removed here, not ported as SQL: Aurora DSQL rejects
-- CREATE FUNCTION outright; V034 holds its true final form -- see that file's own removal comment for
-- the Java port (PostgresDeploymentRepository.hasApprovalInboxScope()).

-- deployment_approval_read_ready() is removed here, not ported: see V034's removal comment -- this
-- file's own copy is not that function's final redefinition.
