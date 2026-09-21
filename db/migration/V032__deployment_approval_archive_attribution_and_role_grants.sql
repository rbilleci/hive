-- This migration declares no schema objects. Archive attribution needs no fallback relation:
-- `record_project_archive_event` is the only writer of
-- deployment_approval_project_archive_events and always supplies its caller's required, non-null
-- actor, so actor_principal_id is never NULL on a row this system creates and readers take the
-- attribution straight from the event.

-- Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, so no database guard restricts who
-- may grant DEPLOYMENT_APPROVER. `approval_role_transition_allowed` enforces that only a platform
-- administrator may add, remove, or move that role, and it runs before every write that could reach
-- the restriction: `add_membership`/`replace_membership` ahead of their `replace_roles` call, and
-- `end_membership` ahead of its ended_at UPDATE. project_memberships.project_id and .principal_id are
-- never updated by anything, so no write can move a grant between scopes.

-- Approval capabilities, approval-inbox visibility, the approval requirement predicate, and archive
-- reconciliation are all application code for the same reason: `deployment_approval_capabilities`
-- (computed from `authority_assignments` plus `is_platform_administrator`), `ensure_requirement`,
-- `invalidate_archived_approval_requirement`, and `reconcile_project_archives`.
