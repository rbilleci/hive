//! The Seaography-composed schema (`GSR-DYNAMIC-SCHEMA`), the only GraphQL tier since
//! `GSR-PHASE-4` deleted the static `async-graphql` macro tier this port replaced
//! (`docs/graphql-seaography-rewrite-plan.md`).

// `GSR-WIRE-CASE`: Seaography spells a GraphQL name from the Rust identifier verbatim, so a
// resolver or argument this module (and its submodules) exposes on the wire is named in wire
// case, not `snake_case`.
#![allow(non_snake_case, non_camel_case_types)]

mod administration;
mod agent;
mod configuration;
mod console;
mod deployment;
mod evaluation;
mod principal;
pub(crate) mod problem;
pub(crate) mod scalars;
pub(crate) mod tenant_hooks;

use hive_persistence::entity::{
    agent_drafts, agent_operational_view_projection, agent_versions, agents,
    audit_event_projection, catalog_definitions, catalog_environments, catalog_projection_heads,
    catalog_releases, deployment_attempts, deployment_evidence_snapshots,
    deployment_plan_review_facts, deployment_plan_versions, deployment_policy_snapshots,
    deployment_runtime_health, deployments, environment_definition_versions,
    evaluation_artifact_metadata, evaluation_audit_events, evaluation_case_runs,
    evaluation_definition_drafts, evaluation_definition_versions, evaluation_definitions,
    evaluation_metric_results, evaluation_runs, evaluation_target_projections,
    evaluation_target_snapshots, organization_membership_roles, organization_memberships,
    organizations, principal_display_preferences, principals, project_approval_policies,
    project_approval_policy_versions, project_budget_policies, project_budget_policy_versions,
    project_dashboard_projection, project_membership_roles, project_memberships,
    project_settings_connections, project_tool_connections, projects, reusable_resource_drafts,
    reusable_resource_versions, reusable_resources,
};
use sea_orm::DatabaseConnection;
use seaography::{
    Builder, BuilderContext, CustomFields, EntityQueryFieldConfig, LifecycleHooks, TypesMapConfig,
};
use std::sync::LazyLock;
use uuid::Uuid;

/// The verified request principal, inserted into the async-graphql request's data map by the
/// `/graphql` handler after `SessionVerifier` succeeds — the same ordering `DirectoryServer.
/// graphql()` uses (authenticate, then execute).
pub struct RequestPrincipal(pub Uuid);

/// The per-HTTP-request correlation id, inserted alongside `RequestPrincipal`. Mirrors
/// `environment.getGraphQlContext().get("requestCorrelationId")`: the same id the
/// `X-Request-Id` response header carries.
pub struct RequestCorrelationId(pub Uuid);

/// Marks a resolver error as a failed PostgreSQL crossing. It travels as the error's `source`,
/// which async-graphql never serializes, so the response body keeps its message-only shape while
/// the `/graphql` handler answers `503`. Ports `DeploymentUnavailableException`, which
/// `PostgresDeploymentRepository` raises for every `SQLException`.
pub struct DependencyUnavailable(pub String);

impl std::fmt::Display for DependencyUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

static CONTEXT: LazyLock<BuilderContext> = LazyLock::new(|| BuilderContext {
    hooks: LifecycleHooks::new(tenant_hooks::TenantHooks),
    // Case-insensitive `ilike` on string filters, for the console's search boxes.
    entity_query_field: EntityQueryFieldConfig {
        use_ilike: true,
        ..Default::default()
    },
    // RFC 3339 timestamps, which every client parses; the default is chrono's display format.
    types: TypesMapConfig {
        timestamp_rfc3339: true,
        ..Default::default()
    },
    ..Default::default()
});

/// The `&'static BuilderContext` every domain module's hand-built resolvers need; a plain
/// accessor since `CONTEXT` itself is private to this file.
pub(crate) fn context() -> &'static BuilderContext {
    &CONTEXT
}

