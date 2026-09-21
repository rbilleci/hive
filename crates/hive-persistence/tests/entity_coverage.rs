//! The entity modules checked against the live, migrated schema. The database has no foreign keys
//! (Aurora DSQL has none) and no enum types, so nothing but this test ties the hand-declared
//! relations and active enums in `hive_persistence::entity` to what the migrations really
//! create. For every entity module it proves:
//!
//! 1. the declared columns equal the table's (or view's) real columns, in both directions, and
//!    `Entity::find().limit(1)` decodes;
//! 2. every `Relation` names tables and columns that exist, and each `from`/`to` column pair has
//!    the same database type;
//! 3. every active-enum column has exactly the value set of that column's `CHECK` constraint, and
//!    every column whose `CHECK` is a plain value list is an active enum.
//!
//! The catalog is read the same way as everything else: through SeaORM entities, here declared
//! over `information_schema`.
//!
//! Requires a live PostgreSQL database named by `HIVE_TEST_DATABASE_URL` (falls back to
//! `postgres://hive:hive@127.0.0.1:15432/hive`); the test migrates it. Not run by default.
//!   cargo test -p hive-persistence --test entity_coverage -- --ignored

use sea_orm::sea_query::TableRef;
use sea_orm::{
    ActiveEnum, ColumnTrait, Database, DatabaseConnection, EntityName, EntityTrait, Identity,
    Iterable, QueryFilter, QuerySelect, RelationTrait,
};
use std::collections::{BTreeMap, BTreeSet};

/// `information_schema.columns`: one row per column of every table and view.
mod catalog_columns {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(schema_name = "information_schema", table_name = "columns")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub table_schema: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub table_name: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub column_name: String,
        /// The underlying type name (`uuid`, `text`, `bpchar`, ...).
        pub udt_name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// `information_schema.check_constraints`: the expression of every `CHECK`.
mod catalog_check_constraints {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(schema_name = "information_schema", table_name = "check_constraints")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub constraint_schema: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub constraint_name: String,
        pub check_clause: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// `information_schema.constraint_column_usage`: which columns a constraint's expression reads.
mod catalog_constraint_columns {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(
        schema_name = "information_schema",
        table_name = "constraint_column_usage"
    )]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub constraint_schema: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub constraint_name: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub table_name: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub column_name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

const SCHEMA: &str = "public";

/// Columns whose `CHECK` pins a value set but which are deliberately not active enums.
const CLOSED_SETS_THAT_ARE_NOT_ENUMS: [(&str, &str, &str); 1] = [(
    "catalog_projection_heads",
    "id",
    "`id = 'local'` is a single-row guard on the primary key, not a status or kind",
)];

type ColumnKey = (String, String);

/// What the migrated database really contains, read once.
struct LiveSchema {
    db: DatabaseConnection,
    /// table or view -> column -> underlying type name
    columns: BTreeMap<String, BTreeMap<String, String>>,
    /// (table, column) -> the values its `CHECK` allows, for every `CHECK` that is a plain value list
    closed_sets: BTreeMap<ColumnKey, BTreeSet<String>>,
}

impl LiveSchema {
    async fn load(db: DatabaseConnection) -> Self {
        let mut columns: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for column in catalog_columns::Entity::find()
            .filter(catalog_columns::Column::TableSchema.eq(SCHEMA))
            .all(&db)
            .await
            .expect("read information_schema.columns")
        {
            columns
                .entry(column.table_name)
                .or_default()
                .insert(column.column_name, column.udt_name);
        }

        let mut clauses: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for constraint in catalog_check_constraints::Entity::find()
            .filter(catalog_check_constraints::Column::ConstraintSchema.eq(SCHEMA))
            .all(&db)
            .await
            .expect("read information_schema.check_constraints")
        {
            clauses
                .entry(constraint.constraint_name)
                .or_default()
                .push(constraint.check_clause);
        }
        let mut constrained: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
        for usage in catalog_constraint_columns::Entity::find()
            .filter(catalog_constraint_columns::Column::ConstraintSchema.eq(SCHEMA))
            .all(&db)
            .await
            .expect("read information_schema.constraint_column_usage")
        {
            constrained
                .entry((usage.table_name, usage.constraint_name))
                .or_default()
                .insert(usage.column_name);
        }
        let mut closed_sets = BTreeMap::new();
        for ((table, constraint), constraint_columns) in constrained {
            // A rule over several columns is not a value list.
            let mut constraint_columns = constraint_columns.into_iter();
            let (Some(column), None) = (constraint_columns.next(), constraint_columns.next())
            else {
                continue;
            };
            for clause in clauses.get(&constraint).into_iter().flatten() {
                if let Some(values) = closed_value_set(clause, &column) {
                    let previous = closed_sets.insert((table.clone(), column.clone()), values);
                    assert!(
                        previous.is_none(),
                        "{table}.{column} has two value-list CHECK constraints"
                    );
                }
            }
        }
        Self {
            db,
            columns,
            closed_sets,
        }
    }

