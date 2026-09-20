-- M13 keeps execution local. These records capture reproducible request facts and never contain credentials,
-- provider execution identifiers, or live infrastructure resource identifiers.
-- organization_id, project_id, agent_id, agent_version_id, catalog_release_id, and requested_by (below)
-- have no FOREIGN KEY: Aurora DSQL does not support them. deploy()'s versionSource()/environment()/
-- activeProject() checks already confirm every one of these rows exists, in the same transaction, before
-- insertDeployment()'s INSERT runs, so removing the constraints needs no new Java-side check.
CREATE TABLE IF NOT EXISTS deployments
(
    id
    UUID
    PRIMARY
    KEY,
    organization_id
    UUID
    NOT
    NULL,
    project_id UUID NOT NULL,
    agent_id UUID NOT NULL,
    agent_version_id UUID NOT NULL,
    catalog_release_id TEXT NOT NULL,
    catalog_release_digest CHAR
(
    64
) NOT NULL CHECK
(
    catalog_release_digest
    ~
    '^[0-9a-f]{64}$'
),
    environment TEXT NOT NULL CHECK
(
    environment
    IN
(
    'DEVELOPMENT',
    'STAGING',
    'PRODUCTION'
)),
    target_digest CHAR
(
    64
) NOT NULL CHECK
(
    target_digest
    ~
    '^[0-9a-f]{64}$'
),
    strategy TEXT NOT NULL CHECK
(
    strategy
    IN
(
    'ROLLING',
    'CANARY',
    'BLUE_GREEN',
    'LOCAL_FAILURE'
)),
    lifecycle_status TEXT NOT NULL CHECK
(
    lifecycle_status
    IN
(
    'WAITING',
    'EXECUTING',
    'SUCCEEDED',
    'FAILED',
    'CANCELED'
)),
    revision BIGINT NOT NULL CHECK
(
    revision >
    0
),
    idempotency_key TEXT NOT NULL CHECK
(
    length (
    btrim
(
    idempotency_key
)) BETWEEN 8 AND 160),
    requested_by UUID NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE
(
    project_id,
    idempotency_key
)
    );
CREATE INDEX IF NOT EXISTS deployments_project_history ON deployments (project_id, requested_at DESC, id DESC);
-- deployments_active_target (project_id, environment, lifecycle_status) WHERE lifecycle_status IN
-- ('WAITING', 'EXECUTING', 'SUCCEEDED') is not recreated: V015 renames every one of those three
-- lifecycle_status values (to REQUESTED/IN_PROGRESS/ACTIVE) and replaces this index's query need with
-- deployments_active_environment_target, so on a greenfield run this index is dead on arrival -- no row
-- can ever match its WHERE clause by the time any row exists. Also moot for Aurora DSQL, which rejects
-- CREATE INDEX ... WHERE outright.
CREATE INDEX IF NOT EXISTS deployments_agent_version ON deployments (agent_id, agent_version_id);

-- deployment_id, agent_version_id, and catalog_release_id have no FOREIGN KEY: Aurora DSQL does not
-- support them. insertPlan() is only ever called moments after insertDeployment() creates the
-- referenced deployments row in the same transaction, and with the same already-verified
-- agent_version_id/catalog_release_id insertDeployment() itself used, so removing the constraints
-- needs no new Java-side check.
CREATE TABLE IF NOT EXISTS deployment_plan_versions
(
    id
    UUID
    PRIMARY
    KEY,
    deployment_id
    UUID
    NOT
    NULL,
    version_number BIGINT NOT NULL CHECK
(
    version_number >
    0
),
    agent_version_id UUID NOT NULL,
    catalog_release_id TEXT NOT NULL,
    environment TEXT NOT NULL CHECK
(
    environment
    IN
(
    'DEVELOPMENT',
    'STAGING',
    'PRODUCTION'
)),
    compiler_version TEXT NOT NULL,
    canonical_plan JSONB NOT NULL,
    plan_digest CHAR
(
    64
) NOT NULL CHECK
(
    plan_digest
    ~
    '^[0-9a-f]{64}$'
),
    package_digest CHAR
(
    64
) NOT NULL CHECK
(
    package_digest
    ~
    '^[0-9a-f]{64}$'
),
    package_reference TEXT NOT NULL CHECK
(
    package_reference
    ~
    '^local://packages/[0-9a-f]{64}$'
),
    created_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE
(
    deployment_id,
    version_number
),
    UNIQUE
(
    deployment_id,
    package_digest
)
    );
-- Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule); see V040's comment
-- for the parallel reasoning on the trigger-based audit guards it removed. PostgresDeploymentRepository
-- only ever INSERTs into this table.

-- deployment_id and policy_id have no FOREIGN KEY: Aurora DSQL does not support them. deploy()'s
-- policy() call reads the project_approval_policies/project_approval_policy_versions row live, moments
-- before insertPolicySnapshot()'s INSERT uses request.policy().id() -- and deployment_id is the
-- deployments row insertDeployment() just created in the same transaction -- so removing either
-- constraint needs no new Java-side check.
CREATE TABLE IF NOT EXISTS deployment_policy_snapshots
(
    deployment_id
    UUID
    PRIMARY
    KEY,
    policy_id UUID NOT NULL,
    policy_revision BIGINT NOT NULL CHECK
(
    policy_revision >
    0
),
    policy_digest CHAR
(
    64
) NOT NULL CHECK
(
    policy_digest
    ~
    '^[0-9a-f]{64}$'
),
    policy_matrix JSONB NOT NULL,
    logical_environment_class TEXT NOT NULL CHECK
(
    logical_environment_class
    IN
(
    'DEVELOPMENT',
    'STAGING',
    'PRODUCTION'
)),
    risk TEXT NOT NULL CHECK
(
    risk
    IN
(
    'LOW',
    'MEDIUM',
    'HIGH'
)),
    -- JSONB (a JSON array of strings), not TEXT[]: Aurora DSQL does not support array types at all
    -- (confirmed against the real hive-dsql-verification cluster: "datatype text[] not supported").
    -- See PostgresDeploymentRepository's array()-equivalent (de)serialization this requires.
    required_evidence JSONB NOT NULL,
    required_approvers INTEGER NOT NULL CHECK
(
    required_approvers
    BETWEEN
    0
    AND
    2
),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
-- Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule); see V040's comment
-- for the parallel reasoning on the trigger-based audit guards it removed. PostgresDeploymentRepository
-- only ever INSERTs into this table.

-- deployment_id's FK is removed, not ported: Aurora DSQL rejects REFERENCES outright. Every writer
-- (PostgresDeploymentRepository's evidence-append path, PostgresEvaluationRepository's
-- EVALUATION_PASSED append) inserts for a deploymentId it already read or locked earlier in the same
-- transaction.
CREATE TABLE IF NOT EXISTS deployment_evidence_snapshots
(
    id
    UUID
    PRIMARY
    KEY,
    deployment_id UUID NOT NULL,
    evidence_kind TEXT NOT NULL CHECK
(
    evidence_kind
    IN
(
    'PLAN_VALIDATED',
    'CHANGE_SUMMARY_READY',
    'EVALUATION_PASSED'
)),
    evidence_digest CHAR
(
    64
) NOT NULL CHECK
(
    evidence_digest
    ~
    '^[0-9a-f]{64}$'
),
    expires_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE
(
    deployment_id,
    evidence_kind
)
    );
-- deployment_evidence_snapshots_no_update/_no_delete are removed, not ported: Aurora DSQL rejects
-- CREATE RULE outright, and nothing in this codebase -- Java or SQL -- ever UPDATEs or DELETEs a row
-- in this table; every writer only INSERTs.

