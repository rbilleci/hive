-- Browser-safe review facts live here, separate from the opaque compiler plan. plan_id has no
-- FOREIGN KEY: Aurora DSQL rejects REFERENCES outright. `insert_plan_review` runs immediately after
-- `insert_plan` -- the only INSERT INTO deployment_plan_versions -- in the same transaction, at both
-- of that function's call sites.
CREATE TABLE IF NOT EXISTS deployment_plan_review_facts
(
    plan_id
    UUID
    PRIMARY
    KEY,
    active_agent_version_number BIGINT NULL CHECK
(
    active_agent_version_number
    IS
    NULL
    OR
    active_agent_version_number >
    0
),
    change_summary TEXT NOT NULL,
    -- JSONB (a JSON array of strings), not TEXT[]: Aurora DSQL does not support array types at all
    -- ("datatype text[] not supported"). The application (de)serializes these three columns itself.
    requested_dependency_versions JSONB NOT NULL,
    added_dependency_versions JSONB NOT NULL,
    removed_dependency_versions JSONB NOT NULL
    );

-- Aurora DSQL rejects CREATE TRIGGER, CREATE FUNCTION, and CREATE RULE outright, so nothing in the
-- database backfills this table, cross-checks it against deployment_plan_versions, or freezes it
-- against change. `insert_plan_review` is its only writer -- a single INSERT, never an UPDATE or
-- DELETE -- and it runs unconditionally after every `insert_plan`, so no deployment_plan_versions row
-- can exist without its review facts.

-- Approval-execution eligibility is decided in application code, by `approval_execution_eligible`,
-- for the same reason: Aurora DSQL has no user-defined functions.
