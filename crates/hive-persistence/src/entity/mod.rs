//! SeaORM entity modules, one per migration-created relation (`GSR-ENTITY-ALL`): every table
//! this schema declares, plus the four read-only views (this repository has four, not the one
//! `audit_event_projection` the design plan originally assumed), plus `organization_read`, a
//! second, narrower module over `organizations` for the Seaography-generated read tier
//! (`GSR-READ-TIER`; SeaORM permits several entity modules over one table). Generated once with
//! `sea-orm-cli generate entity --seaography` against a database `hive migrate` had just
//! migrated (views by hand, since the generator does not discover them), then committed as a
//! starting point: relations are intentionally left empty everywhere except where a specific
//! repository rewrite in a later phase actually needs one, rather than guessing at up to 76
//! tables' worth of relations with no consumer yet to test them against.

pub mod administration_audit_events;
pub mod agent_authoring_audit_events;
pub mod agent_draft_audit_events;
pub mod agent_draft_editor_roles;
pub mod agent_drafts;
pub mod agent_operational_summaries;
pub mod agent_operational_view_projection;
pub mod agent_versions;
pub mod agents;
pub mod audit_event_projection;
pub mod catalog_definitions;
pub mod catalog_environments;
pub mod catalog_projection_heads;
pub mod catalog_releases;
pub mod configuration_audit_events;
pub mod console_role_assignments;
pub mod deployment_approval_decisions;
pub mod deployment_approval_handoff_releases;
pub mod deployment_approval_principal_organization_membership_scopes;
pub mod deployment_approval_principal_organization_scopes;
pub mod deployment_approval_principal_project_scopes;
pub mod deployment_approval_project_archive_events;
pub mod deployment_approval_replay_receipts;
pub mod deployment_approval_requirements;
pub mod deployment_attempts;
pub mod deployment_audit_events;
pub mod deployment_evidence_invalidations;
pub mod deployment_evidence_snapshots;
pub mod deployment_outbox_delivery_audit_repairs;
pub mod deployment_outbox_events;
pub mod deployment_plan_review_facts;
pub mod deployment_plan_versions;
pub mod deployment_policy_snapshots;
pub mod deployment_project_quota_claims;
pub mod deployment_promotion_facts;
pub mod deployment_recovery_action_receipts;
pub mod deployment_runtime_health;
pub mod deployment_stage_events;
pub mod deployment_timeline_counters;
pub mod deployment_worker_heartbeats;
pub mod deployments;
pub mod effective_evaluation_capabilities;
pub mod environment_definition_versions;
pub mod evaluation_artifact_metadata;
pub mod evaluation_audit_events;
pub mod evaluation_case_runs;
pub mod evaluation_command_receipts;
pub mod evaluation_definition_drafts;
pub mod evaluation_definition_versions;
pub mod evaluation_definitions;
pub mod evaluation_metric_results;
pub mod evaluation_outbox_events;
pub mod evaluation_results;
pub mod evaluation_runs;
pub mod evaluation_target_projections;
pub mod evaluation_target_snapshots;
pub mod evaluation_worker_heartbeats;
pub mod frozen_spend_import_batches;
pub mod hive_schema_migration_lock;
pub mod hive_schema_migrations;
pub mod organization_membership_roles;
pub mod organization_memberships;
pub mod organization_read;
pub mod organizations;
pub mod platform_role_assignments;
pub mod principal_display_preferences;
pub mod principals;
pub mod project_approval_policies;
pub mod project_approval_policy_versions;
pub mod project_budget_policies;
pub mod project_budget_policy_versions;
pub mod project_dashboard_metrics;
pub mod project_dashboard_projection;
pub mod project_membership_roles;
pub mod project_memberships;
pub mod project_settings_connections;
pub mod project_tool_connections;
pub mod projects;
pub mod reusable_resource_drafts;
pub mod reusable_resource_versions;
pub mod reusable_resources;

pub mod tenant {
    //! The row-level tenant-scoping predicate a `LifecycleHooksInterface::entity_filter` applies
    //! to a generated read entity (`GSR-TENANT-HOOKS`). Mirrors the `EXISTS` clause
    //! `PgAccessibleOrganizationRepository` already runs by hand
    //! (`organization/accessible_organization.rs`), so a principal sees exactly the organizations
    //! their active membership already grants them elsewhere in the console.

    use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};
    use sea_orm::{ColumnTrait, Condition};
    use uuid::Uuid;

    /// A condition that matches no row. Used by `entity_filter` when no authenticated principal is
    /// present, so the generated read tier never leaks a row rather than failing the schema build.
    pub fn deny_all() -> Condition {
        Condition::all().add(Expr::cust("FALSE"))
    }

    /// `EXISTS (SELECT 1 FROM organization_memberships WHERE organization_id = <organization_id_column>
    /// AND principal_id = <principal> AND started_at <= now() AND ended_at IS NULL)`.
    ///
    /// `organization_id_column.as_column_ref()` (not a bare `Expr::col(organization_id_column)`)
    /// is required: a `ColumnTrait`'s `Iden` impl carries only the column's own name, so an
    /// unqualified reference inside this subquery resolved against `organization_memberships`
    /// itself (which has its own `id` column) rather than the outer row under scrutiny — the first
    /// version of this function compared `organization_memberships.organization_id` to
    /// `organization_memberships.id` on the same membership row, which is never true, so the
    /// `EXISTS` clause was always false and every principal, member or not, got zero
    /// organizations back. An integration test asserting a *known member* gets their organization
    /// back (not just that a stranger gets none) is what caught this; a filter that fails toward
    /// "matches nothing" reads exactly like a correct, strict filter until checked against a case
    /// that should pass.
    pub fn organization_membership_exists<C: ColumnTrait>(
        organization_id_column: C,
        principal: Uuid,
    ) -> Condition {
        let memberships = Alias::new("organization_memberships");
        let subquery = Query::select()
            .expr(Expr::val(1))
            .from(memberships.clone())
            .and_where(
                Expr::col((memberships.clone(), Alias::new("organization_id")))
                    .eq(Expr::col(organization_id_column.as_column_ref())),
            )
            .and_where(Expr::col((memberships.clone(), Alias::new("principal_id"))).eq(principal))
            .and_where(
                Expr::col((memberships.clone(), Alias::new("started_at")))
                    .lte(Expr::current_timestamp()),
            )
            .and_where(Expr::col((memberships, Alias::new("ended_at"))).is_null())
            .to_owned();
        Condition::all().add(Expr::exists(subquery))
    }
}