-- deployment_id and deployment_plan_version_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- Every insert site (startExecution(), deadLetterState()) selects its deployment_plan_version_id via a
-- live subquery scoped to the same deployment_id in the same INSERT statement, and every deployment_id
-- used is one the caller already loaded/locked earlier in the same transaction, so removing the
-- constraints needs no new Java-side check.
CREATE TABLE IF NOT EXISTS deployment_attempts
(
    id
    UUID
    PRIMARY
    KEY,
    deployment_id
    UUID
    NOT
    NULL,
    deployment_plan_version_id UUID NOT NULL,
    attempt_number BIGINT NOT NULL CHECK
(
    attempt_number >
    0
),
    status TEXT NOT NULL CHECK
(
    status
    IN
(
    'QUEUED',
    'RUNNING',
    'SUCCEEDED',
    'FAILED',
    'CANCELED'
)),
    generation BIGINT NOT NULL CHECK
(
    generation >
    0
),
    started_at TIMESTAMPTZ NULL,
    completed_at TIMESTAMPTZ NULL,
    failure_code TEXT NULL CHECK
(
    failure_code
    IS
    NULL
    OR
    failure_code
    ~
    '^[A-Z][A-Z0-9_]{2,80}$'
),
    failure_summary TEXT NULL CHECK
(
    failure_summary
    IS
    NULL
    OR
    length
(
    failure_summary
) <= 240),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE
(
    deployment_id,
    attempt_number
)
    );
CREATE INDEX IF NOT EXISTS deployment_attempts_deployment_time ON deployment_attempts (deployment_id, attempt_number DESC);

-- deployment_attempt_id has no FOREIGN KEY: Aurora DSQL does not support them. stage() is only ever
-- called with an attempt id just inserted (or already loaded) earlier in the same transaction, so
-- removing the constraint needs no new Java-side check.
CREATE TABLE IF NOT EXISTS deployment_stage_events
(
    id
    UUID
    PRIMARY
    KEY,
    deployment_attempt_id
    UUID
    NOT
    NULL,
    sequence_number BIGINT NOT NULL CHECK
(
    sequence_number >
    0
),
    stage TEXT NOT NULL CHECK
(
    stage
    IN
(
    'REQUESTED',
    'PACKAGING',
    'EXECUTING',
    'COMPLETED',
    'CANCELED',
    'FAILED'
)),
    status TEXT NOT NULL CHECK
(
    status
    IN
(
    'STARTED',
    'SUCCEEDED',
    'FAILED',
    'CANCELED'
)),
    message TEXT NOT NULL CHECK
(
    length
(
    message
) <= 240),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE
(
    deployment_attempt_id,
    sequence_number
)
    );
-- Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule); see V040's comment
-- for the parallel reasoning on the trigger-based audit guards it removed. PostgresDeploymentRepository
-- only ever INSERTs into this table.

-- deployment_id has no FOREIGN KEY: Aurora DSQL does not support them. insertRuntimeHealth() is only
-- ever called moments after insertDeployment() creates the referenced deployments row in the same
-- transaction, so removing the constraint needs no new Java-side check.
CREATE TABLE IF NOT EXISTS deployment_runtime_health
(
    deployment_id
    UUID
    PRIMARY
    KEY,
    status TEXT NOT NULL CHECK
(
    status
    IN
(
    'NOT_OBSERVED',
    'STARTING',
    'HEALTHY',
    'UNHEALTHY',
    'CANCELED'
)),
    summary TEXT NOT NULL CHECK
(
    length
(
    summary
) <= 240),
    observed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    generation BIGINT NOT NULL CHECK
(
    generation >
    0
)
    );
CREATE INDEX IF NOT EXISTS deployment_runtime_health_observed ON deployment_runtime_health (status, observed_at DESC);

