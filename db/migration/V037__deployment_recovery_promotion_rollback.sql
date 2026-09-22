-- Local recovery actions are immutable facts. The action receipt owns client retry semantics and
-- holds no caller-entered reason or production confirmation text.
-- source_deployment_id/actor_principal_id/result_deployment_id have no FOREIGN KEY, and neither table
-- has a no-update/no-delete RULE: Aurora DSQL supports neither. `record_action_receipt` and
-- `record_promotion` are these tables' only writers -- each a single straight-line INSERT supplying
-- every value from an already-loaded deployment, receipt, and actor -- and nothing UPDATEs or DELETEs
-- either table.
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

-- Aurora DSQL rejects CREATE FUNCTION outright, so deployment capabilities are computed in
-- application code, by `deployment_capabilities`: VIEW is granted to project approvers and auditors
-- as well as readers, and every writer role additionally holds RETRY, PROMOTE, and ROLLBACK on top of
-- REQUEST and CANCEL.
