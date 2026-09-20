-- V016 repairs the persisted shapes written by the M13 predecessor migrations before the V015
-- version ledger existed. It derives every mutable compatibility value from retained deployment,
-- immutable agent-version, plan, policy, evidence, and outbox facts.
-- deployments_frozen_request_trigger, deployments_environment_catalog_binding_trigger,
-- deployment_plan_versions_no_update, and deployment_policy_snapshots_no_update are not dropped here:
-- none of the four exist any more (Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION/CREATE RULE
-- outright; see V015's comments), so there is nothing left for any of these four DROPs to find.
-- deployment_legacy_request_facts_no_update/_no_delete's DROPs, and the INSERT below that derived
-- deployment_legacy_request_facts rows from deployment_audit_events for deployments a predecessor
-- migration had already converted from LOCAL_FAILURE to ROLLING, are removed along with the table
-- itself: see V015's removal comment.
-- Databases that recorded V015 before the projection watermark existed keep their V015 marker.
-- V016 supplies that additive column without replaying the V015 historical migration.
-- Aurora DSQL rejects any constraint (NOT NULL, DEFAULT, or CHECK) inline on ADD COLUMN outright
-- (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN with
-- constraint not supported"). Column added bare; default and NOT NULL/CHECK follow as separate
-- statements under the same constraint names V015 uses for this column, so whichever migration a
-- given database actually needed ends up with one consistent pair of constraints, not two - see
-- DatabaseMigrator.runStatement()'s comment for how the CHECK statements reach Aurora DSQL's
-- required NOT VALID + VALIDATE CONSTRAINT form automatically.
ALTER TABLE deployments
    ADD COLUMN IF NOT EXISTS projection_revision BIGINT;
ALTER TABLE deployments ALTER COLUMN projection_revision SET DEFAULT 1;
UPDATE deployments SET projection_revision = 1 WHERE projection_revision IS NULL;
ALTER TABLE deployments ADD CONSTRAINT deployments_projection_revision_nn CHECK (projection_revision IS NOT NULL);
ALTER TABLE deployments ADD CONSTRAINT deployments_projection_revision_check CHECK (projection_revision > 0);

-- Earlier candidates could persist a non-null local environment identifier for another logical
-- environment. Resolve the canonical immutable version from the retained catalog release and
-- logical class, not from the previous identifier.
UPDATE deployments deployment
SET environment_definition_version_id = expected_environment.id FROM environment_definition_versions expected_environment
WHERE expected_environment.catalog_release_id = deployment.catalog_release_id
  AND expected_environment.logical_environment_class = deployment.environment
  AND deployment.environment_definition_version_id IS DISTINCT
FROM expected_environment.id;

-- The request_fingerprint/deployment_plan_versions/deployment_policy_snapshots/
-- deployment_evidence_snapshots backfill UPDATEs that used to run here are removed, not ported: each
-- needed digest()/sha256, unavailable now that Aurora DSQL rejects CREATE EXTENSION pgcrypto outright
-- (confirmed against the real hive-dsql-verification cluster: "unsupported statement:
-- CreateExtension"; see V015's header comment). Removing them needs no Java-side change: this whole
-- file exists only to repair M13-predecessor legacy rows (see this file's header comment), which
-- cannot exist in this greenfield rewrite - every one of these tables is empty at migration time, so
-- each UPDATE ran against zero rows regardless.

-- deployments_frozen_request_trigger, deployments_environment_catalog_binding_trigger,
-- deployment_plan_versions_no_update, and deployment_policy_snapshots_no_update are not restored here:
-- Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION/CREATE RULE outright; see V015's comments for the
-- Java-side replacement (deployment_policy_snapshots_no_update's own trigger equivalents are covered
-- there too).
-- deployment_evidence_snapshots_no_update's redeclaration here is removed, not ported: see V014's
-- removal comment, its original declaration site. deployment_legacy_request_facts_no_update/_no_delete's
-- redeclaration here is removed along with the table itself: see V015's removal comment.
