-- M13 contract alignment keeps local execution deterministic while binding every frozen fact to an
-- immutable environment-definition version. The rows below contain catalog metadata only.
--
-- CREATE EXTENSION pgcrypto is removed, not ported: Aurora DSQL rejects CREATE EXTENSION outright
-- (confirmed against the real hive-dsql-verification cluster: "unsupported statement:
-- CreateExtension"). Every digest()/sha256 call this file used to make through pgcrypto is removed
-- along with it - see each removed call's own comment for why it needs no replacement in this
-- greenfield rewrite.
-- catalog_release_id has no FOREIGN KEY: Aurora DSQL does not support them. This table has no Java
-- write path at all -- it's seeded by the two fixed INSERT statements below, in this same migration --
-- so removing the constraint needs no new Java-side check.
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

-- The legacy-catalog-release backfill INSERT that used to run here (deriving a definition version
-- for any deployments.catalog_release_id other than 'local-2026-08-10') is removed, not ported: Aurora
-- DSQL rejects CREATE EXTENSION outright (confirmed against the real hive-dsql-verification cluster:
-- "unsupported statement: CreateExtension"), so pgcrypto - and the digest()/sha256 it provided - is
-- unavailable, and this INSERT needed digest() to compute its content_digest column. Removing it needs
-- no Java-side change: its own WHERE clause (catalog_release_id <> 'local-2026-08-10') can never match
-- a row in this greenfield rewrite, where every deployment is seeded against that one release (see
-- V011's local-catalog-configuration.sql seed) and no code path writes any other catalog_release_id.

-- environment_definition_versions_no_update/_no_delete are removed, not ported: Aurora DSQL rejects
-- CREATE RULE outright, and no Java code path ever UPDATEs or DELETEs this table -- confirmed by grep,
-- it's seeded by two fixed V015 rows (below) and read-only everywhere else this rewrite touches it.

-- environment_definition_version_id has no FOREIGN KEY: Aurora DSQL does not support them. deploy()'s
-- environment() call reads the environment_definition_versions row live earlier in the same
-- transaction, so removing the constraint needs no new Java-side check.
-- Aurora DSQL rejects any constraint (NOT NULL, DEFAULT, or CHECK) inline on ADD COLUMN outright
-- (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN with
-- constraint not supported"), and has no ALTER COLUMN ... SET NOT NULL at all ("unsupported ALTER
-- TABLE ALTER COLUMN ... SET NOT NULL statement"). Every column below is added bare; a default,
-- backfill, and NOT NULL-equivalent/regular CHECK are attached as separate statements once this
-- file's own backfill logic below has populated it - see DatabaseMigrator.runStatement()'s comment
-- for how the CHECK statements reach Aurora DSQL's required NOT VALID + VALIDATE CONSTRAINT form
-- automatically.
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
-- deployment_legacy_request_facts is removed outright, not ported: it existed solely to record a
-- one-time compatibility transform (LOCAL_FAILURE_TO_ROLLING) for deployments a live database could
-- have accumulated with the since-removed 'LOCAL_FAILURE' strategy value before this migration ran --
-- structurally impossible in this greenfield rewrite, where deployments.strategy's current CHECK
-- constraint (see below) never allows that value to be written in the first place. The backfill INSERT
-- this table existed to receive is removed with it, for the same reason.
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
-- The request_fingerprint backfill UPDATE that used to run here is removed, not ported: it needed
-- digest()/sha256, unavailable now that CREATE EXTENSION pgcrypto is rejected (see this file's header
-- comment). Removing it needs no Java-side change: its own WHERE clause (request_fingerprint IS NULL)
-- can never match a row in this greenfield rewrite, where deployments only ever gets rows from
-- insertPlan() (PostgresDeploymentRepository), which always sets request_fingerprint itself.
ALTER TABLE deployments
    ADD CONSTRAINT deployments_request_fingerprint_nn CHECK (request_fingerprint IS NOT NULL);
CREATE INDEX IF NOT EXISTS deployments_project_keyset ON deployments (project_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployments_project_agent_keyset ON deployments (project_id, agent_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployments_project_version_keyset ON deployments (project_id, agent_version_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployments_project_environment_keyset ON deployments (project_id, environment_definition_version_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployments_project_lifecycle_keyset ON deployments (project_id, lifecycle_status, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployments_project_strategy_keyset ON deployments (project_id, strategy, requested_at DESC, id DESC);
-- Both widened to full indexes: Aurora DSQL rejects CREATE INDEX ... WHERE outright (confirmed against
-- the real cluster in Phase 0). deployments_active_environment_target keeps lifecycle_status as an
-- equality column alongside activeTarget()'s other WHERE predicates rather than a partial filter.
CREATE INDEX IF NOT EXISTS deployments_active_environment_target
    ON deployments (project_id, agent_id, environment_definition_version_id, lifecycle_status, updated_at DESC);
CREATE INDEX IF NOT EXISTS deployments_pending_requester ON deployments (project_id, requested_by, requested_at DESC);

-- environment_definition_version_id has no FOREIGN KEY: Aurora DSQL does not support them. insertPlan()
-- sets it from request.environment().id(), read live via deploy()'s environment() call earlier in the
-- same transaction, so removing the constraint needs no new Java-side check.
ALTER TABLE deployment_plan_versions
    ADD COLUMN IF NOT EXISTS environment_definition_version_id UUID,
    ADD COLUMN IF NOT EXISTS agent_content_digest CHAR (64),
    ADD COLUMN IF NOT EXISTS catalog_release_digest CHAR (64),
    ADD COLUMN IF NOT EXISTS target_digest CHAR (64);
ALTER TABLE deployment_plan_versions ADD CONSTRAINT deployment_plan_versions_agent_content_digest_ck CHECK (agent_content_digest ~ '^[0-9a-f]{64}$');
ALTER TABLE deployment_plan_versions ADD CONSTRAINT deployment_plan_versions_catalog_release_digest_ck CHECK (catalog_release_digest ~ '^[0-9a-f]{64}$');
ALTER TABLE deployment_plan_versions ADD CONSTRAINT deployment_plan_versions_target_digest_ck CHECK (target_digest ~ '^[0-9a-f]{64}$');
-- Harmless no-op: deployment_plan_versions_no_update is never created (Aurora DSQL rejects CREATE RULE
-- outright; see V014's comment), so there is nothing here for this DROP to find.
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
-- support them. insertPolicySnapshot() sets both from request.version().id()/request.environment().id(),
-- read live earlier in the same transaction (versionSource()/environment()), so removing the
-- constraints needs no new Java-side check.
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
-- Harmless no-op: deployment_policy_snapshots_no_update is never created (Aurora DSQL rejects CREATE
-- RULE outright; see V014's comment), so there is nothing here for this DROP to find.
-- The deployment_policy_snapshots backfill UPDATE that used to run here is removed, not ported: it
-- needed digest()/sha256, unavailable now that CREATE EXTENSION pgcrypto is rejected (see this file's
-- header comment). Removing it needs no Java-side change: its own WHERE clause (matching a policy row
-- with any of these columns still NULL) can never match a row in this greenfield rewrite, where
-- deployment_policy_snapshots only ever gets rows from insertPolicySnapshot()
-- (PostgresDeploymentRepository), which always sets every one of these columns itself, binding_digest
-- included (computed in Java, not SQL).
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

-- These REFERENCES clauses are removed, not ported: Aurora DSQL rejects REFERENCES outright; see
-- V014's removal comment, deployment_evidence_snapshots' original declaration site, for the reasoning.
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
-- The deployment_evidence_snapshots backfill UPDATE that used to run here is removed, not ported: it
-- needed digest()/sha256, unavailable now that CREATE EXTENSION pgcrypto is rejected (see this file's
-- header comment). Removing it needs no Java-side change: its own WHERE clause (matching an evidence
-- row with any of these columns still NULL) can never match a row in this greenfield rewrite, where
-- deployment_evidence_snapshots only ever gets rows from the evidence-insert path
-- (PostgresDeploymentRepository), which always sets every one of these columns itself, binding_digest
-- and evidence_digest included (computed in Java, not SQL).
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
-- deployment_attempt_id has no FOREIGN KEY: Aurora DSQL does not support them. Every audit() call
-- resolves this column through timelineAnchor(), which re-verifies any caller-supplied attempt id with
-- a live SELECT ... WHERE id = ? AND deployment_id = ? (falling back to the latest real attempt, or
-- NULL) moments before the INSERT in the same transaction -- so removing the constraint needs no new
-- Java-side check.
ALTER TABLE deployment_audit_events
    ADD COLUMN IF NOT EXISTS deployment_attempt_id UUID,
    ADD COLUMN IF NOT EXISTS attempt_number BIGINT,
    ADD COLUMN IF NOT EXISTS timeline_sequence BIGINT;
ALTER TABLE deployment_audit_events ADD CONSTRAINT deployment_audit_events_attempt_number_ck CHECK (attempt_number >= 0);
-- No interim timeline_sequence >= 0 check here (unlike attempt_number's, which is permanent): the
-- backfill below runs within this same migration file and is immediately followed by a strictly
-- tighter deployment_audit_events_timeline_sequence_check (> 0), so an interim >= 0 check would only
-- ever be observed for the instant between this ADD COLUMN and that later ADD CONSTRAINT.
-- Both harmless no-ops: neither rule is ever created (Aurora DSQL rejects CREATE RULE outright; see
-- V014's comment on each table), so there is nothing here for either DROP to find.
UPDATE deployment_audit_events audit
SET deployment_attempt_id = attempt.id,
    attempt_number        = attempt.attempt_number FROM deployment_attempts attempt
WHERE audit.deployment_attempt_id IS NULL AND NULLIF (audit.facts ->> 'attemptId', '')::uuid = attempt.id;
UPDATE deployment_audit_events
SET attempt_number = 0
WHERE attempt_number IS NULL;
-- Reconstruct one cross-table event sequence per deployment attempt. V014 kept stage and audit
-- facts separately, so independent row numbers would make cursor order depend on UUID ties.
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
-- deployment_stage_events_no_update and deployment_audit_events_no_update are not restored here:
-- Aurora DSQL rejects CREATE RULE outright, and V014 no longer creates either one (see that
-- migration's comments).
CREATE INDEX IF NOT EXISTS deployment_stage_events_timeline ON deployment_stage_events (deployment_attempt_id, timeline_sequence, id);
CREATE INDEX IF NOT EXISTS deployment_audit_events_timeline ON deployment_audit_events (deployment_id, attempt_number, timeline_sequence, id);
-- deployment_id has no FOREIGN KEY: Aurora DSQL does not support them. The one Java insert site
-- (initializing this deployment's first timeline counter) always runs with a deployment_id that was
-- already loaded/locked earlier in the same transaction, so removing the constraint needs no new
-- Java-side check.
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

-- effective_deployment_capabilities() is removed, not ported as SQL: Aurora DSQL rejects CREATE
-- FUNCTION outright, even LANGUAGE sql ones. Its logic now lives in Java --
-- PostgresEffectiveCapabilityEvaluator.deploymentCapabilities() (recomposed from this class's existing
-- role-check primitives) and deploymentViewPredicate() (the reader condition inlined as a correlated
-- subquery, since that predicate is embedded into other queries built elsewhere and has to stay SQL).
-- Widened to a full index: Aurora DSQL rejects CREATE INDEX ... WHERE outright.
CREATE INDEX IF NOT EXISTS deployment_outbox_events_reclaim ON deployment_outbox_events (status, claimed_at);
CREATE INDEX IF NOT EXISTS deployment_outbox_events_status_available ON deployment_outbox_events (status, available_at, created_at);

-- deployment_frozen_request_columns() and deployment_environment_catalog_binding() (BEFORE UPDATE /
-- BEFORE INSERT OR UPDATE triggers on deployments) are removed, not ported: Aurora DSQL rejects
-- CREATE TRIGGER/CREATE FUNCTION outright, and neither trigger's guard is reachable from the current
-- Java write path. deploy()'s environment() call already reads environment_definition_versions scoped
-- to the exact catalog_release_id it will insert (WHERE id = ? AND catalog_release_id = ?), returning
-- null -- and refusing the request -- on any mismatch, so deployment_environment_catalog_binding()'s
-- check can never fire. deployment_frozen_request_columns()'s frozen-column guard never fires either:
-- every UPDATE deployments statement in PostgresDeploymentRepository.java sets only lifecycle_status,
-- revision, updated_at, or projection_revision, never any of the columns it protects. Its second
-- guard -- reject a terminal lifecycle_status while a nonterminal deployment_attempts row still exists
-- -- found one real site that would have reached it under the real trigger: cancel() updates deployments
-- to CANCELED, then terminalizes running attempts, the reverse of deadLetterState()'s order (terminalize
-- first, then update deployments). Left as-is, not reordered or ported: cancel()'s own comment already
-- explains why this order is deliberate (the revision-keyed UPDATE claims first so a lost OCC race
-- leaves no dependent write to roll back), and the guard's intermediate-state check only ever mattered
-- within a single transaction no other reader can observe before it commits -- by commit time,
-- terminalizeRunningAttempts() has already run, so the invariant the guard protected still holds for
-- every reader outside this transaction. Not a live concern under DSQL either way, which never enforced
-- this guard for anything created after Phase 0 removed the equivalent pg_advisory_lock-adjacent
-- pessimistic assumptions elsewhere.
-- deployment_evidence_snapshots_no_update's redeclaration here is removed, not ported: Aurora DSQL
-- rejects CREATE RULE outright; deployment_evidence_snapshots is Deployment/approval's own table
-- (sub-step (b)) -- see V014's removal comment, its original declaration site, for the reasoning.