CREATE TABLE IF NOT EXISTS deployment_audit_events
(
    id
    UUID
    PRIMARY
    KEY,
    deployment_id
    UUID
    NOT
    NULL,
    actor_principal_id UUID NULL,
    action TEXT NOT NULL CHECK
(
    action
    IN
(
    'REQUESTED',
    'CANCELED',
    'EXECUTION_STARTED',
    'EXECUTION_SUCCEEDED',
    'EXECUTION_FAILED',
    'OUTBOX_DEAD_LETTERED'
)),
    facts JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
-- deployment_id and actor_principal_id have no FOREIGN KEY: Aurora DSQL does not support them. audit()
-- is only ever called with a deployment id already loaded/locked or just inserted earlier in the same
-- transaction, and actor_principal_id is either NULL (a system-initiated action) or the calling
-- principal, guaranteed to exist transitively the same way established across every prior domain step
-- (every capability check gating a write resolves through a membership row, and nothing in this
-- codebase ever deletes a principal) -- removing the constraints needs no new Java-side check.
-- Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule); see V040's comment
-- for the parallel reasoning on the trigger-based audit guards it removed. PostgresDeploymentRepository
-- only ever INSERTs into this table. (V015 previously redefined _no_update here to temporarily lift it
-- for a backfill UPDATE, then restore it -- that DROP RULE IF EXISTS is now a harmless no-op, since
-- the rule it targeted is never created, and the corresponding CREATE RULE there is removed too.)
CREATE INDEX IF NOT EXISTS deployment_audit_events_history ON deployment_audit_events (deployment_id, occurred_at DESC);

-- deployment_id has no FOREIGN KEY: Aurora DSQL does not support them. insertOutboxEvent() is only
-- ever called moments after insertDeployment() creates the referenced deployments row in the same
-- transaction, so removing the constraint needs no new Java-side check.
CREATE TABLE IF NOT EXISTS deployment_outbox_events
(
    id
    UUID
    PRIMARY
    KEY,
    deployment_id
    UUID
    NOT
    NULL,
    event_type TEXT NOT NULL CHECK
(
    event_type
    IN
(
    'EXECUTE_DEPLOYMENT',
    'COMPLETE_DEPLOYMENT'
)),
    payload JSONB NOT NULL,
    status TEXT NOT NULL CHECK
(
    status
    IN
(
    'PENDING',
    'PROCESSING',
    'DELIVERED',
    'DEAD_LETTER'
)),
    available_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    claimed_at TIMESTAMPTZ NULL,
    claimed_by TEXT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK
(
    attempt_count
    >=
    0
),
    delivered_at TIMESTAMPTZ NULL,
    last_error TEXT NULL CHECK
(
    last_error
    IS
    NULL
    OR
    length
(
    last_error
) <= 240),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
-- Aurora DSQL rejects CREATE INDEX ... WHERE outright (0A000 WHERE not supported for CREATE INDEX,
-- confirmed against the real cluster in Phase 0). Widened to a full index with status as the leading
-- column instead of a partial one scoped to PENDING -- claim()'s query already filters on status = ?
-- first, so the column order keeps the same selectivity the partial index gave it.
CREATE INDEX IF NOT EXISTS deployment_outbox_events_claim ON deployment_outbox_events (status, available_at, created_at);
CREATE INDEX IF NOT EXISTS deployment_outbox_events_deployment ON deployment_outbox_events (deployment_id, created_at);

-- Replaces pg_advisory_xact_lock('m13-deployment-quota:<project>') (quotaAnchor(), removed): Aurora
-- DSQL rejects pg_advisory_xact_lock outright, and unlike m14-approval-transition (see V015's comment),
-- no already-present row lock covers a project-wide quota check -- activeTarget()'s FOR SHARE lock is
-- scoped to one specific (project, agent, environment) target, and only fires when an ACTIVE deployment
-- already exists for it. This claim table gives deploy() a row every request for the same project
-- writes, so DSQL's OCC (confirmed to validate a transaction's whole read/write set at commit, not only
-- a write's own WHERE clause) serializes concurrent requests through it: quotaAnchor() claims this row
-- first, and if two overlapping deploy() calls for the same project both proceed, the loser's commit
-- fails with SQLSTATE 40001, caught the same way deploy() already catches SQLSTATE 23505 for a raced
-- idempotency key.
CREATE TABLE IF NOT EXISTS deployment_project_quota_claims
(
    project_id
    UUID
    PRIMARY
    KEY,
    claimed_at TIMESTAMPTZ NOT NULL
    );
