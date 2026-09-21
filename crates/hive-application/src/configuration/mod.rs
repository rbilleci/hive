//! Configuration rules with no storage in them: the identity value types (`TypedReference`,
//! `resource_identity`), canonical documents and digests, the command result and its refusals,
//! the persistence boundary of the commands (`ConfigurationRepository`), and command-shape
//! validation (`ConfigurationService`).

pub mod canonical;
pub mod identity;
pub mod models;
pub mod repository;
pub mod service;

pub use canonical::{digest, document, sorted};
pub use identity::{resource_identity, TypedReference};
pub use models::{ConfigurationMutationResult, ConfigurationProblem, ConfigurationProblemKind};
pub use repository::ConfigurationRepository;
pub use service::{mcp_server_status, safe_arguments, safe_remote_url, ConfigurationService};
