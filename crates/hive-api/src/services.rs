//! The one place `hive-api` names a persistence adapter.
//!
//! Every domain service is generic over its port trait (`DeploymentService<R:
//! DeploymentRepository>` and its five siblings), so the choice of `Pg*Repository` is a wiring
//! decision, not a transport one. Holding that choice here keeps `schema/*.rs` naming only the
//! application-layer service it calls: a resolver asks for `services(ctx)?.deployment`, and the
//! concrete adapter behind it is this module's business.
//!
//! Built once per schema rather than once per resolver call. A `DatabaseConnection` is a handle
//! onto a shared pool, and each service is a repository plus stateless policy, so one instance
//! serves every request. `PgDeploymentRepository`'s `next_approval_maintenance_at` gate is the
//! one piece of per-instance state, and it rate-gates only `record_worker_heartbeat`, which the
//! `deployment-worker` subcommand calls on its own instance and no resolver here ever reaches.

use hive_application::administration::AdministrationService;
use hive_application::agent::AgentDraftEditorService;
use hive_application::configuration::ConfigurationService;
use hive_application::console::ConsoleContextService;
use hive_application::deployment::DeploymentService;
use hive_application::evaluation::EvaluationService;
use hive_persistence::administration::PgAdministrationRepository;
use hive_persistence::agent::PgAgentDraftRepository;
use hive_persistence::configuration::PgConfigurationRepository;
use hive_persistence::console::PgConsoleRepository;
use hive_persistence::deployment::PgDeploymentRepository;
use hive_persistence::evaluation::PgEvaluationRepository;
use sea_orm::DatabaseConnection;

/// The application services the GraphQL resolvers call, each already bound to its adapter.
pub struct Services {
    pub administration: AdministrationService<PgAdministrationRepository>,
    pub agent_draft: AgentDraftEditorService<PgAgentDraftRepository>,
    pub configuration: ConfigurationService<PgConfigurationRepository>,
    pub console: ConsoleContextService<PgConsoleRepository>,
    pub deployment: DeploymentService<PgDeploymentRepository>,
    pub evaluation: EvaluationService<PgEvaluationRepository>,
}

impl Services {
    pub fn new(db: &DatabaseConnection) -> Self {
        Self {
            administration: AdministrationService::new(PgAdministrationRepository::new(db.clone())),
            agent_draft: AgentDraftEditorService::new(PgAgentDraftRepository::new(db.clone())),
            configuration: ConfigurationService::new(PgConfigurationRepository::new(db.clone())),
            console: ConsoleContextService::new(PgConsoleRepository::new(db.clone())),
            deployment: DeploymentService::new(PgDeploymentRepository::new(db.clone())),
            evaluation: EvaluationService::new(PgEvaluationRepository::new(db.clone())),
        }
    }
}
