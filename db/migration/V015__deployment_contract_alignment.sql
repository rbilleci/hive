-- Local execution stays deterministic by binding every frozen fact to an immutable
-- environment-definition version. The rows below contain catalog metadata only.
--
-- No statement in any migration calls digest() or sha256(): Aurora DSQL rejects CREATE EXTENSION
-- outright (confirmed against the real hive-dsql-verification cluster: "unsupported statement:
-- CreateExtension"), so pgcrypto is unavailable. Every digest column is computed by the application
-- before its INSERT.
-- catalog_release_id has no FOREIGN KEY: Aurora DSQL does not support them. The application never
-- writes environment_definition_versions; the fixed INSERT below is its only writer, so no
-- application-side check replaces the constraint.
CREATE TABLE IF NOT EXISTS environment_definition_versions
(
    id
    UUID
    PRIMARY
    KEY,
    catalog_release_id
    TEXT
    NOT
    NULL,
    catalog_release_digest CHAR
(
    64
) NOT NULL CHECK
(
    catalog_release_digest
    ~
    '^[0-9a-f]{64}$'
),
    stable_definition_id TEXT NOT NULL CHECK
(
    stable_definition_id
    ~
    '^[a-z][a-z0-9-]{0,62}$'
),
    version TEXT NOT NULL CHECK
(
    version
    ~
    '^[a-z][a-z0-9-]{0,62}$'
),
    display_name TEXT NOT NULL CHECK
(
    length (
    btrim
(
    display_name
)) BETWEEN 2 AND 120),
    logical_environment_class TEXT NOT NULL CHECK
(
    logical_environment_class
    IN
(
    'DEVELOPMENT',
    'STAGING',
    'PRODUCTION'
)),
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
    published_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE
(
    stable_definition_id,
    version
),
    UNIQUE
(
    stable_definition_id,
    content_digest
)
    );
CREATE INDEX IF NOT EXISTS environment_definition_versions_release ON environment_definition_versions (catalog_release_id);
CREATE INDEX IF NOT EXISTS environment_definition_versions_release_keyset
    ON environment_definition_versions (catalog_release_id, stable_definition_id, version, id);


INSERT INTO environment_definition_versions (id, catalog_release_id, catalog_release_digest, stable_definition_id,
                                             version, display_name,
                                             logical_environment_class, canonical_document, content_digest)
SELECT value.id, release.id, release.source_digest, value.stable_definition_id, 'v1', value.display_name,
  value.logical_environment_class, value.document::jsonb, value.content_digest
FROM catalog_releases release
    JOIN (VALUES
    ('e1300000-0000-0000-0000-000000000001'::uuid, 'local-development', 'Local development', 'DEVELOPMENT', '{"logicalEnvironmentClass":"DEVELOPMENT","stableDefinitionId":"local-development","version":"v1"}', 'cf174304e8b23d27f4ca877bea45fb51d66eaa480f0a55a17c4fd5a46b9e328e'), ('e1300000-0000-0000-0000-000000000002'::uuid, 'local-staging', 'Local staging', 'STAGING', '{"logicalEnvironmentClass":"STAGING","stableDefinitionId":"local-staging","version":"v1"}', '22497d27693820c11ce06d934b5bf259a0d47459155e53060f93744ad7f6b237'), ('e1300000-0000-0000-0000-000000000003'::uuid, 'local-production', 'Local production', 'PRODUCTION', '{"logicalEnvironmentClass":"PRODUCTION","stableDefinitionId":"local-production","version":"v1"}', '4a93f1bbdf8d5774067d847da94f759b330114b71bbdc36a5ad3b9c29792f990')
    ) AS value (id, stable_definition_id, display_name, logical_environment_class, document, content_digest)
ON release.id = 'local-2026-08-10'
    ON CONFLICT (id) DO NOTHING;

-- Every deployment binds to the 'local-2026-08-10' release seeded by
-- db/seed/local-catalog-configuration.sql; no code path writes any other catalog_release_id.

-- environment_definition_versions is immutable by convention, not by constraint: Aurora DSQL rejects
-- CREATE RULE outright, so nothing in the schema blocks an UPDATE or DELETE. The rows inserted above
-- are its only content and everything else reads it.

-- environment_definition_version_id has no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::deployment::queries::environment` reads the environment_definition_versions row
-- live earlier in the same transaction; that read is the only referential guard.
-- Aurora DSQL rejects any constraint (NOT NULL, DEFAULT, or CHECK) inline on ADD COLUMN outright
-- (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN with
-- constraint not supported"), and has no ALTER COLUMN ... SET NOT NULL at all ("unsupported ALTER
-- TABLE ALTER COLUMN ... SET NOT NULL statement"). Every column below is added bare; a default,
-- backfill, and NOT NULL-equivalent/regular CHECK are attached as separate statements once this
-- file's own backfill logic below has populated it -
-- `hive_persistence::migrator::run_add_check_constraint` rewrites each of those CHECK statements into
-- Aurora DSQL's required NOT VALID + VALIDATE CONSTRAINT form automatically.
ALTER TABLE deployments
    ADD COLUMN IF NOT EXISTS environment_definition_version_id UUID,
    ADD COLUMN IF NOT EXISTS request_fingerprint CHAR (64),
    ADD COLUMN IF NOT EXISTS projection_revision BIGINT;
ALTER TABLE deployments ALTER COLUMN projection_revision SET DEFAULT 1;
UPDATE deployments SET projection_revision = 1 WHERE projection_revision IS NULL;
ALTER TABLE deployments ADD CONSTRAINT deployments_projection_revision_nn CHECK (projection_revision IS NOT NULL);
ALTER TABLE deployments ADD CONSTRAINT deployments_projection_revision_check CHECK (projection_revision > 0);
ALTER TABLE deployments ADD CONSTRAINT deployments_request_fingerprint_check CHECK (request_fingerprint ~ '^[0-9a-f]{64}$');
DROP TRIGGER IF EXISTS deployments_frozen_request_trigger ON deployments;
DROP TRIGGER IF EXISTS deployments_environment_catalog_binding_trigger ON deployments;
UPDATE deployments
SET environment_definition_version_id = CASE
                                            WHEN catalog_release_id = 'local-2026-08-10' AND environment = 'DEVELOPMENT'
                                                THEN 'e1300000-0000-0000-0000-000000000001'::uuid
                                            WHEN catalog_release_id = 'local-2026-08-10' AND environment = 'STAGING'
                                                THEN 'e1300000-0000-0000-0000-000000000002'::uuid
                                            WHEN catalog_release_id = 'local-2026-08-10' AND environment = 'PRODUCTION'
                                                THEN 'e1300000-0000-0000-0000-000000000003'::uuid
                                            ELSE (
                                                substr(md5(catalog_release_id || ':' || environment), 1, 8) || '-' ||
                                                substr(md5(catalog_release_id || ':' || environment), 9, 4) || '-' ||
                                                substr(md5(catalog_release_id || ':' || environment), 13, 4) || '-' ||
                                                substr(md5(catalog_release_id || ':' || environment), 17, 4) || '-' ||
                                                substr(md5(catalog_release_id || ':' || environment), 21, 12)
                                                )::uuid
    END
WHERE environment_definition_version_id IS NULL;
ALTER TABLE deployments
    ADD CONSTRAINT deployments_env_def_version_id_nn CHECK (environment_definition_version_id IS NOT NULL);
ALTER TABLE deployments DROP CONSTRAINT IF EXISTS deployments_strategy_check;
UPDATE deployments
SET strategy = 'ROLLING'
WHERE strategy = 'LOCAL_FAILURE';
ALTER TABLE deployments
    ADD CONSTRAINT deployments_strategy_check CHECK (strategy IN ('REPLACE', 'ROLLING', 'CANARY', 'BLUE_GREEN'));
ALTER TABLE deployments DROP CONSTRAINT IF EXISTS deployments_lifecycle_status_check;
UPDATE deployments
SET lifecycle_status = CASE lifecycle_status
                           WHEN 'WAITING' THEN 'REQUESTED'
                           WHEN 'EXECUTING' THEN 'IN_PROGRESS'
                           WHEN 'SUCCEEDED' THEN 'ACTIVE'
                           ELSE lifecycle_status
    END;
ALTER TABLE deployments
    ADD CONSTRAINT deployments_lifecycle_status_check CHECK (
        lifecycle_status IN
        ('REQUESTED', 'AWAITING_APPROVAL', 'APPROVED', 'IN_PROGRESS', 'ACTIVE', 'FAILED', 'CANCELED', 'ROLLED_BACK')
        );
-- No backfill precedes this check: `hive_persistence::deployment::writes::insert_deployment` is the
-- only writer of deployments and always sets request_fingerprint itself.
ALTER TABLE deployments
    ADD CONSTRAINT deployments_request_fingerprint_nn CHECK (request_fingerprint IS NOT NULL);
CREATE INDEX IF NOT EXISTS deployments_project_keyset ON deployments (project_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployments_project_agent_keyset ON deployments (project_id, agent_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployments_project_version_keyset ON deployments (project_id, agent_version_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployments_project_environment_keyset ON deployments (project_id, environment_definition_version_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployments_project_lifecycle_keyset ON deployments (project_id, lifecycle_status, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployments_project_strategy_keyset ON deployments (project_id, strategy, requested_at DESC, id DESC);
-- Both are full indexes: Aurora DSQL rejects CREATE INDEX ... WHERE outright.
-- deployments_active_environment_target keeps lifecycle_status as an equality column alongside the
-- other WHERE predicates of `hive_persistence::deployment::queries::active_target` rather than as a
-- partial filter.
CREATE INDEX IF NOT EXISTS deployments_active_environment_target
    ON deployments (project_id, agent_id, environment_definition_version_id, lifecycle_status, updated_at DESC);
CREATE INDEX IF NOT EXISTS deployments_pending_requester ON deployments (project_id, requested_by, requested_at DESC);

-- environment_definition_version_id has no FOREIGN KEY: Aurora DSQL does not support them.
-- `hive_persistence::deployment::writes::insert_plan` sets it from the environment row that
-- `hive_persistence::deployment::queries::environment` read live earlier in the same transaction;
-- that read is the only referential guard.
ALTER TABLE deployment_plan_versions
    ADD COLUMN IF NOT EXISTS environment_definition_version_id UUID,
    ADD COLUMN IF NOT EXISTS agent_content_digest CHAR (64),
    ADD COLUMN IF NOT EXISTS catalog_release_digest CHAR (64),
    ADD COLUMN IF NOT EXISTS target_digest CHAR (64);
ALTER TABLE deployment_plan_versions ADD CONSTRAINT deployment_plan_versions_agent_content_digest_ck CHECK (agent_content_digest ~ '^[0-9a-f]{64}$');
ALTER TABLE deployment_plan_versions ADD CONSTRAINT deployment_plan_versions_catalog_release_digest_ck CHECK (catalog_release_digest ~ '^[0-9a-f]{64}$');
ALTER TABLE deployment_plan_versions ADD CONSTRAINT deployment_plan_versions_target_digest_ck CHECK (target_digest ~ '^[0-9a-f]{64}$');
UPDATE deployment_plan_versions plan
SET environment_definition_version_id = deployment.environment_definition_version_id,
    agent_content_digest              = version.content_digest,
    catalog_release_digest            = deployment.catalog_release_digest,
    target_digest                     = deployment.target_digest FROM deployments deployment JOIN agent_versions version
ON version.id = deployment.agent_version_id
WHERE plan.deployment_id = deployment.id AND plan.environment_definition_version_id IS NULL;
ALTER TABLE deployment_plan_versions
    ADD CONSTRAINT deployment_plan_versions_env_def_version_id_nn CHECK (environment_definition_version_id IS NOT NULL);
ALTER TABLE deployment_plan_versions
    ADD CONSTRAINT deployment_plan_versions_agent_content_digest_nn CHECK (agent_content_digest IS NOT NULL);
ALTER TABLE deployment_plan_versions
    ADD CONSTRAINT deployment_plan_versions_catalog_release_digest_nn CHECK (catalog_release_digest IS NOT NULL);
ALTER TABLE deployment_plan_versions
    ADD CONSTRAINT deployment_plan_versions_target_digest_nn CHECK (target_digest IS NOT NULL);

-- agent_version_id and environment_definition_version_id have no FOREIGN KEY: Aurora DSQL does not
-- support them. `hive_persistence::deployment::writes::insert_policy_snapshot` sets both from rows
-- that `version_source` and `environment` in `hive_persistence::deployment::queries` read live earlier
-- in the same transaction; those reads are the only referential guards.
ALTER TABLE deployment_policy_snapshots
    ADD COLUMN IF NOT EXISTS agent_version_id UUID,
    ADD COLUMN IF NOT EXISTS environment_definition_version_id UUID,
    ADD COLUMN IF NOT EXISTS target_digest CHAR (64),
    ADD COLUMN IF NOT EXISTS plan_digest CHAR (64),
    ADD COLUMN IF NOT EXISTS package_digest CHAR (64),
    ADD COLUMN IF NOT EXISTS binding_digest CHAR (64),
    ADD COLUMN IF NOT EXISTS evaluation_requirement_expires_at TIMESTAMPTZ;
ALTER TABLE deployment_policy_snapshots ADD CONSTRAINT deployment_policy_snapshots_target_digest_ck CHECK (target_digest ~ '^[0-9a-f]{64}$');
ALTER TABLE deployment_policy_snapshots ADD CONSTRAINT deployment_policy_snapshots_plan_digest_ck CHECK (plan_digest ~ '^[0-9a-f]{64}$');
ALTER TABLE deployment_policy_snapshots ADD CONSTRAINT deployment_policy_snapshots_package_digest_ck CHECK (package_digest ~ '^[0-9a-f]{64}$');
ALTER TABLE deployment_policy_snapshots ADD CONSTRAINT deployment_policy_snapshots_binding_digest_ck CHECK (binding_digest ~ '^[0-9a-f]{64}$');
-- No backfill precedes these checks: `hive_persistence::deployment::writes::insert_policy_snapshot` is
-- this table's only writer and sets every one of these columns itself, binding_digest included, which
-- the application computes rather than SQL.
ALTER TABLE deployment_policy_snapshots
    ADD CONSTRAINT deployment_policy_snapshots_agent_version_id_nn CHECK (agent_version_id IS NOT NULL);
ALTER TABLE deployment_policy_snapshots
    ADD CONSTRAINT deployment_policy_snapshots_env_def_version_id_nn CHECK (environment_definition_version_id IS NOT NULL);
ALTER TABLE deployment_policy_snapshots
    ADD CONSTRAINT deployment_policy_snapshots_target_digest_nn CHECK (target_digest IS NOT NULL);
ALTER TABLE deployment_policy_snapshots
    ADD CONSTRAINT deployment_policy_snapshots_plan_digest_nn CHECK (plan_digest IS NOT NULL);
ALTER TABLE deployment_policy_snapshots
    ADD CONSTRAINT deployment_policy_snapshots_package_digest_nn CHECK (package_digest IS NOT NULL);
ALTER TABLE deployment_policy_snapshots
    ADD CONSTRAINT deployment_policy_snapshots_binding_digest_nn CHECK (binding_digest IS NOT NULL);

-- The columns added below carry no REFERENCES clause: Aurora DSQL rejects REFERENCES outright; see
-- deployment_evidence_snapshots' declaration in V014 for what guards these references instead.
ALTER TABLE deployment_evidence_snapshots
    ADD COLUMN IF NOT EXISTS agent_version_id UUID,
    ADD COLUMN IF NOT EXISTS environment_definition_version_id UUID,
    ADD COLUMN IF NOT EXISTS target_digest CHAR (64),
    ADD COLUMN IF NOT EXISTS plan_digest CHAR (64),
    ADD COLUMN IF NOT EXISTS package_digest CHAR (64),
    ADD COLUMN IF NOT EXISTS binding_digest CHAR (64);
ALTER TABLE deployment_evidence_snapshots ADD CONSTRAINT deployment_evidence_snapshots_target_digest_ck CHECK (target_digest ~ '^[0-9a-f]{64}$');
ALTER TABLE deployment_evidence_snapshots ADD CONSTRAINT deployment_evidence_snapshots_plan_digest_ck CHECK (plan_digest ~ '^[0-9a-f]{64}$');
ALTER TABLE deployment_evidence_snapshots ADD CONSTRAINT deployment_evidence_snapshots_package_digest_ck CHECK (package_digest ~ '^[0-9a-f]{64}$');
ALTER TABLE deployment_evidence_snapshots ADD CONSTRAINT deployment_evidence_snapshots_binding_digest_ck CHECK (binding_digest ~ '^[0-9a-f]{64}$');
-- No backfill precedes these checks: `hive_persistence::deployment::writes::insert_evidence` sets
-- every one of these columns itself, binding_digest and evidence_digest included, which the
-- application computes rather than SQL.
ALTER TABLE deployment_evidence_snapshots
    ADD CONSTRAINT deployment_evidence_snapshots_agent_version_id_nn CHECK (agent_version_id IS NOT NULL);
ALTER TABLE deployment_evidence_snapshots
    ADD CONSTRAINT deployment_evidence_snapshots_env_def_version_id_nn CHECK (environment_definition_version_id IS NOT NULL);
ALTER TABLE deployment_evidence_snapshots
    ADD CONSTRAINT deployment_evidence_snapshots_target_digest_nn CHECK (target_digest IS NOT NULL);
ALTER TABLE deployment_evidence_snapshots
    ADD CONSTRAINT deployment_evidence_snapshots_plan_digest_nn CHECK (plan_digest IS NOT NULL);
ALTER TABLE deployment_evidence_snapshots
    ADD CONSTRAINT deployment_evidence_snapshots_package_digest_nn CHECK (package_digest IS NOT NULL);
ALTER TABLE deployment_evidence_snapshots
    ADD CONSTRAINT deployment_evidence_snapshots_binding_digest_nn CHECK (binding_digest IS NOT NULL);

ALTER TABLE deployment_audit_events DROP CONSTRAINT IF EXISTS deployment_audit_events_action_check;
ALTER TABLE deployment_audit_events
    ADD CONSTRAINT deployment_audit_events_action_check CHECK (
    action IN ('REQUESTED', 'CANCELED', 'EXECUTION_STARTED', 'EXECUTION_SUCCEEDED', 'EXECUTION_FAILED', 'OUTBOX_DEAD_LETTERED', 'OUTBOX_LEASE_RECLAIMED')
    );
ALTER TABLE deployment_stage_events
    ADD COLUMN IF NOT EXISTS timeline_sequence BIGINT;
ALTER TABLE deployment_stage_events ADD CONSTRAINT deployment_stage_events_timeline_sequence_ck CHECK (timeline_sequence > 0);
-- deployment_attempt_id has no FOREIGN KEY: Aurora DSQL does not support them. Every call to
-- `hive_persistence::deployment::writes::audit` resolves this column through `timeline_anchor`, which
-- re-verifies any caller-supplied attempt id against a live query scoped to the same deployment
-- (falling back to the latest real attempt, or NULL) just before the INSERT in the same transaction;
-- that check is the only referential guard.
ALTER TABLE deployment_audit_events
    ADD COLUMN IF NOT EXISTS deployment_attempt_id UUID,
    ADD COLUMN IF NOT EXISTS attempt_number BIGINT,
    ADD COLUMN IF NOT EXISTS timeline_sequence BIGINT;
ALTER TABLE deployment_audit_events ADD CONSTRAINT deployment_audit_events_attempt_number_ck CHECK (attempt_number >= 0);
-- No interim timeline_sequence >= 0 check here (unlike attempt_number's, which is permanent): the
-- backfill below runs within this same migration file and is immediately followed by a strictly
-- tighter deployment_audit_events_timeline_sequence_check (> 0), so an interim >= 0 check would only
-- ever be observed for the instant between this ADD COLUMN and that later ADD CONSTRAINT.
UPDATE deployment_audit_events audit
SET deployment_attempt_id = attempt.id,
    attempt_number        = attempt.attempt_number FROM deployment_attempts attempt
WHERE audit.deployment_attempt_id IS NULL AND NULLIF (audit.facts ->> 'attemptId', '')::uuid = attempt.id;
UPDATE deployment_audit_events
SET attempt_number = 0
WHERE attempt_number IS NULL;
-- Reconstruct one cross-table event sequence per deployment attempt. Stage and audit facts live in
-- separate tables, so independent row numbers would make cursor order depend on UUID ties.
WITH unified AS (SELECT 'STAGE' AS source, event.id, attempt.deployment_id, attempt.attempt_number, event.occurred_at
                 FROM deployment_stage_events event
                          JOIN deployment_attempts attempt ON attempt.id = event.deployment_attempt_id
                 UNION ALL
                 SELECT 'AUDIT' AS source, audit.id, audit.deployment_id, audit.attempt_number, audit.occurred_at
                 FROM deployment_audit_events audit),
     sequenced AS (SELECT source,
                          id,
                          ROW_NUMBER() OVER (
    PARTITION BY deployment_id, attempt_number ORDER BY occurred_at ASC, id ASC) AS sequence
                   FROM unified)
UPDATE deployment_stage_events event
SET timeline_sequence = sequenced.sequence FROM sequenced
WHERE sequenced.source = 'STAGE' AND event.id = sequenced.id;
WITH unified AS (SELECT 'STAGE' AS source, event.id, attempt.deployment_id, attempt.attempt_number, event.occurred_at
                 FROM deployment_stage_events event
                          JOIN deployment_attempts attempt ON attempt.id = event.deployment_attempt_id
                 UNION ALL
                 SELECT 'AUDIT' AS source, audit.id, audit.deployment_id, audit.attempt_number, audit.occurred_at
                 FROM deployment_audit_events audit),
     sequenced AS (SELECT source,
                          id,
                          ROW_NUMBER() OVER (
    PARTITION BY deployment_id, attempt_number ORDER BY occurred_at ASC, id ASC) AS sequence
                   FROM unified)
UPDATE deployment_audit_events audit
SET timeline_sequence = sequenced.sequence FROM sequenced
WHERE sequenced.source = 'AUDIT' AND audit.id = sequenced.id;
ALTER TABLE deployment_stage_events
    ADD CONSTRAINT deployment_stage_events_timeline_sequence_nn CHECK (timeline_sequence IS NOT NULL);
ALTER TABLE deployment_audit_events
    ADD CONSTRAINT deployment_audit_events_attempt_number_nn CHECK (attempt_number IS NOT NULL);
ALTER TABLE deployment_audit_events
    ADD CONSTRAINT deployment_audit_events_timeline_sequence_nn CHECK (timeline_sequence IS NOT NULL);
ALTER TABLE deployment_audit_events
    ADD CONSTRAINT deployment_audit_events_timeline_sequence_check CHECK (timeline_sequence > 0);
CREATE INDEX IF NOT EXISTS deployment_stage_events_timeline ON deployment_stage_events (deployment_attempt_id, timeline_sequence, id);
CREATE INDEX IF NOT EXISTS deployment_audit_events_timeline ON deployment_audit_events (deployment_id, attempt_number, timeline_sequence, id);
-- deployment_id has no FOREIGN KEY: Aurora DSQL does not support them. The one insert site
-- (`next_timeline_sequence` in `hive_persistence::deployment::writes`, initializing a deployment's
-- first timeline counter) always runs with a deployment_id loaded or locked earlier in the same
-- transaction; that is the only referential guard.
CREATE TABLE IF NOT EXISTS deployment_timeline_counters
(
    deployment_id
    UUID
    NOT
    NULL,
    attempt_number BIGINT NOT NULL CHECK
(
    attempt_number
    >=
    0
),
    next_sequence BIGINT NOT NULL CHECK
(
    next_sequence >
    0
),
    PRIMARY KEY
(
    deployment_id,
    attempt_number
)
    );
WITH unified AS (SELECT attempt.deployment_id, attempt.attempt_number, event.timeline_sequence
                 FROM deployment_stage_events event
                          JOIN deployment_attempts attempt ON attempt.id = event.deployment_attempt_id
                 UNION ALL
                 SELECT deployment_id, attempt_number, timeline_sequence
                 FROM deployment_audit_events)
INSERT
INTO deployment_timeline_counters (deployment_id, attempt_number, next_sequence)
SELECT deployment_id, attempt_number, MAX(timeline_sequence) + 1
FROM unified
GROUP BY deployment_id, attempt_number ON CONFLICT (deployment_id, attempt_number) DO
UPDATE
    SET next_sequence = GREATEST(deployment_timeline_counters.next_sequence, EXCLUDED.next_sequence);

CREATE TABLE IF NOT EXISTS deployment_worker_heartbeats
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
    last_batch_deliveries INTEGER NOT NULL CHECK
(
    last_batch_deliveries
    >=
    0
),
    pending_events INTEGER NOT NULL CHECK
(
    pending_events
    >=
    0
),
    oldest_pending_at TIMESTAMPTZ NULL,
    state TEXT NOT NULL CHECK
(
    state
    IN
(
    'READY',
    'DEGRADED'
)),
    failure_code TEXT NULL CHECK
(
    failure_code
    IS
    NULL
    OR
    failure_code
    ~
    '^[A-Z][A-Z0-9_]{2,80}$'
)
    );
CREATE INDEX IF NOT EXISTS deployment_worker_heartbeats_observed ON deployment_worker_heartbeats (observed_at DESC);

-- Deployment capability evaluation has no SQL function: Aurora DSQL rejects CREATE FUNCTION outright,
-- even LANGUAGE sql ones. `hive_persistence::capability::deployment_capabilities` decides a
-- principal's deployment capabilities, and `hive_persistence::authority` supplies the matching
-- row-visibility condition that other queries embed.
-- A full index, because Aurora DSQL rejects CREATE INDEX ... WHERE outright.
CREATE INDEX IF NOT EXISTS deployment_outbox_events_reclaim ON deployment_outbox_events (status, claimed_at);
CREATE INDEX IF NOT EXISTS deployment_outbox_events_status_available ON deployment_outbox_events (status, available_at, created_at);

-- deployments has no trigger: Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright, so three
-- rules the schema cannot express are the application's responsibility.
-- Frozen request columns: every UPDATE of deployments sets only lifecycle_status, revision,
-- updated_at, or projection_revision, never a frozen request column.
-- Environment/catalog binding: `hive_persistence::deployment::queries::environment` reads
-- environment_definition_versions scoped to both the environment id and the exact catalog_release_id
-- the request will insert, and refuses the request on any mismatch, so a deployment can never bind an
-- environment belonging to another release.
-- Terminal lifecycle while a nonterminal attempt exists:
-- `hive_persistence::deployment::mutations::cancel` sets the deployment to CANCELED and then calls
-- `hive_persistence::deployment::writes::terminalize_running_attempts`, the reverse of the order
-- `hive_persistence::deployment::worker::dead_letter_state` uses. The revision-keyed UPDATE claims
-- first deliberately, so a lost optimistic-concurrency race leaves no dependent write to roll back;
-- the intermediate state exists only inside one transaction, and by commit time every running attempt
-- is terminal.
