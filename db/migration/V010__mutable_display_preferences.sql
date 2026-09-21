-- Display preferences are private mutable ergonomics, not auditable or revisioned domain facts.
ALTER TABLE principal_display_preferences DROP COLUMN IF EXISTS revision;
ALTER TABLE principal_display_preferences DROP COLUMN IF EXISTS updated_at;
-- Aurora DSQL rejects any constraint (NOT NULL, DEFAULT, or CHECK) inline on ADD COLUMN outright
-- (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN with
-- constraint not supported"), and has no ALTER COLUMN ... SET NOT NULL at all ("unsupported ALTER
-- TABLE ALTER COLUMN ... SET NOT NULL statement"). The column is added bare, then a default and a
-- NOT NULL-equivalent CHECK are attached as separate statements -
-- `hive_persistence::migrator::run_add_check_constraint` rewrites each of those CHECK statements into
-- Aurora DSQL's required NOT VALID + VALIDATE CONSTRAINT form automatically.
ALTER TABLE principal_display_preferences
    ADD COLUMN IF NOT EXISTS sidebar_state TEXT;
ALTER TABLE principal_display_preferences ALTER COLUMN sidebar_state SET DEFAULT 'EXPANDED';
UPDATE principal_display_preferences SET sidebar_state = 'EXPANDED' WHERE sidebar_state IS NULL;
ALTER TABLE principal_display_preferences
    ADD CONSTRAINT principal_display_preferences_sidebar_state_not_null CHECK (sidebar_state IS NOT NULL);
ALTER TABLE principal_display_preferences
    ADD CONSTRAINT principal_display_preferences_sidebar_state_check CHECK (sidebar_state IN ('EXPANDED', 'COLLAPSED'));
