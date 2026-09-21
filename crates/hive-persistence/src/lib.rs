//! The SeaORM persistence tier: the generated entity modules, the capability evaluator, and one
//! module per write domain.
//!
//! Each of the five write domains — `administration`, `agent`, `configuration`, `deployment`,
//! `evaluation` — is laid out the same way, so a file name means the same thing everywhere:
//!
//! * `mod.rs` — the repository struct and its trait `impl`, dispatching one call per command.
//! * `queries.rs` — the hand-written reads that stay repository methods. Absent where every read
//!   is a generated entity query.
//! * `mutations.rs` — the domain's commands, one transaction each. A domain with enough of them
//!   makes it a directory instead, one module per command or per command family, with what they
//!   share in its `mod.rs`: `deployment::mutations` and `evaluation::mutations`.
//! * `rows.rs` — the row-level layer the commands, the workers and the computed fields share: the
//!   row types and their mappers, the locked single-row reads and writes, and the audit writer.
//! * `computed.rs` — the computed fields the generated entity objects carry.
//!
//! A domain with a separate engine keeps it beside those, in its own file or its own directory:
//! `deployment::worker` and `evaluation::worker`, `deployment::decisions`,
//! `deployment::approval`, `deployment::loaders`, `administration::scopes`.

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