    fn column_type(&self, table: &str, column: &str) -> Option<&str> {
        self.columns.get(table)?.get(column).map(String::as_str)
    }
}

/// Removes parentheses that wrap the whole expression, as PostgreSQL prints them.
fn without_outer_parentheses(mut text: &str) -> &str {
    loop {
        text = text.trim();
        let Some(inner) = text
            .strip_prefix('(')
            .and_then(|rest| rest.strip_suffix(')'))
        else {
            return text;
        };
        let mut depth = 0_i32;
        for character in inner.chars() {
            match character {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth < 0 {
                // The leading parenthesis closes before the end: `(a) OR (b)`.
                return text;
            }
        }
        text = inner;
    }
}

/// The value set of a `CHECK` that only lists allowed values for `column`, as PostgreSQL prints
/// one: `col = ANY (ARRAY['A'::text, 'B'::text])`, `col = 'A'::text`, either optionally preceded by
/// `(col IS NULL) OR`. Anything else (ranges, patterns, lengths, rules over trimmed text) is `None`.
fn closed_value_set(clause: &str, column: &str) -> Option<BTreeSet<String>> {
    let mut text = without_outer_parentheses(clause);
    if let Some(rest) = text.strip_prefix(&format!("({column} IS NULL) OR ")) {
        text = without_outer_parentheses(rest);
    }
    let allowed = text.strip_prefix(&format!("{column} = "))?;
    let literals: Vec<&str> = match allowed
        .strip_prefix("ANY (ARRAY[")
        .and_then(|rest| rest.strip_suffix("])"))
    {
        Some(list) => list.split(", ").collect(),
        None => vec![allowed],
    };
    literals
        .into_iter()
        .map(|literal| {
            let value = literal.strip_prefix('\'')?.strip_suffix("'::text")?;
            (!value.contains('\'')).then(|| value.to_string())
        })
        .collect()
}

fn test_database_url() -> String {
    std::env::var("HIVE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hive:hive@127.0.0.1:15432/hive".to_string())
}

/// The plain table name a `RelationDef.from_tbl`/`to_tbl` names. Every relation in this codebase
/// is a `belongs_to`/`has_one`/`has_many` between tables, so the other `TableRef` variants (a
/// subquery, a values list, a function call) never appear.
fn table_ref_name(table_ref: &TableRef) -> String {
    match table_ref {
        TableRef::Table(name, _alias) => name.1.to_string(),
        other => panic!(
            "relation names a non-table TableRef, unexpected for an entity relation: {other:?}"
        ),
    }
}

/// An `Identity` (SeaORM's column-or-tuple-of-columns type for a relation's `from_col`/`to_col`)
/// as plain column names.
fn identity_columns(identity: &Identity) -> Vec<String> {
    match identity {
        Identity::Unary(a) => vec![a.to_string()],
        Identity::Binary(a, b) => vec![a.to_string(), b.to_string()],
        Identity::Ternary(a, b, c) => vec![a.to_string(), b.to_string(), c.to_string()],
        Identity::Many(columns) => columns.iter().map(ToString::to_string).collect(),
    }
}

/// The values of the active enum a `Model` field has. The field accessor is what proves, at
/// compile time, that the column is typed by an active enum and not a `String`.
fn enum_values<M, E>(_field: fn(&M) -> E) -> BTreeSet<String>
where
    E: ActiveEnum<Value = String> + Iterable,
{
    E::iter().map(|variant| variant.to_value()).collect()
}

fn optional_enum_values<M, E>(_field: fn(&M) -> Option<E>) -> BTreeSet<String>
where
    E: ActiveEnum<Value = String> + Iterable,
{
    E::iter().map(|variant| variant.to_value()).collect()
}

macro_rules! enum_column {
    ($declared:ident, $module:ident, $field:ident) => {
        $declared.insert(
            (
                stringify!($module).to_string(),
                stringify!($field).to_string(),
            ),
            enum_values(|model: &hive_persistence::entity::$module::Model| model.$field),
        );
    };
}

macro_rules! optional_enum_column {
    ($declared:ident, $module:ident, $field:ident) => {
        $declared.insert(
            (
                stringify!($module).to_string(),
                stringify!($field).to_string(),
            ),
            optional_enum_values(|model: &hive_persistence::entity::$module::Model| model.$field),
        );
    };
}

/// Every active-enum column of every entity, with the enum's values.
fn declared_enum_columns() -> BTreeMap<ColumnKey, BTreeSet<String>> {
    let mut declared = BTreeMap::new();
    enum_column!(declared, agent_draft_audit_events, action);
    enum_column!(declared, administration_audit_events, scope_type);
    enum_column!(declared, agent_drafts, validation_status);
    enum_column!(declared, deployment_stage_events, stage);
    enum_column!(declared, deployment_stage_events, status);
    enum_column!(
        declared,
        agent_operational_summaries,
        draft_validation_status
    );
    enum_column!(declared, agent_operational_summaries, deployment_status);
    enum_column!(declared, agent_operational_summaries, evaluation_outcome);
    enum_column!(declared, agent_operational_summaries, runtime_health);
    enum_column!(declared, deployment_attempts, status);
    enum_column!(declared, deployment_approval_decisions, decision);
    enum_column!(declared, deployment_runtime_health, status);
    enum_column!(declared, organization_membership_roles, role_code);
    enum_column!(declared, agents, lifecycle_status);
    enum_column!(declared, agent_authoring_audit_events, action);
    enum_column!(declared, evaluation_definition_drafts, validation_status);
    enum_column!(declared, deployment_outbox_delivery_audit_repairs, action);
    enum_column!(declared, deployment_outbox_events, event_type);
    enum_column!(declared, deployment_outbox_events, status);
    enum_column!(declared, project_membership_roles, role_code);
    enum_column!(declared, deployment_worker_heartbeats, state);
    enum_column!(declared, console_role_assignments, role_code);
    enum_column!(declared, evaluation_results, outcome_category);
    enum_column!(declared, project_dashboard_metrics, cost_availability);
    enum_column!(declared, deployment_plan_versions, environment);
    enum_column!(declared, deployment_audit_events, action);
    enum_column!(declared, evaluation_outbox_events, event_type);
    enum_column!(declared, evaluation_outbox_events, status);
    enum_column!(declared, platform_role_assignments, role_code);
    enum_column!(declared, catalog_environments, environment);
    enum_column!(
        declared,
        deployment_policy_snapshots,
        logical_environment_class
    );
    enum_column!(declared, deployment_policy_snapshots, risk);
    enum_column!(declared, deployment_evidence_invalidations, kind);
    enum_column!(declared, deployment_evidence_snapshots, evidence_kind);
    enum_column!(declared, catalog_definitions, definition_kind);
    enum_column!(declared, deployments, environment);
    enum_column!(declared, deployments, strategy);
    enum_column!(declared, deployments, lifecycle_status);
    enum_column!(declared, evaluation_runs, target_kind);
    enum_column!(declared, evaluation_runs, lifecycle_status);
    optional_enum_column!(declared, evaluation_runs, outcome_category);
    enum_column!(declared, organizations, lifecycle_status);
    enum_column!(
        declared,
        environment_definition_versions,
        logical_environment_class
    );
    enum_column!(declared, deployment_approval_requirements, status);
    optional_enum_column!(
        declared,
        deployment_approval_requirements,
        invalidation_code
    );
    enum_column!(declared, deployment_recovery_action_receipts, action);
    enum_column!(declared, principal_display_preferences, color_scheme);
    enum_column!(declared, principal_display_preferences, density);
    optional_enum_column!(declared, principal_display_preferences, sidebar_state);
    enum_column!(declared, project_settings_connections, environment);
    enum_column!(declared, project_settings_connections, credential_status);
    enum_column!(declared, project_settings_connections, lifecycle_status);
    enum_column!(declared, evaluation_target_projections, target_kind);
    enum_column!(
        declared,
        evaluation_target_projections,
        logical_environment_class
    );
    enum_column!(declared, projects, lifecycle_status);
    enum_column!(
        declared,
        evaluation_target_snapshots,
        logical_environment_class
    );
    enum_column!(declared, evaluation_command_receipts, action);
    enum_column!(declared, evaluation_worker_heartbeats, status);
    enum_column!(declared, evaluation_definitions, lifecycle_status);
    enum_column!(declared, evaluation_case_runs, lifecycle_status);
    enum_column!(declared, reusable_resource_drafts, validation_status);
    enum_column!(declared, project_tool_connections, environment);
    enum_column!(declared, project_tool_connections, lifecycle_status);
    enum_column!(declared, reusable_resources, resource_kind);
    enum_column!(declared, reusable_resources, lifecycle_status);
    enum_column!(declared, frozen_spend_import_batches, state);
    enum_column!(declared, agent_draft_editor_roles, role_code);
    declared
}

/// Columns: every declared `Column` exists as a real column and every real column has a declared
/// `Column`; either mismatch means the entity module and the migrations have drifted.
/// `Entity::find().limit(1)` additionally decodes a row where there is one, catching a type
/// mismatch (an `Option<T>` on a `NOT NULL` column, an enum missing a stored value) that a
/// name-only comparison would miss.
///
/// Relations: `Column::X` existing as a Rust identifier proves nothing about the database, and
/// without foreign keys the database proves nothing either. So every `Relation`'s `from_tbl`/
/// `to_tbl` must be a real table or view, every `from_col`/`to_col` a real column of it, and the
/// two sides of each column pair must have the same type (a `uuid` joined to a `text` identity is
/// a wrong target, not a relation).
macro_rules! check_entity_coverage {
    ($schema:expr, $module:ident) => {{
        use hive_persistence::entity::$module::{Column, Entity, Relation};
        let schema: &LiveSchema = $schema;
        let table = stringify!($module);
        assert_eq!(
            Entity.table_name(),
            table,
            "entity module {table} maps a table of another name"
        );
        let declared: BTreeSet<String> = Column::iter()
            .map(|column| <Column as sea_orm::Iden>::to_string(&column))
            .collect();
        let real: BTreeSet<String> = schema
            .columns
            .get(table)
            .map(|columns| columns.keys().cloned().collect())
            .unwrap_or_default();
        let missing_from_entity: Vec<_> = real.difference(&declared).cloned().collect();
        let missing_from_schema: Vec<_> = declared.difference(&real).cloned().collect();
        assert!(
            missing_from_entity.is_empty() && missing_from_schema.is_empty(),
            "{table}: entity module and information_schema.columns disagree; columns only in the schema: {missing_from_entity:?}; columns only in the entity: {missing_from_schema:?}",
        );
        Entity::find()
            .limit(1)
            .all(&schema.db)
            .await
            .unwrap_or_else(|error| panic!("Entity::find().limit(1) for {table}: {error}"));

        for relation in Relation::iter() {
            let definition = relation.def();
            let from_table = table_ref_name(&definition.from_tbl);
            let to_table = table_ref_name(&definition.to_tbl);
            let from_columns = identity_columns(&definition.from_col);
            let to_columns = identity_columns(&definition.to_col);
            assert_eq!(
                from_columns.len(),
                to_columns.len(),
                "{table}: {relation:?} joins {from_columns:?} to {to_columns:?}"
            );
            for (from_column, to_column) in from_columns.iter().zip(&to_columns) {
                let from_type = schema
                    .column_type(&from_table, from_column)
                    .unwrap_or_else(|| {
                        panic!("{table}: {relation:?} names {from_table}.{from_column}, which does not exist")
                    });
                let to_type = schema
                    .column_type(&to_table, to_column)
                    .unwrap_or_else(|| {
                        panic!("{table}: {relation:?} names {to_table}.{to_column}, which does not exist")
                    });
                assert_eq!(
                    from_type, to_type,
                    "{table}: {relation:?} joins {from_table}.{from_column} ({from_type}) to {to_table}.{to_column} ({to_type})"
                );
            }
        }
    }};
}

/// Both directions: an active enum whose values differ from the column's `CHECK` would fail on
/// write or on decode, and a value-list `CHECK` on a column still typed `String` is a closed set
/// the ORM does not know about.
fn check_enum_columns(schema: &LiveSchema) {
    let declared = declared_enum_columns();
    for (table, column, reason) in CLOSED_SETS_THAT_ARE_NOT_ENUMS {
        let key = (table.to_string(), column.to_string());
        assert!(
            schema.closed_sets.contains_key(&key) && !declared.contains_key(&key),
            "{table}.{column} is listed as a closed set that is not an enum ({reason}), but the schema or the entity says otherwise"
        );
    }
    let expected: BTreeMap<&ColumnKey, &BTreeSet<String>> = schema
        .closed_sets
        .iter()
        .filter(|((table, column), _)| {
            !CLOSED_SETS_THAT_ARE_NOT_ENUMS
                .iter()
                .any(|(skipped_table, skipped_column, _)| {
                    skipped_table == table && skipped_column == column
                })
        })
        .collect();
    for (key, values) in &declared {
        let allowed = expected.get(key).unwrap_or_else(|| {
            panic!(
                "{}.{} is an active enum but has no value-list CHECK constraint",
                key.0, key.1
            )
        });
        assert_eq!(
            &values, allowed,
            "{}.{}: the active enum's values differ from the CHECK constraint's",
            key.0, key.1
        );
    }
    let untyped: Vec<_> = expected
        .keys()
        .filter(|key| !declared.contains_key(**key))
        .collect();
    assert!(
        untyped.is_empty(),
        "columns with a value-list CHECK constraint that are not active enums (or are missing from declared_enum_columns): {untyped:?}"
    );
}

#[test]
fn closed_value_sets_are_recognized() {
    let set = |values: &[&str]| Some(values.iter().map(ToString::to_string).collect());
    assert_eq!(
        closed_value_set(
            "((lifecycle_status = ANY (ARRAY['ACTIVE'::text, 'ARCHIVED'::text])))",
            "lifecycle_status"
        ),
        set(&["ACTIVE", "ARCHIVED"])
    );
    assert_eq!(
        closed_value_set(
            "(((outcome_category IS NULL) OR (outcome_category = ANY (ARRAY['PASSED'::text, 'CANCELED'::text]))))",
            "outcome_category"
        ),
        set(&["CANCELED", "PASSED"])
    );
    assert_eq!(
        closed_value_set("((role_code = 'PLATFORM_ADMIN'::text))", "role_code"),
        set(&["PLATFORM_ADMIN"])
    );
    for not_a_value_list in [
        "((revision > 0))",
        "((content_digest ~ '^[0-9a-f]{64}$'::text))",
        "(((comment IS NULL) OR ((length(btrim(comment)) >= 1) AND (btrim(comment) = ANY (ARRAY['A'::text])))))",
        "(((status = 'SATISFIED'::text) = (satisfied_at IS NOT NULL)))",
    ] {
        for column in ["revision", "content_digest", "comment", "status"] {
            assert_eq!(closed_value_set(not_a_value_list, column), None);
        }
    }
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
    let schema = LiveSchema::load(db).await;
    check_enum_columns(&schema);
    check_entity_coverage!(&schema, administration_audit_events);
    check_entity_coverage!(&schema, agent_authoring_audit_events);
    check_entity_coverage!(&schema, agent_draft_audit_events);
    check_entity_coverage!(&schema, agent_draft_editor_roles);
    check_entity_coverage!(&schema, agent_drafts);
    check_entity_coverage!(&schema, agent_operational_summaries);
    check_entity_coverage!(&schema, agent_operational_view_projection);
    check_entity_coverage!(&schema, agent_versions);
    check_entity_coverage!(&schema, agents);
    check_entity_coverage!(&schema, audit_event_projection);
    check_entity_coverage!(&schema, catalog_definitions);
    check_entity_coverage!(&schema, catalog_environments);
    check_entity_coverage!(&schema, catalog_projection_heads);
    check_entity_coverage!(&schema, catalog_releases);
    check_entity_coverage!(&schema, configuration_audit_events);
    check_entity_coverage!(&schema, console_role_assignments);
    check_entity_coverage!(&schema, deployment_approval_decisions);
    check_entity_coverage!(&schema, deployment_approval_handoff_releases);
    check_entity_coverage!(
        &schema,
        deployment_approval_principal_organization_membership_scopes
    );
    check_entity_coverage!(&schema, deployment_approval_principal_organization_scopes);
    check_entity_coverage!(&schema, deployment_approval_principal_project_scopes);
    check_entity_coverage!(&schema, deployment_approval_project_archive_events);
    check_entity_coverage!(&schema, deployment_approval_replay_receipts);
    check_entity_coverage!(&schema, deployment_approval_requirements);
    check_entity_coverage!(&schema, deployment_attempts);
    check_entity_coverage!(&schema, deployment_audit_events);
    check_entity_coverage!(&schema, deployment_evidence_invalidations);
    check_entity_coverage!(&schema, deployment_evidence_snapshots);
    check_entity_coverage!(&schema, deployment_outbox_delivery_audit_repairs);
    check_entity_coverage!(&schema, deployment_outbox_events);
    check_entity_coverage!(&schema, deployment_plan_review_facts);
    check_entity_coverage!(&schema, deployment_plan_versions);
    check_entity_coverage!(&schema, deployment_policy_snapshots);
    check_entity_coverage!(&schema, deployment_project_quota_claims);
    check_entity_coverage!(&schema, deployment_promotion_facts);
    check_entity_coverage!(&schema, deployment_recovery_action_receipts);
    check_entity_coverage!(&schema, deployment_runtime_health);
    check_entity_coverage!(&schema, deployment_stage_events);
    check_entity_coverage!(&schema, deployment_timeline_counters);
    check_entity_coverage!(&schema, deployment_worker_heartbeats);
    check_entity_coverage!(&schema, deployments);
    check_entity_coverage!(&schema, effective_evaluation_capabilities);
    check_entity_coverage!(&schema, environment_definition_versions);
    check_entity_coverage!(&schema, evaluation_artifact_metadata);
    check_entity_coverage!(&schema, evaluation_audit_events);
    check_entity_coverage!(&schema, evaluation_case_runs);
    check_entity_coverage!(&schema, evaluation_command_receipts);
    check_entity_coverage!(&schema, evaluation_definition_drafts);
    check_entity_coverage!(&schema, evaluation_definition_versions);
    check_entity_coverage!(&schema, evaluation_definitions);
    check_entity_coverage!(&schema, evaluation_metric_results);
    check_entity_coverage!(&schema, evaluation_outbox_events);
    check_entity_coverage!(&schema, evaluation_results);
    check_entity_coverage!(&schema, evaluation_runs);
    check_entity_coverage!(&schema, evaluation_target_projections);
    check_entity_coverage!(&schema, evaluation_target_snapshots);
    check_entity_coverage!(&schema, evaluation_worker_heartbeats);
    check_entity_coverage!(&schema, frozen_spend_import_batches);
    check_entity_coverage!(&schema, hive_schema_migration_lock);
    check_entity_coverage!(&schema, hive_schema_migrations);
    check_entity_coverage!(&schema, organization_membership_roles);
    check_entity_coverage!(&schema, organization_memberships);
    check_entity_coverage!(&schema, organizations);
    check_entity_coverage!(&schema, platform_role_assignments);
    check_entity_coverage!(&schema, principal_display_preferences);
    check_entity_coverage!(&schema, principals);
    check_entity_coverage!(&schema, project_approval_policies);
    check_entity_coverage!(&schema, project_approval_policy_versions);
    check_entity_coverage!(&schema, project_budget_policies);
    check_entity_coverage!(&schema, project_budget_policy_versions);
    check_entity_coverage!(&schema, project_dashboard_metrics);
    check_entity_coverage!(&schema, project_dashboard_projection);
    check_entity_coverage!(&schema, project_membership_roles);
    check_entity_coverage!(&schema, project_memberships);
    check_entity_coverage!(&schema, project_settings_connections);
    check_entity_coverage!(&schema, project_tool_connections);
    check_entity_coverage!(&schema, projects);
    check_entity_coverage!(&schema, reusable_resource_drafts);
    check_entity_coverage!(&schema, reusable_resource_versions);
    check_entity_coverage!(&schema, reusable_resources);
}
