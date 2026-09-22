-- A database can have recorded V015 or V016 before the deployment projection watermark existed. This
-- additive preflight supplies the column before V017 needs it, without replaying immutable request,
-- plan, policy, evidence, or decision facts.
-- Aurora DSQL rejects any constraint (NOT NULL, DEFAULT, or CHECK) inline on ADD COLUMN outright
-- (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN with
-- constraint not supported"). Column added bare; default and NOT NULL/CHECK follow as separate
-- statements under the same constraint names V015 uses for this column -
-- `hive_persistence::migrator::run_add_check_constraint` rewrites each of those CHECK statements into
-- Aurora DSQL's required NOT VALID + VALIDATE CONSTRAINT form automatically.
ALTER TABLE deployments
    ADD COLUMN IF NOT EXISTS projection_revision BIGINT;
ALTER TABLE deployments ALTER COLUMN projection_revision SET DEFAULT 1;
UPDATE deployments SET projection_revision = 1 WHERE projection_revision IS NULL;
ALTER TABLE deployments ADD CONSTRAINT deployments_projection_revision_nn CHECK (projection_revision IS NOT NULL);
ALTER TABLE deployments ADD CONSTRAINT deployments_projection_revision_check CHECK (projection_revision > 0);