pub fn build(db: DatabaseConnection) -> async_graphql::dynamic::Schema {
    let mut builder = Builder::new(&CONTEXT, db.clone());
    // `Builder::new` bakes an internal `_ping` field onto `Mutation` (`seaography-2.0.0-rc.9/src/
    // builder.rs`: `Object::new("Mutation").field(Field::new("_ping", ...))`) — a Seaography-side
    // liveness-probe artifact, not part of the frozen contract. Every real mutation this schema
    // registers lands in `builder.mutations: Vec<Field>` and is folded onto `builder.mutation`
    // only later, inside `schema_builder()`, so replacing the base `Object` here (before any
    // `register_custom_mutation` call) drops `_ping` without losing anything real. Slipped past
    // every earlier check because none of them asserted the *exact* field set on `Mutation` —
    // `check:schema:contract`'s reachability walk only verifies fields the console actually
    // selects, and this port's own whole-schema comparisons only ever checked specific named
    // fields, never "no extra fields exist" — until `check:integration:agent-draft-editor`
    // (`GSR-PHASE-3`) did an exhaustive introspection comparison and caught it.
    builder.mutation = async_graphql::dynamic::Object::new("Mutation");

    // Generated API: standard Seaography entity queries, relations and dataloaders. Generated
    // CRUD mutations stay off (`docs/idiomatic-seaography-plan.md`, A5).
    seaography::register_entity!(builder, organizations, mutation: false);
    seaography::register_entity!(builder, projects, mutation: false);
    seaography::register_entity!(builder, agents, mutation: false);
    seaography::register_entity!(builder, agent_versions, mutation: false);
    seaography::register_entity!(builder, agent_drafts, mutation: false);
    seaography::register_entity!(builder, project_dashboard_projection, mutation: false);
    seaography::register_entity!(builder, agent_operational_view_projection, mutation: false);
    seaography::register_entity!(builder, principals, mutation: false);
    seaography::register_entity!(builder, principal_display_preferences, mutation: false);
    seaography::register_entity!(builder, catalog_projection_heads, mutation: false);
    seaography::register_entity!(builder, catalog_releases, mutation: false);
    seaography::register_entity!(builder, catalog_definitions, mutation: false);
    seaography::register_entity!(builder, catalog_environments, mutation: false);
    seaography::register_entity!(builder, reusable_resources, mutation: false);
    seaography::register_entity!(builder, reusable_resource_drafts, mutation: false);
    seaography::register_entity!(builder, reusable_resource_versions, mutation: false);
    seaography::register_entity!(builder, project_tool_connections, mutation: false);
    seaography::register_entity!(builder, organization_memberships, mutation: false);
    seaography::register_entity!(builder, organization_membership_roles, mutation: false);
    seaography::register_entity!(builder, project_memberships, mutation: false);
    seaography::register_entity!(builder, project_membership_roles, mutation: false);
    seaography::register_entity!(builder, project_budget_policies, mutation: false);
    seaography::register_entity!(builder, project_budget_policy_versions, mutation: false);
    seaography::register_entity!(builder, project_approval_policies, mutation: false);
    seaography::register_entity!(builder, project_approval_policy_versions, mutation: false);
    seaography::register_entity!(builder, project_settings_connections, mutation: false);
    seaography::register_entity!(builder, audit_event_projection, mutation: false);
    seaography::register_entity!(builder, evaluation_definitions, mutation: false);
    seaography::register_entity!(builder, evaluation_definition_drafts, mutation: false);
    seaography::register_entity!(builder, evaluation_definition_versions, mutation: false);
    seaography::register_entity!(builder, evaluation_runs, mutation: false);
    seaography::register_entity!(builder, evaluation_case_runs, mutation: false);
    seaography::register_entity!(builder, evaluation_metric_results, mutation: false);
    seaography::register_entity!(builder, evaluation_artifact_metadata, mutation: false);
    seaography::register_entity!(builder, evaluation_audit_events, mutation: false);
    seaography::register_entity!(builder, evaluation_target_snapshots, mutation: false);
    seaography::register_entity!(builder, evaluation_target_projections, mutation: false);
    seaography::register_entity!(builder, deployments, mutation: false);
    seaography::register_entity!(builder, deployment_attempts, mutation: false);
    seaography::register_entity!(builder, deployment_plan_versions, mutation: false);
    seaography::register_entity!(builder, deployment_plan_review_facts, mutation: false);
    seaography::register_entity!(builder, deployment_policy_snapshots, mutation: false);
    seaography::register_entity!(builder, deployment_runtime_health, mutation: false);
    seaography::register_entity!(builder, deployment_evidence_snapshots, mutation: false);
    seaography::register_entity!(builder, environment_definition_versions, mutation: false);

    // Computed fields (A4): `capabilities`, the codes the requesting principal holds at the row's
    // scope. The `#[CustomFields] impl Model` blocks are in `hive_persistence::console`.
    attach_computed_fields::<organizations::Model>(&mut builder, "Organizations");
    attach_computed_fields::<projects::Model>(&mut builder, "Projects");
    attach_computed_fields::<principals::Model>(&mut builder, "Principals");
    // Agent authoring (`hive_persistence::agent::computed`): `Agents.draft`,
    // `AgentDrafts.canUpdate` / `canPublish` / `review`, `AgentVersions.comparison`.
    attach_computed_fields::<agents::Model>(&mut builder, "Agents");
    attach_computed_fields::<agent_drafts::Model>(&mut builder, "AgentDrafts");
    attach_computed_fields::<agent_versions::Model>(&mut builder, "AgentVersions");
    // Configuration (`hive_persistence::configuration::computed`): `ReusableResources.draft` /
    // `dependentResources`, `ProjectToolConnections.arguments` / `remoteUrl` / `status` /
    // `dependentResources`.
    attach_computed_fields::<reusable_resources::Model>(&mut builder, "ReusableResources");
    attach_computed_fields::<project_tool_connections::Model>(
        &mut builder,
        "ProjectToolConnections",
    );
    // Administration (`hive_persistence::administration::computed`): `roleCodes`,
    // `projectAccessSummary`, the budget policy's `currentVersion` / `status`, the approval
    // policy's `currentVersion` and a version's `rules`. `assignableRoles` and
    // `availablePrincipals` are on `Organizations` and `Projects`, attached above.
    attach_computed_fields::<organization_memberships::Model>(
        &mut builder,
        "OrganizationMemberships",
    );
    attach_computed_fields::<project_memberships::Model>(&mut builder, "ProjectMemberships");
    attach_computed_fields::<project_budget_policies::Model>(&mut builder, "ProjectBudgetPolicies");
    attach_computed_fields::<project_approval_policies::Model>(
        &mut builder,
        "ProjectApprovalPolicies",
    );
    attach_computed_fields::<project_approval_policy_versions::Model>(
        &mut builder,
        "ProjectApprovalPolicyVersions",
    );

    // Evaluation (`hive_persistence::evaluation::computed`): the definition's `canAuthor` /
    // `canPublish` / `draft` / `latestVersion`, the redacted `canonicalDocument` and
    // `diagnostics`, a version's `comparison`, a run's `durationMillis` / `failureSummary` /
    // `deploymentEvidenceDisposition` / `target`, and an audit event's `summary`.
    // `Projects.compatibleEvaluationTargets` rides on the `Projects` block attached above.
    attach_computed_fields::<evaluation_definitions::Model>(&mut builder, "EvaluationDefinitions");
    attach_computed_fields::<evaluation_definition_drafts::Model>(
        &mut builder,
        "EvaluationDefinitionDrafts",
    );
    attach_computed_fields::<evaluation_definition_versions::Model>(
        &mut builder,
        "EvaluationDefinitionVersions",
    );
    attach_computed_fields::<evaluation_runs::Model>(&mut builder, "EvaluationRuns");
    attach_computed_fields::<evaluation_audit_events::Model>(&mut builder, "EvaluationAuditEvents");

    // Deployment (`hive_persistence::deployment::computed`): the deployment's `plan`,
    // `currentAttempt`, `rollbackTarget` and `timeline`, a plan's retained `review`, and the
    // time-dependent evidence `state`.
    attach_computed_fields::<deployments::Model>(&mut builder, "Deployments");
    attach_computed_fields::<deployment_plan_versions::Model>(
        &mut builder,
        "DeploymentPlanVersions",
    );
    attach_computed_fields::<deployment_evidence_snapshots::Model>(
        &mut builder,
        "DeploymentEvidenceSnapshots",
    );

    // Audit (`hive_persistence::audit`): `sourceIp`, `userAgent` and `sensitiveFieldsRedacted`,
    // answered by `AUDIT_SENSITIVE.VIEW` at the event's scope.
    attach_computed_fields::<audit_event_projection::Model>(&mut builder, "AuditEventProjection");

    // Custom tier: `#[CustomFields]` only builds the *field*; a return type's own object
    // definition needs its own `register_custom_output` call (`GSR-PHASE-0` found this the hard
    // way: `SchemaError("Type \"Principal\" not found")` at `finish()` without it).
    builder.register_custom_output::<principal::Principal>();
    builder.register_custom_query::<principal::CoreQueries>();

    problem::register(&mut builder);
    console::register(&mut builder);
    agent::register(&mut builder);
    configuration::register(&mut builder);
    administration::register(&mut builder);
    evaluation::register(&mut builder);
    deployment::register(&mut builder);

    let schema_builder = builder
        .set_depth_limit(Some(20))
        .set_complexity_limit(Some(500))
        .schema_builder();
    // `Builder` has no interface vector; each module's `Interface`s attach directly to the
    // `SchemaBuilder` here instead, after the module's implementor `Object`s already landed on
    // `builder.outputs` above.
    let schema_builder = console::interfaces()
        .into_iter()
        .chain(deployment::interfaces())
        .fold(schema_builder, |schema_builder, interface| {
            schema_builder.register(interface)
        });
    // A `CustomOutputType`/`CustomInputType` impl only supplies a *type ref* (`TypeRef::named_nn`)
    // for a scalar; the scalar's own `Type` still needs registering once, the same way an
    // `Object`/`Interface` does — missed here until `agent.rs` became the first production module
    // to actually use `scalars::Json`, and `finish()` failed with `SchemaError("Type \"JSON\" not
    // found")`. `scalars.rs`'s own test schema already does this (it must, for the same reason),
    // but that registration is scoped to its own throwaway schema, not this one. `evaluation.rs`
    // is the first production module to use `scalars::Long`, needing the identical treatment.
    let schema_builder = schema_builder
        .register(
            async_graphql::dynamic::Scalar::new("JSON").description("A structured JSON value."),
        )
        .register(
            async_graphql::dynamic::Scalar::new("Long").description("A signed 64-bit integer."),
        );
    // `db` (the `sea_orm::DatabaseConnection`) backs both the generated `organization_read` entity
    // tier and every hand-written resolver across all nine domain modules, which call
    // `ctx.data::<DatabaseConnection>()` to build their own `Pg*Repository` — it must be present,
    // or every resolver fails at request time with `Data ... does not exist`, a gap the SDL
    // shape/interface tests could never catch since none of them execute a resolver through this
    // real `build()` function (only through their own throwaway schemas).
    let schema = schema_builder.data(db);
    schema
        .finish()
        .expect("the schema composes without a type-name collision")
}

