//! Ports `application/configuration` in full: the identity value types
//! (`TypedReference`, `ResourceIdentity`), `CanonicalConfiguration`, the read
//! models and mutation envelope (`ConfigurationModels`), the persistence
//! boundary (`ConfigurationRepository`), and the command-shape validation
//! layer (`ConfigurationService`).

pub mod canonical;
pub mod identity;
pub mod models;
pub mod repository;
pub mod service;

pub use canonical::{digest, document, sorted};
pub use identity::{resource_identity, TypedReference};
pub use models::{
    CatalogDefinition, CatalogRelease, ConfigurationMutationResult, ConfigurationProblem,
    ConfigurationProblemKind, McpServerConfiguration, ResourceVersion, ReusableResource,
};
pub use repository::{ConfigurationRepository, RepositoryError as ConfigurationRepositoryError};
pub use service::ConfigurationService;
