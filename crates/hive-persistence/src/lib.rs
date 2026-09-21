pub mod administration;
pub mod agent;
pub mod approval_maintenance;
pub mod audit;
pub mod authority;
pub mod capability;
pub mod configuration;
pub mod connection;
pub mod console;
pub mod deployment;
pub mod entity;
pub mod error;
pub mod evaluation;
pub mod guard;
pub mod migrator;
pub mod retry;
mod status;
pub mod worker_health;

pub use approval_maintenance::{ApprovalMaintenanceHealth, ApprovalMaintenanceState};
pub use connection::ConnectionFactory;
pub use migrator::{migrate_and_seed, Dialect, MigratorError};
/// Re-exported so a caller outside `hive-persistence`/`hive-api` (the two crates the
/// architecture rules permit to depend on `sea-orm` directly, e.g. `hive`'s binary entry point)
/// can still name `ConnectionFactory::dynamic()`'s return type without adding its own `sea-orm`
/// dependency edge.
pub use sea_orm::DatabaseConnection;