/// Adds a model's `#[CustomFields]` to the object `register_entity!` generated for it. The
/// resolvers receive the row as `&self`: the generated object's parent value is the `Model`.
fn attach_computed_fields<M: CustomFields>(builder: &mut Builder, type_name: &str) {
    let index = builder
        .outputs
        .iter()
        .position(|object| object.type_name() == type_name)
        .unwrap_or_else(|| panic!("{type_name} is registered before its computed fields"));
    let object = builder.outputs.swap_remove(index);
    let object = M::to_fields(builder.context)
        .into_iter()
        .fold(object, |object, field| object.field(field));
    builder.outputs.push(object);
}

/// Prints `schema`'s SDL with the one post-processing fix Seaography's own build needs (see
/// `strip_dangling_subscription_root`), the single place `hive schema-sdl` and
/// `npm run generate:schema` both read the SDL from.
pub fn sdl(schema: &async_graphql::dynamic::Schema) -> String {
    strip_dangling_subscription_root(schema.sdl())
}

/// Works around a Seaography defect (recorded in `docs/graphql-seaography-rewrite-plan.md`'s risk
/// table): `Builder::new` (`seaography-2.0.0-rc.9/src/builder.rs:82-87`) hard-codes the dynamic
/// schema's `subscription_type` to `Some("Subscription")` at construction, with no public setter
/// to clear it and no way to influence it through `Builder`'s only public path to a `SchemaBuilder`
/// (`schema_builder()` threads the same private, already-built value through unchanged). Because
/// this schema never registers a subscription, the printed SDL always ends with `schema { query:
/// Query mutation: Mutation subscription: Subscription }` and never defines `type Subscription`
/// — invalid per strict SDL validation, and a mismatch with the frozen contract either way (which
/// has no `schema { ... }` block at all, since its root names are the async-graphql-default
/// `Query`/`Mutation` with no subscription). Guarded to match only the exact known-bad block, so a
/// future Seaography upgrade that fixes this (or a schema that legitimately adds a subscription)
/// fails loudly here instead of silently mangling a correct SDL.
fn strip_dangling_subscription_root(sdl: String) -> String {
    const DANGLING_BLOCK: &str =
        "schema {\n\tquery: Query\n\tmutation: Mutation\n\tsubscription: Subscription\n}\n";
    match sdl.strip_suffix(DANGLING_BLOCK) {
        Some(without_block) => without_block.to_string(),
        None => panic!(
            "the dynamic engine's SDL no longer ends with the expected dangling `schema {{ ... subscription: Subscription }}` block; \
             either Seaography now omits it (drop `strip_dangling_subscription_root`) or a real subscription was added (this workaround is now wrong)"
        ),
    }
}

