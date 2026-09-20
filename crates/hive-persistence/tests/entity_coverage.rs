//! `GSR-ENTITY-ALL`'s smoke test: every full-table entity module's declared columns match
//! `information_schema.columns` in both directions, and `Entity::find().limit(1)` decodes every
//! column without error — the same guarantee Hibernate's `validate` schema strategy gave the Java
//! tree, now checked once per module rather than assumed. `organization_read` is deliberately
//! excluded: it is a narrower read-tier projection over the `organizations` table
//! (`GSR-READ-TIER`), not a 1:1 mapping, so a bidirectional column match is the wrong check for it.
//!
//! Requires a live, empty-or-migrated PostgreSQL database named by `HIVE_TEST_DATABASE_URL`
//! (falls back to `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default.
//!   cargo test -p hive-persistence --test entity_coverage -- --ignored

use sea_orm::sea_query::TableRef;
use sea_orm::{
    Database, DatabaseConnection, EntityTrait, FromQueryResult, Identity, Iterable, QuerySelect,
    RelationTrait, Statement,
};
use std::collections::BTreeSet;

fn test_database_url() -> String {
    std::env::var("HIVE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hive:hive@127.0.0.1:15432/hive".to_string())
}

#[derive(FromQueryResult)]
struct ColumnNameRow {
    column_name: String,
}

async fn real_columns_of(db: &DatabaseConnection, table: &str) -> BTreeSet<String> {
    ColumnNameRow::find_by_statement(Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT column_name FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1",
        [table.into()],
    ))
    .all(db)
    .await
    .unwrap_or_else(|error| panic!("reading information_schema.columns for {table}: {error}"))
    .into_iter()
    .map(|row| row.column_name)
    .collect()
}

/// The plain SQL table name a `RelationDef.from_tbl`/`to_tbl` names, for the ordinary
/// `belongs_to`/`has_many` case (`TableRef::Table`) every relation in this codebase would use; the
/// other variants (a subquery, a values list, a function call) never appear as a relation's table.
fn table_ref_name(table_ref: &TableRef) -> String {
    match table_ref {
        TableRef::Table(name, _alias) => name.1.to_string(),
        other => panic!(
            "relation names a non-table TableRef, unexpected for an entity relation: {other:?}"
        ),
    }
}

/// An `Identity` (SeaORM's column-or-tuple-of-columns type for a relation's `from_col`/`to_col`)
/// rendered as plain column-name strings.
fn identity_columns(identity: &Identity) -> Vec<String> {
    match identity {
        Identity::Unary(a) => vec![a.to_string()],
        Identity::Binary(a, b) => vec![a.to_string(), b.to_string()],
        Identity::Ternary(a, b, c) => vec![a.to_string(), b.to_string(), c.to_string()],
        Identity::Many(columns) => columns.iter().map(ToString::to_string).collect(),
    }
}

/// Both directions: every declared `Column` exists as a real column, and every real column has a
/// declared `Column` — either mismatch means the entity module and the migrations have drifted.
/// `Entity::find().limit(1)` additionally decodes every row-in-scope column, catching a type
/// mismatch (e.g. an `Option<T>` on a `NOT NULL` column, or the reverse) a name-only comparison
/// would miss.
///
/// Also checks `check:schema:entity-relations`'s guarantee in the same pass: every declared
/// `Relation`'s `from_tbl`/`to_tbl` name a table this database actually has, and `from_col`/
/// `to_col` name real columns on it. Every entity currently declares zero relations
/// (`GSR-ENTITY-ALL`'s scoping decision: annotate a relation only once a repository rewrite in a
/// later phase actually needs it, not speculatively for all 76 tables at once), so this loop body
/// has no runtime coverage yet — it exists so the first relation phase 2 or later adds is checked
/// against the live schema from the moment it exists, not assumed correct because it compiles
/// (`Column::X` existing as a Rust identifier proves nothing about whether the migration still
/// names that column, unlike this check).
macro_rules! check_entity_coverage {
    ($db:expr, $module:ident, $table:literal) => {{
        use hive_persistence::entity::$module::{Column, Entity, Relation};
        let declared: BTreeSet<String> = Column::iter()
            .map(|column| <Column as sea_orm::Iden>::to_string(&column))
            .collect();
        let real = real_columns_of($db, $table).await;
        let missing_from_entity: Vec<_> = real.difference(&declared).cloned().collect();
        let missing_from_schema: Vec<_> = declared.difference(&real).cloned().collect();
        assert!(
            missing_from_entity.is_empty() && missing_from_schema.is_empty(),
            "{}: entity module and information_schema.columns disagree — columns only in the schema: {missing_from_entity:?}; columns only in the entity: {missing_from_schema:?}",
            $table,
        );
        Entity::find()
            .limit(1)
            .all($db)
            .await
            .unwrap_or_else(|error| panic!("Entity::find().limit(1) for {}: {error}", $table));

        for relation in Relation::iter() {
            let definition = relation.def();
            let from_table = table_ref_name(&definition.from_tbl);
            let to_table = table_ref_name(&definition.to_tbl);
            for column in identity_columns(&definition.from_col) {
                assert!(
                    real_columns_of($db, &from_table).await.contains(&column),
                    "{}: relation names from_col {column:?} on {from_table}, which has no such column",
                    $table,
                );
            }
            for column in identity_columns(&definition.to_col) {
                assert!(
                    real_columns_of($db, &to_table).await.contains(&column),
                    "{}: relation names to_col {column:?} on {to_table}, which has no such column",
                    $table,
                );
            }
        }
    }};
}

