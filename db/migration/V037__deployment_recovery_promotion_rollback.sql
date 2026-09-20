-- M15 records local recovery actions as immutable facts. The action receipt owns client retry
-- semantics and contains no caller-entered reason or production confirmation text.
-- source_deployment_id/actor_principal_id/result_deployment_id have no FOREIGN KEY, and neither table
-- has a no-update/no-delete RULE: Aurora DSQL supports neither. recordActionReceipt()/recordPromotion()
-- (PostgresDeploymentRepository.java) are these tables' only writers -- each a single straight-line
-- INSERT supplying every value from an already-loaded deployment/receipt/actor, and neither table is
-- ever the target of an UPDATE or DELETE anywhere in this codebase.
CREATE TABLE deployment_recovery_action_receipts
(
    id                   UUID PRIMARY KEY,
    source_deployment_id UUID NOT NULL,
    actor_principal_id   UUID NOT NULL,
    action               TEXT NOT NULL CHECK (action IN ('RETRY', 'PROMOTE', 'ROLLBACK')
) ,
  idempotency_key TEXT NOT NULL CHECK (length(btrim(idempotency_key)) BETWEEN 8 AND 160),
  request_fingerprint CHAR(64) NOT NULL CHECK (request_fingerprint ~ '^[0-9a-f]{64}$'),
  result_deployment_id UUID NOT NULL,
  occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (source_deployment_id, actor_principal_id, action, idempotency_key)
);
CREATE INDEX deployment_recovery_action_receipts_source_history
    ON deployment_recovery_action_receipts (source_deployment_id, occurred_at DESC, id DESC);

CREATE TABLE deployment_promotion_facts
(
    id                UUID PRIMARY KEY,
    deployment_id     UUID     NOT NULL,
    action_receipt_id UUID     NOT NULL UNIQUE,
    agent_version_id  UUID     NOT NULL,
    target_digest     CHAR(64) NOT NULL CHECK (target_digest ~ '^[0-9a-f]{64}$'
) ,
  runtime_health_generation BIGINT NOT NULL CHECK (runtime_health_generation > 0),
  occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX deployment_promotion_facts_history ON deployment_promotion_facts (deployment_id, occurred_at DESC, id DESC);

ALTER TABLE deployment_audit_events DROP CONSTRAINT IF EXISTS deployment_audit_events_action_check;
ALTER TABLE deployment_audit_events
    ADD CONSTRAINT deployment_audit_events_action_check CHECK (
    action IN ('REQUESTED', 'CANCELED', 'EXECUTION_STARTED', 'EXECUTION_SUCCEEDED', 'EXECUTION_FAILED', 'OUTBOX_DEAD_LETTERED', 'OUTBOX_LEASE_RECLAIMED',
    'OUTBOX_DELIVERY_RETRIED', 'APPROVAL_RECORDED', 'APPROVAL_REPLAYED', 'APPROVAL_SATISFIED', 'APPROVAL_REJECTED', 'APPROVAL_EXPIRED',
    'APPROVAL_INVALIDATED', 'APPROVAL_EXECUTION_BLOCKED', 'RETRY_RECORDED', 'PROMOTION_RECORDED', 'ROLLBACK_RECORDED')
    );

-- effective_deployment_capabilities() is removed, not ported as SQL, here too: this CREATE OR REPLACE
-- FUNCTION is the final redefinition of the same function V015 originally declared (removed there --
-- see that migration's comment) and is the version PostgresEffectiveCapabilityEvaluator.
-- deploymentCapabilities()/deploymentViewPredicate() were actually ported from (VIEW's reader condition
-- widened to include project_approver/project_auditor; every writer additionally granted RETRY/PROMOTE/
-- ROLLBACK, not only REQUEST/CANCEL). Aurora DSQL rejects CREATE FUNCTION outright regardless of which
-- migration declares it, so leaving this redefinition in place would still break V037 even with V015's
-- declaration already gone.