#[cfg(test)]
mod sdl_tests {
    use super::strip_dangling_subscription_root;

    #[test]
    fn strips_the_known_dangling_block_and_nothing_else() {
        let sdl = "type Query {\n\tfoo: String\n}\nschema {\n\tquery: Query\n\tmutation: Mutation\n\tsubscription: Subscription\n}\n";
        assert_eq!(
            strip_dangling_subscription_root(sdl.to_string()),
            "type Query {\n\tfoo: String\n}\n"
        );
    }

    #[test]
    #[should_panic(expected = "no longer ends with the expected dangling")]
    fn panics_loudly_if_the_dangling_block_is_ever_absent() {
        strip_dangling_subscription_root("type Query {\n\tfoo: String\n}\n".to_string());
    }
}

#[cfg(test)]
mod generated_entity_tests {
    use hive_persistence::entity::{
        agent_drafts, agent_operational_view_projection, agent_versions, agents,
        audit_event_projection, catalog_definitions, catalog_environments,
        catalog_projection_heads, catalog_releases, deployment_attempts,
        deployment_evidence_snapshots, deployment_plan_review_facts, deployment_plan_versions,
        deployment_policy_snapshots, deployment_runtime_health, deployments,
        environment_definition_versions, evaluation_artifact_metadata, evaluation_audit_events,
        evaluation_case_runs, evaluation_definition_drafts, evaluation_definition_versions,
        evaluation_definitions, evaluation_metric_results, evaluation_runs,
        evaluation_target_projections, evaluation_target_snapshots, organization_membership_roles,
        organization_memberships, organizations, principal_display_preferences, principals,
        project_approval_policies, project_approval_policy_versions, project_budget_policies,
        project_budget_policy_versions, project_dashboard_projection, project_membership_roles,
        project_memberships, project_settings_connections, project_tool_connections, projects,
        reusable_resource_drafts, reusable_resource_versions, reusable_resources,
    };
    use sea_orm::{Iterable, PrimaryKeyToColumn};

