//! SeaORM entity modules, one per migration-created table or view. These are the single source
//! of truth for database access and for the Seaography-generated GraphQL API
//! (`docs/idiomatic-seaography-plan.md`, A1).
//!
//! **Relations.** The schema has no foreign keys (Aurora DSQL has none), so every `Relation` is
//! declared by column: a `belongs_to` on the entity that holds the reference, and the reverse
//! `has_many` (or `has_one`, where the referencing column is unique on its own) on the target,
//! each with its `Related` impl. Where an entity refers to the same target twice
//! (`deployment_recovery_action_receipts` to its source and result deployment) or to itself
//! (`evaluation_runs.source_run_id`, `evaluation_definition_versions.based_on_version_id`), the
//! variants have distinct names, only the first has a `Related` impl (Rust allows one per target),
//! and the reverse of the others is declared with `via_rel`; use those through `Relation::X.def()`.
//! Columns that look like references but are not relations stay undeclared: `request_id`,
//! `correlation_id`, idempotency keys (`request_key`, `case_slot`), worker names (`claimed_by`,
//! `worker_id`), and ids whose target table depends on another column (`scope_id`, `subject_id`,
//! `resource_id` and `source_event_id` on the audit tables and view, `target_id` on
//! `evaluation_runs` and `evaluation_target_projections`).
//!
//! **`RelatedEntity`** is the list Seaography turns into relation fields. It is filled in slice by
//! slice, as each entity is registered with the GraphQL schema: a variant pointing at an entity
//! that is not registered breaks the schema build. It is therefore a subset of `Relation`.
//!
//! **Closed-set text columns** (`TEXT` with a `CHECK (... IN (...))`) are active enums (`enums`).
//! The views have no constraints of their own, so their text columns stay `String`.
//!
//! `tests/entity_coverage.rs` checks all of this against the live, migrated schema: columns,
//! relation tables, columns and column types, and each active enum's values against its `CHECK`.

pub mod enums;

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
