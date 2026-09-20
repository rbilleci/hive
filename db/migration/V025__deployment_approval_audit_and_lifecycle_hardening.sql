-- V025 preserves compiler-owned review facts, records human-decision transport correlation, and
-- closes retained archive and evidence projection boundaries.

ALTER TABLE deployment_approval_decisions
    ADD COLUMN IF NOT EXISTS correlation_id UUID NULL;

-- deployment_approval_audit_correlation_insert and its function are removed, not ported: Aurora DSQL
-- rejects CREATE TRIGGER/CREATE FUNCTION outright, and nothing in this codebase -- Java or SQL --
-- reads facts->>'correlationId' from any deployment_audit_events row; every actual reader
-- (PostgresAuditRepository, AuditGraphql, AuditFilter) already uses the real correlation_id column,
-- which PostgresDeploymentRepository.audit() already populates from
-- PostgresAuditRequestContext.bindMetadata() for every insert regardless of this trigger. The four
-- decision-recording audit() calls that also set a "correlationId" facts key do so explicitly,
-- preserving the caller-supplied identifier exactly as this trigger's own comment describes; every
-- other, system-triggered audit insert simply never had that key, and nothing depended on it being
-- present.

-- deployment_approval_plan_review_compatibility()'s final redefinition -- also removed, not ported:
-- see V021's removal comment. hive.m14_compiler_review is consequently never read by anything again;
-- PostgresDeploymentRepository.compilerReviewFactWrite() (which sets it) is harmless, unread, session-
-- local dead code, left alone as out of scope for this DSQL-conformance rewrite.

-- deployment_approval_evidence_invalidation_handoff_trigger's final redefinition -- also removed, not
-- ported: see V017's removal comment (deployment_evidence_invalidations has no writer, so neither
-- form of this trigger can ever fire).

-- deployment_approval_block_invalid_handoff()'s V025 redefinition is removed, not ported as SQL: V033
-- redeclares it and holds its true final form -- see V017's removal comment.

-- This V025 redeclaration of deployment_approval_reconcile_project_archives_page() is removed, not
-- ported as SQL: see V023's removal comment -- ported from V032's later, final redefinition instead.