    /// Seaography applies `orderBy` columns in declaration order, and the console always adds the
    /// primary key as the last tie-break. That only works while the key is declared last.
    #[test]
    fn every_generated_entity_declares_its_primary_key_last() {
        macro_rules! check {
            ($($entity:ident),+) => {$({
                let columns: Vec<String> = $entity::Column::iter().map(|column| format!("{column:?}")).collect();
                let keys: Vec<String> = $entity::PrimaryKey::iter()
                    .map(|key| format!("{:?}", key.into_column()))
                    .collect();
                assert_eq!(
                    columns[columns.len() - keys.len()..],
                    keys[..],
                    "{}: declare the primary key after every other column",
                    stringify!($entity)
                );
            })+};
        }
        check!(
            organizations,
            projects,
            agents,
            agent_versions,
            agent_drafts,
            project_dashboard_projection,
            agent_operational_view_projection,
            principals,
            principal_display_preferences,
            catalog_projection_heads,
            catalog_releases,
            catalog_definitions,
            catalog_environments,
            reusable_resources,
            reusable_resource_drafts,
            reusable_resource_versions,
            project_tool_connections,
            organization_memberships,
            organization_membership_roles,
            project_memberships,
            project_membership_roles,
            project_budget_policies,
            project_budget_policy_versions,
            project_approval_policies,
            project_approval_policy_versions,
            project_settings_connections,
            audit_event_projection,
            evaluation_definitions,
            evaluation_definition_drafts,
            evaluation_definition_versions,
            evaluation_runs,
            evaluation_case_runs,
            evaluation_metric_results,
            evaluation_artifact_metadata,
            evaluation_audit_events,
            evaluation_target_snapshots,
            evaluation_target_projections,
            deployments,
            deployment_attempts,
            deployment_plan_versions,
            deployment_plan_review_facts,
            deployment_policy_snapshots,
            deployment_runtime_health,
            deployment_evidence_snapshots,
            environment_definition_versions
        );
        // Every registered entity must be in the list above.
        let registered = include_str!("mod.rs")
            .lines()
            .filter(|line| {
                line.trim_start()
                    .starts_with("seaography::register_entity!(")
            })
            .count();
        assert_eq!(
            registered, 45,
            "add the newly registered entity to this test"
        );
    }
}
