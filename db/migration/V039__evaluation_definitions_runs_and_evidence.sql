-- Evaluation definitions are local and deterministic; evaluation runs are immutable facts. Nothing
-- here reaches a provider endpoint, credentials, cloud resources, or a live deployment adapter.
-- project_id and created_by have no FOREIGN KEY: Aurora DSQL does not support them.
-- `create_definition` confirms the project exists (`project_active`), and created_by is the calling,
-- capability-checked principal, which exists transitively via the membership row a capability check
-- requires. No application-side check replaces the constraints.
CREATE TABLE IF NOT EXISTS evaluation_definitions
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL,
    slug TEXT NOT NULL CHECK
(
    length (
    btrim
(
    slug
)) BETWEEN 1 AND 120),
    lifecycle_status TEXT NOT NULL DEFAULT 'ACTIVE' CHECK
(
    lifecycle_status
    IN
(
    'ACTIVE',
    'ARCHIVED'
)),
    created_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
CREATE UNIQUE INDEX IF NOT EXISTS evaluation_definitions_project_slug ON evaluation_definitions (project_id, LOWER (slug));
CREATE INDEX IF NOT EXISTS evaluation_definitions_project_history ON evaluation_definitions (project_id, created_at DESC, id DESC);

-- definition_id has no FOREIGN KEY: Aurora DSQL does not support them. `insert_draft` is only ever
-- called moments after a fresh evaluation_definitions INSERT in the same transaction, so the row is
-- guaranteed to exist.
CREATE TABLE IF NOT EXISTS evaluation_definition_drafts
(
    definition_id
    UUID
    PRIMARY
    KEY,
    canonical_document JSONB NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1 CHECK
(
    revision >
    0
),
    validation_status TEXT NOT NULL DEFAULT 'NOT_VALIDATED' CHECK
(
    validation_status
    IN
(
    'NOT_VALIDATED',
    'VALID',
    'INVALID'
)),
    diagnostics JSONB NOT NULL DEFAULT '[]'::jsonb,
    based_on_version_id UUID NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );

-- definition_id, based_on_version_id, and published_by have no FOREIGN KEY: Aurora DSQL does not
-- support them. `publish_draft` confirms the definition exists earlier in the same transaction (via
-- `definition`'s FOR UPDATE read), based_on_version_id is copied from the draft's own
-- already-validated field, and published_by is the calling, capability-checked principal.
CREATE TABLE IF NOT EXISTS evaluation_definition_versions
(
    id
    UUID
    PRIMARY
    KEY,
    definition_id
    UUID
    NOT
    NULL,
    version_number BIGINT NOT NULL CHECK
(
    version_number >
    0
),
    canonical_document JSONB NOT NULL,
    content_digest CHAR
(
    64
) NOT NULL CHECK
(
    content_digest
    ~
    '^[0-9a-f]{64}$'
),
    based_on_version_id UUID NULL,
    published_by UUID NOT NULL,
    published_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE
(
    definition_id,
    version_number
),
    UNIQUE
(
    definition_id,
    content_digest
)
    );
CREATE INDEX IF NOT EXISTS evaluation_definition_versions_history ON evaluation_definition_versions (definition_id, version_number DESC);
CREATE INDEX IF NOT EXISTS evaluation_definition_versions_page
    ON evaluation_definition_versions (definition_id, version_number DESC, id DESC);

-- This read projection is derived entirely from existing target authorities. The source write owns
-- the transaction; the upserts that maintain this table (`project_agent_version_target` on agent
-- publication, `project_deployment_target` on deployment insert) only reconstruct the
-- selectable-keyset representation.
-- project_id, agent_version_id, and environment_definition_version_id have no FOREIGN KEY: Aurora
-- DSQL does not support them. Every upsert reads its source row in the same transaction that writes
-- here, so the referenced rows always exist.
CREATE TABLE IF NOT EXISTS evaluation_target_projections
(
    project_id
    UUID
    NOT
    NULL,
    target_kind TEXT NOT NULL CHECK
(
    target_kind
    IN
(
    'AGENT_VERSION',
    'DEPLOYMENT'
)),
    target_id UUID NOT NULL,
    agent_version_id UUID NOT NULL,
    environment_definition_version_id UUID NOT NULL,
    logical_environment_class TEXT NOT NULL CHECK
(
    logical_environment_class
    IN
(
    'DEVELOPMENT',
    'STAGING',
    'PRODUCTION'
)),
    display_name TEXT NOT NULL CHECK
(
    length (
    btrim
(
    display_name
)) BETWEEN 1 AND 240),
    PRIMARY KEY
(
    target_kind,
    target_id,
    environment_definition_version_id
)
    );
CREATE INDEX IF NOT EXISTS evaluation_target_projections_project_keyset
    ON evaluation_target_projections (project_id, target_kind, display_name, target_id, environment_definition_version_id);

-- project_id, definition_version_id, environment_definition_version_id, requester_id, and
-- source_run_id have no FOREIGN KEY: Aurora DSQL does not support them. `run_evaluation` and `rerun`
-- confirm each referenced row (project, definition version, target's environment, source run) earlier
-- in the same transaction before this INSERT, and requester_id is the calling, capability-checked
-- principal.
CREATE TABLE IF NOT EXISTS evaluation_runs
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL,
    definition_version_id UUID NOT NULL,
    target_kind TEXT NOT NULL CHECK
(
    target_kind
    IN
(
    'AGENT_VERSION',
    'DEPLOYMENT'
)),
    target_id UUID NOT NULL,
    environment_definition_version_id UUID NOT NULL,
    requester_id UUID NOT NULL,
    source_run_id UUID NULL,
    creation_fingerprint CHAR
(
    64
) NOT NULL CHECK
(
    creation_fingerprint
    ~
    '^[0-9a-f]{64}$'
),
    lifecycle_status TEXT NOT NULL DEFAULT 'QUEUED' CHECK
(
    lifecycle_status
    IN
(
    'QUEUED',
    'RUNNING',
    'COMPLETED',
    'FAILED',
    'CANCELED'
)),
    generation BIGINT NOT NULL DEFAULT 1 CHECK
(
    generation >
    0
),
    outcome_category TEXT NULL CHECK
(
    outcome_category
    IS
    NULL
    OR
    outcome_category
    IN
(
    'PASSED',
    'CASE_FAILED',
    'TARGET_FAILED',
    'RUNNER_FAILED',
    'CANCELED'
)),
    outcome_code TEXT NULL CHECK
(
    outcome_code
    IS
    NULL
    OR
    outcome_code
    ~
    '^[A-Z][A-Z0-9_]{2,80}$'
),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    started_at TIMESTAMPTZ NULL,
    completed_at TIMESTAMPTZ NULL
    );
CREATE INDEX IF NOT EXISTS evaluation_runs_project_history ON evaluation_runs (project_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS evaluation_runs_project_status_history ON evaluation_runs (project_id, lifecycle_status, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS evaluation_runs_definition_version_history ON evaluation_runs (definition_version_id, created_at DESC, id DESC);
-- Widened to a full index: Aurora DSQL rejects partial indexes outright (0A000 WHERE not supported
-- for CREATE INDEX). Read-path performance only, not a constraint.
CREATE INDEX IF NOT EXISTS evaluation_runs_source ON evaluation_runs (source_run_id);

-- run_id, agent_version_id, deployment_id, environment_definition_version_id, and catalog_release_id
-- have no FOREIGN KEY: Aurora DSQL does not support them. `insert_target` is only ever called moments
-- after a fresh evaluation_runs INSERT in the same transaction, using a target already resolved from
-- a live query against agent_versions/deployments/environment_definition_versions/catalog_releases
-- earlier in that same transaction, so every referenced row is guaranteed to exist.
CREATE TABLE IF NOT EXISTS evaluation_target_snapshots
(
    run_id
    UUID
    PRIMARY
    KEY,
    agent_version_id UUID NOT NULL,
    deployment_id UUID NULL,
    environment_definition_version_id UUID NOT NULL,
    logical_environment_class TEXT NOT NULL CHECK
(
    logical_environment_class
    IN
(
    'DEVELOPMENT',
    'STAGING',
    'PRODUCTION'
)),
    agent_content_digest CHAR
(
    64
) NOT NULL CHECK
(
    agent_content_digest
    ~
    '^[0-9a-f]{64}$'
),
    target_digest CHAR
(
    64
) NULL CHECK
(
    target_digest
    IS
    NULL
    OR
    target_digest
    ~
    '^[0-9a-f]{64}$'
),
    plan_digest CHAR
(
    64
) NULL CHECK
(
    plan_digest
    IS
    NULL
    OR
    plan_digest
    ~
    '^[0-9a-f]{64}$'
),
    package_digest CHAR
(
    64
) NULL CHECK
(
    package_digest
    IS
    NULL
    OR
    package_digest
    ~
    '^[0-9a-f]{64}$'
),
    binding_digest CHAR
(
    64
) NULL CHECK
(
    binding_digest
    IS
    NULL
    OR
    binding_digest
    ~
    '^[0-9a-f]{64}$'
),
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
    environment_content_digest CHAR
(
    64
) NOT NULL CHECK
(
    environment_content_digest
    ~
    '^[0-9a-f]{64}$'
),
    CHECK
(
(
    deployment_id
    IS
    NULL
    AND
    target_digest
    IS
    NULL
    AND
    plan_digest
    IS
    NULL
    AND
    package_digest
    IS
    NULL
    AND
    binding_digest
    IS
    NULL
)
    OR
(
    deployment_id
    IS
    NOT
    NULL
    AND
    target_digest
    IS
    NOT
    NULL
    AND
    plan_digest
    IS
    NOT
    NULL
    AND
    package_digest
    IS
    NOT
    NULL
    AND
    binding_digest
    IS
    NOT
    NULL
))
    );

-- run_id has no FOREIGN KEY: Aurora DSQL does not support them. `insert_cases` is only ever called
-- moments after a fresh evaluation_runs INSERT in the same transaction, so the row is guaranteed to
-- exist.
CREATE TABLE IF NOT EXISTS evaluation_case_runs
(
    id
    UUID
    PRIMARY
    KEY,
    run_id
    UUID
    NOT
    NULL,
    case_key TEXT NOT NULL CHECK
(
    length (
    btrim
(
    case_key
)) BETWEEN 1 AND 120),
    ordinal INTEGER NOT NULL CHECK
(
    ordinal >
    0
),
    lifecycle_status TEXT NOT NULL DEFAULT 'QUEUED' CHECK
(
    lifecycle_status
    IN
(
    'QUEUED',
    'RUNNING',
    'COMPLETED',
    'FAILED',
    'CANCELED'
)),
    passed BOOLEAN NULL,
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
    completed_at TIMESTAMPTZ NULL,
    UNIQUE
(
    run_id,
    case_key
),
    UNIQUE
(
    run_id,
    ordinal
)
    );
CREATE INDEX IF NOT EXISTS evaluation_case_runs_page ON evaluation_case_runs (run_id, ordinal, id);

-- run_id has no FOREIGN KEY: Aurora DSQL does not support them. `insert_result` and `finalize_run`
-- are only ever called with a run_id already resolved (and, on every call path, FOR UPDATE-locked)
-- earlier in the same transaction, so the row is guaranteed to exist.
CREATE TABLE IF NOT EXISTS evaluation_results
(
    id
    UUID
    PRIMARY
    KEY,
    run_id
    UUID
    NOT
    NULL
    UNIQUE,
    passed BOOLEAN NOT NULL,
    outcome_category TEXT NOT NULL CHECK
(
    outcome_category
    IN
(
    'PASSED',
    'CASE_FAILED',
    'TARGET_FAILED',
    'RUNNER_FAILED',
    'CANCELED'
)),
    summary_digest CHAR
(
    64
) NOT NULL CHECK
(
    summary_digest
    ~
    '^[0-9a-f]{64}$'
),
    completed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
    );

-- run_id has no FOREIGN KEY: Aurora DSQL does not support them. `finalize_run` is only ever called
-- with a run_id already FOR UPDATE-locked earlier in the same transaction.
CREATE TABLE IF NOT EXISTS evaluation_metric_results
(
    id
    UUID
    PRIMARY
    KEY,
    run_id
    UUID
    NOT
    NULL,
    metric_code TEXT NOT NULL CHECK
(
    metric_code
    ~
    '^[A-Z][A-Z0-9_]{2,80}$'
),
    value NUMERIC
(
    12,
    8
) NOT NULL CHECK
(
    value
    >=
    0
    AND
    value
    <=
    1
),
    threshold NUMERIC
(
    12,
    8
) NOT NULL CHECK
(
    threshold
    >=
    0
    AND
    threshold
    <=
    1
),
    passed BOOLEAN NOT NULL,
    UNIQUE
(
    run_id,
    metric_code
)
    );
CREATE INDEX IF NOT EXISTS evaluation_metric_results_page ON evaluation_metric_results (run_id, metric_code, id);

-- run_id has no FOREIGN KEY: Aurora DSQL does not support them. `finalize_run` is only ever called
-- with a run_id already FOR UPDATE-locked earlier in the same transaction.
CREATE TABLE IF NOT EXISTS evaluation_artifact_metadata
(
    id
    UUID
    PRIMARY
    KEY,
    run_id
    UUID
    NOT
    NULL,
    artifact_kind TEXT NOT NULL CHECK
(
    artifact_kind
    ~
    '^[A-Z][A-Z0-9_]{2,80}$'
),
    content_digest CHAR
(
    64
) NOT NULL CHECK
(
    content_digest
    ~
    '^[0-9a-f]{64}$'
),
    media_type TEXT NOT NULL CHECK
(
    length (
    btrim
(
    media_type
)) BETWEEN 1 AND 120),
    byte_length BIGINT NOT NULL CHECK
(
    byte_length
    >=
    0
),
    UNIQUE
(
    run_id,
    artifact_kind,
    content_digest
)
    );
CREATE INDEX IF NOT EXISTS evaluation_artifact_metadata_page ON evaluation_artifact_metadata (run_id, artifact_kind, id);

-- run_id, definition_id, and actor_principal_id have no FOREIGN KEY: Aurora DSQL does not support
-- them. `audit_definition` and `audit_run` are only ever called with an id already resolved earlier
-- in the same transaction, and actor_principal_id is either NULL (the local worker) or the calling,
-- capability-checked principal.
CREATE TABLE IF NOT EXISTS evaluation_audit_events
(
    id
    UUID
    PRIMARY
    KEY,
    run_id
    UUID
    NULL,
    definition_id UUID NULL,
    actor_principal_id UUID NULL,
    action TEXT NOT NULL CHECK
(
    action
    ~
    '^[A-Z][A-Z0-9_]{2,80}$'
),
    facts JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK
(
    run_id
    IS
    NOT
    NULL
    OR
    definition_id
    IS
    NOT
    NULL
)
    );
-- Both widened to full indexes: Aurora DSQL rejects partial indexes outright (0A000 WHERE not
-- supported for CREATE INDEX). Read-path performance only, not a constraint, for both.
CREATE INDEX IF NOT EXISTS evaluation_audit_events_run_history ON evaluation_audit_events (run_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS evaluation_audit_events_run_page ON evaluation_audit_events (run_id, occurred_at DESC, id DESC);

-- project_id, principal_id, definition_id, definition_version_id, and run_id have no FOREIGN KEY:
-- Aurora DSQL does not support them. `receipt` is only ever called with a project and principal
-- already resolved and, when set, a definition, version, or run id already resolved earlier in the
-- same transaction.
CREATE TABLE IF NOT EXISTS evaluation_command_receipts
(
    id
    UUID
    PRIMARY
    KEY,
    project_id
    UUID
    NOT
    NULL,
    principal_id UUID NOT NULL,
    action TEXT NOT NULL CHECK
(
    action
    IN
(
    'CREATE',
    'UPDATE',
    'VALIDATE',
    'DUPLICATE',
    'PUBLISH',
    'RUN',
    'CANCEL',
    'RERUN'
)),
    idempotency_key TEXT NOT NULL CHECK
(
    length (
    btrim
(
    idempotency_key
)) BETWEEN 8 AND 160),
    request_fingerprint CHAR
(
    64
) NOT NULL CHECK
(
    request_fingerprint
    ~
    '^[0-9a-f]{64}$'
),
    definition_id UUID NULL,
    definition_version_id UUID NULL,
    run_id UUID NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK
(
    num_nonnulls
(
    definition_id,
    definition_version_id,
    run_id
) = 1),
    UNIQUE
(
    project_id,
    principal_id,
    action,
    idempotency_key
)
    );

-- run_id and case_run_id have no FOREIGN KEY: Aurora DSQL does not support them. `enqueue` is only
-- ever called with a run_id and case_run_id already resolved earlier in the same transaction.
CREATE TABLE IF NOT EXISTS evaluation_outbox_events
(
    id
    UUID
    PRIMARY
    KEY,
    run_id
    UUID
    NOT
    NULL,
    event_type TEXT NOT NULL CHECK
(
    event_type
    IN
(
    'START',
    'CASE',
    'FINALIZE'
)),
    case_run_id UUID NULL,
    status TEXT NOT NULL DEFAULT 'PENDING' CHECK
(
    status
    IN
(
    'PENDING',
    'PROCESSING',
    'DELIVERED',
    'DEAD_LETTER',
    'CANCELED'
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
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Aurora DSQL rejects partial indexes outright (0A000 WHERE not supported for CREATE INDEX), so
    -- the two invariants this table needs -- at most one no-case event per (run, type), and at most
    -- one event per (run, type, case) -- cannot be two partial unique indexes. case_slot collapses
    -- them into one full unique index: it equals case_run_id when that is set, and a fixed nil UUID
    -- sentinel when case_run_id is NULL, so every no-case event for a given (run, type) shares one
    -- non-null value and a duplicate still conflicts. Leaving the column NULL would not work, because
    -- a unique index never treats two NULLs as conflicting. `enqueue` derives case_slot the same way
    -- on every insert; a writer that skips it breaks both invariants.
    case_slot UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000'
    );
CREATE INDEX IF NOT EXISTS evaluation_outbox_events_claim ON evaluation_outbox_events (available_at, created_at);
CREATE UNIQUE INDEX IF NOT EXISTS evaluation_outbox_events_run_event_slot
    ON evaluation_outbox_events (run_id, event_type, case_slot);

CREATE TABLE IF NOT EXISTS evaluation_worker_heartbeats
(
    worker_id
    TEXT
    PRIMARY
    KEY
    CHECK (
    length (
    btrim
(
    worker_id
)) BETWEEN 1 AND 120),
    observed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    status TEXT NOT NULL DEFAULT 'READY' CHECK
(
    status
    IN
(
    'READY',
    'DEGRADED'
)),
    last_failure_code TEXT NULL CHECK
(
    last_failure_code
    IS
    NULL
    OR
    last_failure_code
    ~
    '^[A-Z][A-Z0-9_]{2,80}$'
),
    last_delivery_count INTEGER NOT NULL DEFAULT 0 CHECK
(
    last_delivery_count
    >=
    0
),
    pending_events INTEGER NOT NULL DEFAULT 0 CHECK
(
    pending_events
    >=
    0
),
    oldest_pending_at TIMESTAMPTZ NULL,
    pending_truncated BOOLEAN NOT NULL DEFAULT FALSE
    );

-- Aurora DSQL rejects CREATE TRIGGER, CREATE FUNCTION ... LANGUAGE plpgsql, and pg_advisory_xact_lock
-- outright, so the evaluation domain has no database-side machinery at all. What that costs, and what
-- holds each invariant instead:
--
-- Derived-projection maintenance. evaluation_target_projections is maintained by explicit upserts:
-- `project_agent_version_target` after the agent_versions INSERT in `publish_draft`, and
-- `project_deployment_target` after the deployments INSERT in `insert_deployment` -- the only two
-- sources of the rows this projection derives from. Nothing INSERTs into
-- environment_definition_versions outside V015's seed rows, and nothing UPDATEs agents.display_name,
-- so no other event can invalidate the projection. The upserts take no lock: every upsert writes
-- deterministic values for a given (target_kind, target_id, environment_definition_version_id) key,
-- since agent_versions and deployments rows are immutable once created, so two concurrent upserts on
-- one key write identical content.
--
-- Blanket immutability. Nothing rejects an UPDATE or DELETE against evaluation_definition_versions,
-- evaluation_target_snapshots, evaluation_results, evaluation_metric_results,
-- evaluation_artifact_metadata, evaluation_audit_events, or evaluation_command_receipts. Nothing
-- writes those tables except the INSERTs above; a future writer that updates or deletes one of them
-- has nothing to stop it.
--
-- Transition guards. A case run cannot leave a terminal state because `update_case` and `start_case`
-- restrict their UPDATE with WHERE lifecycle_status IN (...), which is the transition check itself
-- rather than a guard on top of one. For runs, `update_run` rejects a terminal source state and
-- requires its caller to pass generation + 1, so the generation counter advances exactly once per
-- transition. Its UPDATE never sets project_id, definition_version_id, or the other
-- creation-time columns, which is the only thing keeping them fixed after insert.

-- source_evaluation_run_id is nullable: an evidence fact need not come from an evaluation. The CHECK
-- below confines it to EVALUATION_PASSED evidence, and the unique index below that keeps one
-- evaluation run from satisfying a second deployment.
-- The column has no FOREIGN KEY: Aurora DSQL does not support them. `append_evidence` is its only
-- writer and sets it only to a run id resolved and FOR UPDATE-locked earlier in the same transaction.
ALTER TABLE deployment_evidence_snapshots
    ADD COLUMN IF NOT EXISTS source_evaluation_run_id UUID NULL;
ALTER TABLE deployment_evidence_snapshots DROP CONSTRAINT IF EXISTS deployment_evidence_snapshots_evaluation_provenance;
ALTER TABLE deployment_evidence_snapshots
    ADD CONSTRAINT deployment_evidence_snapshots_evaluation_provenance
        CHECK (source_evaluation_run_id IS NULL OR evidence_kind = 'EVALUATION_PASSED');
-- A full unique index, not a partial one: Aurora DSQL rejects partial indexes outright (0A000 WHERE
-- not supported for CREATE INDEX). Nothing is lost here, unlike the outbox case above -- a unique
-- index never treats two NULLs as conflicting, so rows with no source run coexist freely while a row
-- with a real source run is still limited to one.
CREATE UNIQUE INDEX IF NOT EXISTS deployment_evidence_snapshots_source_evaluation_run
    ON deployment_evidence_snapshots (source_evaluation_run_id);

-- Aurora DSQL rejects CREATE FUNCTION ... LANGUAGE plpgsql and pg_advisory_xact_lock outright, so
-- recording evaluation evidence and re-evaluating the approval it feeds happen in `append_evidence`,
-- together with the approval predicates it consults (`approval_evidence_issue`,
-- `waiting_for_evaluation`). It holds a FOR UPDATE lock on the deployment row rather than an advisory
-- lock; every other approval-transition path locks that same row, so the deployment row is the single
-- serialization point.
CREATE
OR REPLACE VIEW effective_evaluation_capabilities AS
SELECT DISTINCT membership.principal_id,
                membership.project_id,
                capability.code
FROM project_memberships membership
         JOIN project_membership_roles role ON role.membership_id = membership.id
         CROSS JOIN LATERAL (VALUES ('EVALUATION_DEFINITION.VIEW'),
                                    ('EVALUATION_DEFINITION.AUTHOR'),
                                    ('EVALUATION_DEFINITION.PUBLISH'),
                                    ('EVALUATION_RUN.VIEW'),
                                    ('EVALUATION_RUN.RUN'),
                                    ('EVALUATION_RUN.CANCEL'),
                                    ('EVALUATION_RUN.RERUN')
    ) AS capability(code)
WHERE membership.ended_at IS NULL
  AND (role.role_code IN ('PROJECT_ADMIN', 'AGENT_DEVELOPER')
    OR (role.role_code = 'OPERATOR' AND capability.code IN
                                        ('EVALUATION_RUN.VIEW', 'EVALUATION_RUN.RUN', 'EVALUATION_RUN.CANCEL',
                                         'EVALUATION_RUN.RERUN'))
    OR (role.role_code IN ('AUDITOR', 'DEPLOYMENT_APPROVER') AND
        capability.code IN ('EVALUATION_DEFINITION.VIEW', 'EVALUATION_RUN.VIEW')));
