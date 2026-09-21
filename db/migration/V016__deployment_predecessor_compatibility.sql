-- V016 repairs rows persisted before V015 established the version ledger. It derives every mutable
-- compatibility value from retained deployment, agent-version, plan, policy, evidence, and outbox
-- facts.
-- A database that recorded V015 before the projection watermark existed keeps its V015 marker, so V016
-- supplies that additive column without replaying V015.
-- Aurora DSQL rejects any constraint (NOT NULL, DEFAULT, or CHECK) inline on ADD COLUMN outright
-- (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN with
-- constraint not supported"). Column added bare; default and NOT NULL/CHECK follow as separate
-- statements under the same constraint names V015 uses for this column, so whichever migration a
-- given database actually needed ends up with one consistent pair of constraints, not two -
-- `hive_persistence::migrator::run_add_check_constraint` rewrites each of those CHECK statements into
-- Aurora DSQL's required NOT VALID + VALIDATE CONSTRAINT form automatically.
ALTER TABLE deployments
    ADD COLUMN IF NOT EXISTS projection_revision BIGINT;
ALTER TABLE deployments ALTER COLUMN projection_revision SET DEFAULT 1;
UPDATE deployments SET projection_revision = 1 WHERE projection_revision IS NULL;
ALTER TABLE deployments ADD CONSTRAINT deployments_projection_revision_nn CHECK (projection_revision IS NOT NULL);
ALTER TABLE deployments ADD CONSTRAINT deployments_projection_revision_check CHECK (projection_revision > 0);

-- A row persisted earlier can carry a non-null local environment identifier belonging to another
-- logical environment. Resolve the canonical immutable version from the retained catalog release and
-- logical class, not from the stored identifier.
UPDATE deployments deployment
SET environment_definition_version_id = expected_environment.id FROM environment_definition_versions expected_environment
WHERE expected_environment.catalog_release_id = deployment.catalog_release_id
  AND expected_environment.logical_environment_class = deployment.environment
  AND deployment.environment_definition_version_id IS DISTINCT
FROM expected_environment.id;
