mod administration;
mod agent;
mod audit;
mod configuration;
mod connection;
mod console;
mod deployment;
mod evaluation;
mod organization;
mod project;

use async_graphql::{Context, EmptySubscription, MergedObject, Object, Schema, SimpleObject};
use sqlx::PgPool;
use uuid::Uuid;

/// The verified request principal, inserted into the async-graphql request's data
/// map by the `/graphql` handler after `SessionVerifier` succeeds — the same
/// ordering `DirectoryServer.graphql()` uses (authenticate, then execute).
pub struct RequestPrincipal(pub Uuid);

/// The per-HTTP-request correlation id, inserted alongside `RequestPrincipal`. Mirrors
/// `environment.getGraphQlContext().get("requestCorrelationId")`: the same id the
/// `X-Request-Id` response header carries.
pub struct RequestCorrelationId(pub Uuid);

/// Mirrors the SDL's `type Principal { id: ID!, subject: String! }`.
#[derive(SimpleObject)]
pub struct Principal {
    pub id: async_graphql::ID,
    pub subject: String,
}

pub struct CoreQueries;

#[Object]
impl CoreQueries {
    /// Mirrors `GraphqlSchemaFactory`'s `currentPrincipal` field: both `id` and
    /// `subject` are the principal UUID's text form.
    async fn current_principal(&self, ctx: &Context<'_>) -> async_graphql::Result<Principal> {
        let principal = ctx
            .data::<RequestPrincipal>()
            .map_err(|_| async_graphql::Error::new("An authenticated principal is required."))?;
        let text = principal.0.to_string();
        Ok(Principal {
            id: async_graphql::ID(text.clone()),
            subject: text,
        })
    }
}

/// One feature bundle per Java `*Graphql.types(...)` factory. Merged into a single
/// `Query` root type, matching `GraphqlSchemaFactory`'s composition.
#[derive(MergedObject)]
#[graphql(name = "Query")]
pub struct RootQuery(
    CoreQueries,
    organization::OrganizationQueries,
    project::ProjectQueries,
    console::ConsoleQueries,
    administration::AdministrationQueries,
    agent::AgentQueries,
    configuration::ConfigurationQueries,
    deployment::DeploymentQueries,
    evaluation::EvaluationQueries,
    audit::AuditQueries,
);

#[derive(MergedObject)]
#[graphql(name = "Mutation")]
pub struct RootMutation(
    console::ConsoleMutations,
    administration::AdministrationMutations,
    agent::AgentMutations,
    configuration::ConfigurationMutations,
    deployment::DeploymentMutations,
    evaluation::EvaluationMutations,
);

pub type HiveSchema = Schema<RootQuery, RootMutation, EmptySubscription>;

/// Depth 20 / complexity 500 mirror `GraphqlExecutor.java`'s instrumentation limits.
/// `RTD-HTTP-GRAPHQL`. The pool is schema-global data (every request shares one
/// pool), not per-request data.
pub fn build_schema(pool: PgPool) -> HiveSchema {
    Schema::build(
        RootQuery(
            CoreQueries,
            organization::OrganizationQueries,
            project::ProjectQueries,
            console::ConsoleQueries,
            administration::AdministrationQueries,
            agent::AgentQueries,
            configuration::ConfigurationQueries,
            deployment::DeploymentQueries,
            evaluation::EvaluationQueries,
            audit::AuditQueries,
        ),
        RootMutation(
            console::ConsoleMutations,
            administration::AdministrationMutations,
            agent::AgentMutations,
            configuration::ConfigurationMutations,
            deployment::DeploymentMutations,
            evaluation::EvaluationMutations,
        ),
        EmptySubscription,
    )
    .data(pool)
    .limit_depth(20)
    .limit_complexity(500)
    .finish()
}
