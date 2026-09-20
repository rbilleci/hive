-- V018 upgrades databases that recorded V017 before compact scopes, immutable-review screening,
-- and projection-watermark maintenance were finalized. It is additive and preserves every
-- immutable requirement, decision, evidence, and audit fact.
-- This CREATE TABLE IF NOT EXISTS is an unconditional no-op in this greenfield rewrite (V017's own
-- CREATE TABLE already declares this table), so its REFERENCES clauses never execute, but the clauses
-- are still removed here for the same reason the ADD COLUMN statements below remove theirs: Aurora
-- DSQL does not support them, and leaving dead REFERENCES text in a migration is misleading regardless
-- of whether this exact statement is reachable. See V017's own removal comment for the live FK's
-- reasoning.
CREATE TABLE IF NOT EXISTS deployment_approval_principal_organization_scopes
(
    principal_id UUID NOT NULL,
    organization_id UUID NOT NULL,
    valid_after TIMESTAMPTZ NOT NULL,
    PRIMARY KEY
(
    principal_id,
    organization_id
)
    );
CREATE INDEX IF NOT EXISTS deployment_approval_principal_organization_scopes_organization
    ON deployment_approval_principal_organization_scopes (organization_id, principal_id);
-- Historical V017 revisions recorded the migration marker before these discovery fields existed.
-- Add them without a table-wide rewrite; the bounded upgrade worker populates each retained row
-- before approval traffic becomes ready (the transition-trigger reference this comment used to make
-- no longer applies -- see V017's removal comment). These ADD COLUMN IF NOT EXISTS statements are
-- unconditional no-ops in this greenfield rewrite (V017's own CREATE TABLE already declares both
-- columns), so their REFERENCES clauses never execute either, but the clauses are still removed here
-- for the same reason every other domain step in this rewrite removes a never-taken one: Aurora DSQL
-- does not support them, and leaving dead REFERENCES text in a migration is misleading regardless of
-- whether this exact statement is reachable.
ALTER TABLE deployment_approval_requirements
    ADD COLUMN IF NOT EXISTS organization_id UUID;
ALTER TABLE deployment_approval_requirements
    ADD COLUMN IF NOT EXISTS project_id UUID;
ALTER TABLE deployment_approval_requirements
    ADD COLUMN IF NOT EXISTS requested_at TIMESTAMPTZ;
ALTER TABLE deployment_approval_requirements
    ADD COLUMN IF NOT EXISTS required_approvers INTEGER;
ALTER TABLE deployment_approval_requirements DROP CONSTRAINT IF EXISTS deployment_approval_requirements_required_approvers_check;
ALTER TABLE deployment_approval_requirements
    ADD CONSTRAINT deployment_approval_requirements_required_approvers_check
        CHECK (required_approvers IS NULL OR required_approvers BETWEEN 0 AND 2);
-- Aurora DSQL rejects partial indexes outright (0A000 WHERE not supported for CREATE INDEX); widened
-- to full indexes, same as V017's identical redeclarations of some of these names.
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_global_inbox
    ON deployment_approval_requirements (requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_organization_inbox
    ON deployment_approval_requirements (organization_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_project_inbox
    ON deployment_approval_requirements (project_id, requested_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_approval_requirements_zero_handoff
    ON deployment_approval_requirements (requested_at ASC, id ASC);
-- Also an unconditional no-op (V017's own CREATE TABLE already declares this table); its REFERENCES
-- clauses are removed for the same reason as the sibling table above.
CREATE TABLE IF NOT EXISTS deployment_approval_principal_project_scopes
(
    principal_id UUID NOT NULL,
    project_id UUID NOT NULL,
    valid_after TIMESTAMPTZ NOT NULL,
    PRIMARY KEY
(
    principal_id,
    project_id
)
    );
CREATE INDEX IF NOT EXISTS deployment_approval_principal_project_scopes_project
    ON deployment_approval_principal_project_scopes (project_id, principal_id);

-- deployment_approval_upgrade_progress and its two "_page" backfill functions below are removed
-- outright, not ported: see PostgresDeploymentRepository.reconcileApprovalUpgrade()'s comment for the
-- shared reasoning every "_progress"/"_backfill_page"/"_upgrade_*" object this domain step removes has
-- in common -- each existed to catch up rows a live database could have accumulated before its own
-- migration ran, which cannot happen in this greenfield rewrite.

-- deployment_approval_review_text_safe() is removed here, not ported as SQL: Aurora DSQL rejects
-- CREATE FUNCTION outright; see this file's own m14_comment_check/m14_rejection_reason_check below
-- (and V017's CHECK constraints, its original declaration site) for the same logic inlined directly.
-- This V018 redeclaration of deployment_approval_is_platform_administrator() (identical to V017's
-- original) is removed, not ported as SQL: see V017's removal comment for the Java port
-- (PostgresEffectiveCapabilityEvaluator.isPlatformAdministrator()).
-- Historical decision rows stay immutable. These NOT VALID constraints preserve those facts while
-- applying the final M14 code-only text policy to every post-upgrade append. The original DO block's
-- "IF NOT EXISTS" guards protected an idempotent replay against an already-upgraded live database;
-- Aurora DSQL rejects anonymous plpgsql blocks outright, and this file runs through
-- executeVersionedFile() (DatabaseMigrator.java), which the schema-migration ledger already runs at
-- most once per database ever, so a fresh database can never see these constraints pre-exist -- the
-- guards have nothing left to protect against and are dropped, not ported.
ALTER TABLE deployment_approval_decisions
    ADD CONSTRAINT deployment_approval_decisions_m14_comment_check CHECK (
        comment IS NULL OR (length(btrim(comment)) BETWEEN 1 AND 2000 AND btrim(comment) IN (
            'REVIEWED_CHANGE_SCOPE', 'AUTHORIZATION_GRANTED', 'UNACCEPTABLE_CHANGE_SCOPE', 'CHANGE_SCOPE_NOT_APPROVED'))
        ) NOT VALID;
ALTER TABLE deployment_approval_decisions
    ADD CONSTRAINT deployment_approval_decisions_m14_rejection_reason_check CHECK (
        rejection_reason IS NULL OR
        (length(btrim(rejection_reason)) BETWEEN 1 AND 2000 AND btrim(rejection_reason) IN (
            'REVIEWED_CHANGE_SCOPE', 'AUTHORIZATION_GRANTED', 'UNACCEPTABLE_CHANGE_SCOPE', 'CHANGE_SCOPE_NOT_APPROVED'))
        ) NOT VALID;
ALTER TABLE deployment_approval_decisions
    ADD CONSTRAINT deployment_approval_decisions_m14_approval_reason_check CHECK (
        decision <> 'APPROVE' OR rejection_reason IS NULL
        ) NOT VALID;

-- This V018 redeclaration of the scope-cache anchor/refresh functions and their four trigger
-- bindings (organization_memberships/organization_membership_roles/project_memberships/
-- project_membership_roles), plus V018's own deployment_approval_project_role_scope_insert_batch()
-- statement-level batch variant, are removed here too -- see V017's removal comment for the Java
-- port (PostgresAdministrationRepository.java's refreshMembershipScope()/refreshRoleScope()). The
-- batch variant needed no separate port: refreshRoleScope() is idempotent and already covers the
-- same (principal, project) keys a batch role INSERT touches, one call per replaceRoles() rather
-- than a deduplicated statement-level pass over the whole batch -- replaceRoles() is only ever
-- called for one membership's roles at a time, so there is only ever one key to refresh.

-- The API maintenance pass fills retained authority scopes in capped pages. A readiness gate keeps
-- the inbox closed until this projection and the retained requirement facts are complete.

-- V018 redeclares deployment_approval_visible_requirements() (retained-authority-cache-aware,
-- adding recheck branches) -- removed here too, not ported as SQL: V034 holds the true final
-- redefinition (confirmed by grepping every migration file for the last declaration), and the
-- Java port lives at PostgresDeploymentRepository.approvalInboxRequirementIds() -- see that
-- method's comment and V034's own removal comment for the full reasoning.

-- deployment_approval_audit_projection_watermark_trigger and its function are removed, not ported:
-- Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright, and
-- PostgresDeploymentRepository.audit() -- the shared helper behind every INSERT INTO
-- deployment_audit_events this domain step's Java owns -- already calls touchProjection()
-- unconditionally for every action, not only APPROVAL_-prefixed ones, so this trigger's own bump is
-- already covered for every current and ported audit path. The three hand-rolled, hardcoded-attempt
-- audit inserts that bypass audit() (blockApprovalExecution(), reconcilePending(),
-- automaticApprovalHandoff()'s SATISFIED branch) each already call touchProjection() explicitly,
-- literally porting the SQL functions' own PERFORM calls.

-- This V018 redefinition of deployment_approval_requirement_transition() -- itself superseded by
-- V033's later redefinition, see V017's removal comment for the actual port -- is removed here too:
-- Aurora DSQL rejects CREATE FUNCTION regardless of which migration declares it, and leaving an
-- intermediate redefinition in place would still break this file on its own.

-- deployment_approval_touch_projection()'s final redefinition is removed here, not ported as SQL: see
-- V017's removal comment, its original declaration site.

-- deployment_approval_upgrade_requirement_facts_page(), deployment_approval_upgrade_scopes_page(),
-- this file's own redeclaration of deployment_approval_compatibility_progress, and this file's
-- redeclaration of deployment_approval_read_ready() are all removed, not ported: see
-- PostgresDeploymentRepository.reconcileApprovalUpgrade()'s comment for the shared reasoning --
-- each existed to catch up rows a live database could have accumulated before its own migration ran,
-- which cannot happen in this greenfield rewrite. deployment_approval_upgrade_scopes_page() specifically
-- upgraded retained role-scope rows into deployment_approval_principal_organization_scopes/
-- _project_scopes; the live triggers that now maintain those tables going forward
-- (PostgresAdministrationRepository.refreshMembershipScope()/refreshRoleScope(), ported in this domain
-- step's sub-step (b)) already cover every row this rewrite's write path ever creates.

-- deployment_approval_outbox_claim_gate() is removed here (a redeclaration; V019 holds its trigger
-- binding), not ported as SQL: see V017's removal comment.

-- deployment_approval_record_terminal_invalidation()'s final redefinition is removed here, not ported
-- as SQL: Aurora DSQL rejects CREATE FUNCTION outright. Ported to
-- PostgresDeploymentRepository.recordTerminalInvalidation() -- see V017's removal comment, its original
-- declaration site, for the shared reasoning.

-- deployment_approval_ensure_requirement()'s V018 redefinition is removed, not ported as SQL: V024/
-- V027/V032 redeclare it and V032 holds its true final form -- see V017's removal comment.

-- deployment_approval_compatibility_backfill_page()'s only declaration (never redeclared elsewhere) is
-- removed outright, not ported: its own loop condition (a deployment with no
-- deployment_approval_requirements row) can never match in this greenfield rewrite, where
-- insertApprovalRequirement() (PostgresDeploymentRepository.java) creates that row synchronously for
-- every deployment from the start -- see the removal comment on deployment_approval_compatibility_progress
-- (V017) for the shared reasoning.

-- deployment_approval_requirement_compatibility_trigger (this file's own trigger-binding site,
-- superseding V017's) and its function are removed, not ported: see V017's removal comment.

-- deployment_approval_evaluation_handoff_trigger's final redefinition (this file's own trigger-
-- binding site, superseding V017's, and the form PostgresEvaluationRepository's Java port matches --
-- it adds an unconditional deployment_approval_touch_projection() call V017's simpler form did not
-- have) and its function are removed, not ported: see V017's removal comment.
