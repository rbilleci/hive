-- Execution stays local. These records capture reproducible request facts and never contain credentials,
-- provider execution identifiers, or live infrastructure resource identifiers.
-- organization_id, project_id, agent_id, agent_version_id, catalog_release_id, and requested_by (below)
-- have no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::deployment::mutations::deploy` confirms every one of these rows exists, in the
-- same transaction, before `hive_persistence::deployment::writes::insert_deployment` runs; those
-- checks are the only referential guards.
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
CREATE INDEX IF NOT EXISTS deployments_agent_version ON deployments (agent_id, agent_version_id);

-- deployment_id, agent_version_id, and catalog_release_id have no FOREIGN KEY: Aurora DSQL does not
-- support them. `hive_persistence::deployment::writes::insert_plan` runs immediately after
-- `insert_deployment` creates the referenced deployments row in the same transaction, with the same
-- already-verified agent_version_id/catalog_release_id; that ordering is the only referential guard.
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
-- deployment_plan_versions is append-only by convention, not by constraint: Aurora DSQL rejects CREATE
-- RULE outright (0A000 unsupported statement: Rule), so nothing in the schema blocks an UPDATE or
-- DELETE. `hive_persistence::deployment` only ever INSERTs into this table.

-- deployment_id and policy_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::deployment::queries::policy` reads the project_approval_policies/
-- project_approval_policy_versions row live just before
-- `hive_persistence::deployment::writes::insert_policy_snapshot` records its id, and deployment_id is
-- the deployments row `insert_deployment` created in the same transaction; those reads are the only
-- referential guards.
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
    -- Readers and writers serialize it as a JSON array rather than a SQL array.
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
-- deployment_policy_snapshots is append-only by convention, not by constraint: Aurora DSQL rejects
-- CREATE RULE outright (0A000 unsupported statement: Rule), so nothing in the schema blocks an UPDATE
-- or DELETE. `hive_persistence::deployment` only ever INSERTs into this table.

-- deployment_id has no FOREIGN KEY: Aurora DSQL rejects REFERENCES outright. Every writer
-- (`hive_persistence::deployment::writes::insert_evidence` and the EVALUATION_PASSED append in
-- `hive_persistence::evaluation`) inserts for a deployment id it read or locked earlier in the same
-- transaction; that is the only referential guard.
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
-- deployment_evidence_snapshots is append-only by convention, not by constraint: Aurora DSQL rejects
-- CREATE RULE outright, so nothing in the schema blocks an UPDATE or DELETE. Every writer only
-- INSERTs.

-- deployment_id and deployment_plan_version_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- Every insert site (`insert_attempt` and `dead_letter_state` in
-- `hive_persistence::deployment::worker`) resolves deployment_plan_version_id from a live query scoped
-- to the same deployment_id, and every deployment_id used is one the caller loaded or locked earlier
-- in the same transaction; that is the only referential guard.
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

-- deployment_attempt_id has no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::deployment::writes::stage` is only ever called with an attempt id inserted or
-- loaded earlier in the same transaction; that is the only referential guard.
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
-- deployment_stage_events is append-only by convention, not by constraint: Aurora DSQL rejects CREATE
-- RULE outright (0A000 unsupported statement: Rule), so nothing in the schema blocks an UPDATE or
-- DELETE. `hive_persistence::deployment` only ever INSERTs into this table.

-- deployment_id has no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::deployment::writes::insert_runtime_health` runs immediately after
-- `insert_deployment` creates the referenced deployments row in the same transaction; that ordering is
-- the only referential guard.
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
-- deployment_id and actor_principal_id have no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::deployment::writes::audit` is only ever called with a deployment id loaded,
-- locked, or inserted earlier in the same transaction, and actor_principal_id is either NULL (a
-- system-initiated action) or the calling principal, guaranteed to exist transitively: every
-- capability check gating a write resolves through a membership row, and nothing ever deletes a
-- principal.
-- deployment_audit_events is append-only by convention, not by constraint: Aurora DSQL rejects CREATE
-- RULE outright (0A000 unsupported statement: Rule), so nothing in the schema blocks an UPDATE or
-- DELETE. `hive_persistence::deployment` only ever INSERTs into this table.
CREATE INDEX IF NOT EXISTS deployment_audit_events_history ON deployment_audit_events (deployment_id, occurred_at DESC);

-- deployment_id has no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::deployment::writes::enqueue` runs immediately after `insert_deployment` creates
-- the referenced deployments row in the same transaction; that ordering is the only referential guard.
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
-- Aurora DSQL rejects CREATE INDEX ... WHERE outright (0A000 WHERE not supported for CREATE INDEX), so
-- this is a full index with status as its leading column rather than a partial one scoped to PENDING.
-- The claim query filters on status first, so the column order preserves the selectivity.
CREATE INDEX IF NOT EXISTS deployment_outbox_events_claim ON deployment_outbox_events (status, available_at, created_at);
CREATE INDEX IF NOT EXISTS deployment_outbox_events_deployment ON deployment_outbox_events (deployment_id, created_at);

-- This table exists only to serialize concurrent deploy requests for one project. Aurora DSQL rejects
-- pg_advisory_xact_lock outright, and no other row lock covers a project-wide quota check:
-- `hive_persistence::deployment::queries::active_target` takes a FOR SHARE lock scoped to one
-- (project, agent, environment) target, and only when an ACTIVE deployment already exists for it.
-- Every request for the same project writes this one row, and DSQL's optimistic concurrency control
-- validates a transaction's whole read/write set at commit rather than only a write's own WHERE
-- clause, so `hive_persistence::deployment::mutations::quota_anchor` claims the row first and the
-- loser of two overlapping deploys for one project fails its commit with SQLSTATE 40001, handled the
-- same way `deploy` handles SQLSTATE 23505 for a raced idempotency key.
CREATE TABLE IF NOT EXISTS deployment_project_quota_claims
(
    project_id
    UUID
    PRIMARY
    KEY,
    claimed_at TIMESTAMPTZ NOT NULL
    );
