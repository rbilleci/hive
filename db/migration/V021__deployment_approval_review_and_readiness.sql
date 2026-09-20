-- M14 persists browser-safe review facts separately from the opaque compiler plan and gates all
-- approval execution on completion of retained compatibility projections. The FK on plan_id is
-- removed, not ported: Aurora DSQL rejects REFERENCES outright, and insertPlanReview()
-- (PostgresDeploymentRepository.java) is always called immediately after insertPlan() -- the only
-- INSERT INTO deployment_plan_versions -- in the same transaction, at both of insertPlan()'s own call
-- sites.
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
    -- (confirmed against the real hive-dsql-verification cluster: "datatype text[] not supported").
    -- See PostgresDeploymentRepository's insertPlanReview()/deploymentRow() for the Java-side
    -- (de)serialization this requires.
    requested_dependency_versions JSONB NOT NULL,
    added_dependency_versions JSONB NOT NULL,
    removed_dependency_versions JSONB NOT NULL
    );

-- deployment_approval_review_backfill_progress and deployment_approval_review_facts_page() are
-- removed outright, not ported: see PostgresDeploymentRepository.reconcileApprovalUpgrade()'s comment
-- for the shared reasoning -- both existed to catch up deployment_plan_versions rows a live database
-- could have accumulated before this migration ran, which cannot happen in this greenfield rewrite;
-- insertPlanReview() already writes deployment_plan_review_facts synchronously for every plan.
-- deployment_approval_plan_review_compatibility_trigger (AFTER INSERT ON deployment_plan_versions)
-- and its function are removed, not ported: Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION
-- outright, and this "a retained M13 writer" guard has no such writer to guard against in this
-- rewrite -- insertPlan() (PostgresDeploymentRepository.java) is the only INSERT INTO
-- deployment_plan_versions, and both of its own call sites already call insertPlanReview()
-- immediately afterward, unconditionally, in the same transaction.
-- deployment_plan_review_facts_no_update/_no_delete are removed, not ported: Aurora DSQL rejects
-- CREATE RULE outright, and insertPlanReview() (PostgresDeploymentRepository.java) is this table's
-- only writer -- a single, conditional INSERT, never an UPDATE or DELETE.

-- deployment_approval_read_ready()'s V021 redefinition is removed, not ported as SQL: V031/V034
-- redeclare it and V034 holds its true final form -- see V034's removal comment.

-- deployment_approval_execution_commit_eligible() and deployment_approval_execution_eligible() are
-- removed here, not ported as SQL: Aurora DSQL rejects CREATE FUNCTION outright. Redeclarations of
-- both hold their true final form elsewhere -- V033 for execution_eligible (ported to
-- PostgresDeploymentRepository.approvalExecutionEligible()), and see V020's removal comment for why
-- execution_commit_eligible's own final form (also V033) needs no Java port at all.