#[tokio::test]
#[ignore]
async fn every_entity_module_matches_its_migrated_table() {
    let mut options = sea_orm::ConnectOptions::new(test_database_url());
    options.max_connections(1);
    let db = Database::connect(options)
        .await
        .expect("connect to the test database");
    hive_persistence::migrate_and_seed(&db)
        .await
        .expect("migrate the test database");
    check_entity_coverage!(
        &db,
        administration_audit_events,
        "administration_audit_events"
    );
    check_entity_coverage!(
        &db,
        agent_authoring_audit_events,
        "agent_authoring_audit_events"
    );
    check_entity_coverage!(&db, agent_draft_audit_events, "agent_draft_audit_events");
    check_entity_coverage!(&db, agent_draft_editor_roles, "agent_draft_editor_roles");
    check_entity_coverage!(&db, agent_drafts, "agent_drafts");
    check_entity_coverage!(
        &db,
        agent_operational_summaries,
        "agent_operational_summaries"
    );
    check_entity_coverage!(
        &db,
        agent_operational_view_projection,
        "agent_operational_view_projection"
    );
    check_entity_coverage!(&db, agent_versions, "agent_versions");
    check_entity_coverage!(&db, agents, "agents");
    check_entity_coverage!(&db, audit_event_projection, "audit_event_projection");
    check_entity_coverage!(&db, catalog_definitions, "catalog_definitions");
    check_entity_coverage!(&db, catalog_environments, "catalog_environments");
    check_entity_coverage!(&db, catalog_projection_heads, "catalog_projection_heads");
    check_entity_coverage!(&db, catalog_releases, "catalog_releases");
    check_entity_coverage!(
        &db,
        configuration_audit_events,
        "configuration_audit_events"
    );
    check_entity_coverage!(&db, console_role_assignments, "console_role_assignments");
    check_entity_coverage!(
        &db,
        deployment_approval_decisions,
        "deployment_approval_decisions"
    );
    check_entity_coverage!(
        &db,
        deployment_approval_handoff_releases,
        "deployment_approval_handoff_releases"
    );
    check_entity_coverage!(
        &db,
        deployment_approval_principal_organization_membership_scopes,
        "deployment_approval_principal_organization_membership_scopes"
    );
    check_entity_coverage!(
        &db,
        deployment_approval_principal_organization_scopes,
        "deployment_approval_principal_organization_scopes"
    );
    check_entity_coverage!(
        &db,
        deployment_approval_principal_project_scopes,
        "deployment_approval_principal_project_scopes"
    );
    check_entity_coverage!(
        &db,
        deployment_approval_project_archive_events,
        "deployment_approval_project_archive_events"
    );
    check_entity_coverage!(
        &db,
        deployment_approval_replay_receipts,
        "deployment_approval_replay_receipts"
    );
    check_entity_coverage!(
        &db,
        deployment_approval_requirements,
        "deployment_approval_requirements"
    );
    check_entity_coverage!(&db, deployment_attempts, "deployment_attempts");
    check_entity_coverage!(&db, deployment_audit_events, "deployment_audit_events");
    check_entity_coverage!(
        &db,
        deployment_evidence_invalidations,
        "deployment_evidence_invalidations"
    );
    check_entity_coverage!(
        &db,
        deployment_evidence_snapshots,
        "deployment_evidence_snapshots"
    );
    check_entity_coverage!(
        &db,
        deployment_outbox_delivery_audit_repairs,
        "deployment_outbox_delivery_audit_repairs"
    );
    check_entity_coverage!(&db, deployment_outbox_events, "deployment_outbox_events");
    check_entity_coverage!(
        &db,
        deployment_plan_review_facts,
        "deployment_plan_review_facts"
    );
    check_entity_coverage!(&db, deployment_plan_versions, "deployment_plan_versions");
    check_entity_coverage!(
        &db,
        deployment_policy_snapshots,
        "deployment_policy_snapshots"
    );
    check_entity_coverage!(
        &db,
        deployment_project_quota_claims,
        "deployment_project_quota_claims"
    );
    check_entity_coverage!(
        &db,
        deployment_promotion_facts,
        "deployment_promotion_facts"
    );
    check_entity_coverage!(
        &db,
        deployment_recovery_action_receipts,
        "deployment_recovery_action_receipts"
    );
    check_entity_coverage!(&db, deployment_runtime_health, "deployment_runtime_health");
    check_entity_coverage!(&db, deployment_stage_events, "deployment_stage_events");
    check_entity_coverage!(
        &db,
        deployment_timeline_counters,
        "deployment_timeline_counters"
    );
    check_entity_coverage!(
        &db,
        deployment_worker_heartbeats,
        "deployment_worker_heartbeats"
    );
    check_entity_coverage!(&db, deployments, "deployments");
    check_entity_coverage!(
        &db,
        effective_evaluation_capabilities,
        "effective_evaluation_capabilities"
    );
    check_entity_coverage!(
        &db,
        environment_definition_versions,
        "environment_definition_versions"
    );
    check_entity_coverage!(
        &db,
        evaluation_artifact_metadata,
        "evaluation_artifact_metadata"
    );
    check_entity_coverage!(&db, evaluation_audit_events, "evaluation_audit_events");
    check_entity_coverage!(&db, evaluation_case_runs, "evaluation_case_runs");
    check_entity_coverage!(
        &db,
        evaluation_command_receipts,
        "evaluation_command_receipts"
    );
    check_entity_coverage!(
        &db,
        evaluation_definition_drafts,
        "evaluation_definition_drafts"
    );
    check_entity_coverage!(
        &db,
        evaluation_definition_versions,
        "evaluation_definition_versions"
    );
    check_entity_coverage!(&db, evaluation_definitions, "evaluation_definitions");
    check_entity_coverage!(&db, evaluation_metric_results, "evaluation_metric_results");
    check_entity_coverage!(&db, evaluation_outbox_events, "evaluation_outbox_events");
    check_entity_coverage!(&db, evaluation_results, "evaluation_results");
    check_entity_coverage!(&db, evaluation_runs, "evaluation_runs");
    check_entity_coverage!(
        &db,
        evaluation_target_projections,
        "evaluation_target_projections"
    );
    check_entity_coverage!(
        &db,
        evaluation_target_snapshots,
        "evaluation_target_snapshots"
    );
    check_entity_coverage!(
        &db,
        evaluation_worker_heartbeats,
        "evaluation_worker_heartbeats"
    );
    check_entity_coverage!(
        &db,
        frozen_spend_import_batches,
        "frozen_spend_import_batches"
    );
    check_entity_coverage!(
        &db,
        hive_schema_migration_lock,
        "hive_schema_migration_lock"
    );
    check_entity_coverage!(&db, hive_schema_migrations, "hive_schema_migrations");
    check_entity_coverage!(
        &db,
        organization_membership_roles,
        "organization_membership_roles"
    );
    check_entity_coverage!(&db, organization_memberships, "organization_memberships");
    check_entity_coverage!(&db, organizations, "organizations");
    check_entity_coverage!(&db, platform_role_assignments, "platform_role_assignments");
    check_entity_coverage!(
        &db,
        principal_display_preferences,
        "principal_display_preferences"
    );
    check_entity_coverage!(&db, principals, "principals");
    check_entity_coverage!(&db, project_approval_policies, "project_approval_policies");
    check_entity_coverage!(
        &db,
        project_approval_policy_versions,
        "project_approval_policy_versions"
    );
    check_entity_coverage!(&db, project_budget_policies, "project_budget_policies");
    check_entity_coverage!(
        &db,
        project_budget_policy_versions,
        "project_budget_policy_versions"
    );
    check_entity_coverage!(&db, project_dashboard_metrics, "project_dashboard_metrics");
    check_entity_coverage!(
        &db,
        project_dashboard_projection,
        "project_dashboard_projection"
    );
    check_entity_coverage!(&db, project_membership_roles, "project_membership_roles");
    check_entity_coverage!(&db, project_memberships, "project_memberships");
    check_entity_coverage!(
        &db,
        project_settings_connections,
        "project_settings_connections"
    );
    check_entity_coverage!(&db, project_tool_connections, "project_tool_connections");
    check_entity_coverage!(&db, projects, "projects");
    check_entity_coverage!(&db, reusable_resource_drafts, "reusable_resource_drafts");
    check_entity_coverage!(
        &db,
        reusable_resource_versions,
        "reusable_resource_versions"
    );
    check_entity_coverage!(&db, reusable_resources, "reusable_resources");
}
