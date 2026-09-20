//! Ports `ConfigurationModels`: read models that deliberately exclude
//! credential values and executable connector behavior, plus the mutation
//! envelope and its typed refusals.

use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogDefinition {
    pub identity: String,
    pub version: String,
    pub kind: String,
    pub display_name: String,
    pub content_digest: String,
    pub available_environments: Vec<String>,
}

/// `released_at` is already the Java `OffsetDateTime.toString()`-equivalent
/// text by the time it reaches this struct — the repository formats it at
/// read time, matching `PostgresConfigurationRepository.catalog`'s own
/// immediate `.toString()` call, unlike every other domain type in this
/// codebase that keeps a `DateTime<Utc>` and formats at the GraphQL boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRelease {
    pub id: String,
    pub source: String,
    pub source_digest: String,
    pub released_at: String,
    pub definitions: Vec<CatalogDefinition>,
    pub environments: Vec<String>,
}

/// `published_by` is the principal id's text form, matching
/// `PostgresConfigurationRepository.versions`'s own `UUID.toString()` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceVersion {
    pub version: i64,
    pub content_digest: String,
    pub canonical_document: String,
    pub dependencies: Vec<String>,
    pub published_at: DateTime<Utc>,
    pub published_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReusableResource {
    pub id: Uuid,
    pub project_id: Uuid,
    pub kind: String,
    pub name: String,
    pub identity: String,
    pub draft_revision: i64,
    pub draft_content: String,
    pub draft_digest: String,
    pub draft_dependencies: Vec<String>,
    pub validation_status: String,
    pub diagnostics: Vec<String>,
    pub published_version: Option<i64>,
    pub versions: Vec<ResourceVersion>,
    pub dependent_resources: Vec<String>,
    pub lifecycle_status: String,
}

/// An inert project descriptor. Transport values are configuration facts and
/// are never executed here. Ports `McpServerConfiguration`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfiguration {
    pub id: Uuid,
    pub project_id: Uuid,
    pub server_id: String,
    pub name: String,
    pub definition_identity: String,
    pub definition_version: String,
    pub environment: String,
    pub enabled: bool,
    pub transport_type: Option<String>,
    pub command: Option<String>,
    pub arguments: Vec<String>,
    pub remote_url: Option<String>,
    pub redacted_bindings: Vec<String>,
    pub tools: Vec<String>,
    pub resources: Vec<String>,
    pub prompts: Vec<String>,
    pub lifecycle_status: String,
    pub status: String,
    pub revision: i64,
    pub dependent_resources: Vec<String>,
}

/// Ports `Problem.Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationProblemKind {
    NotFound,
    Forbidden,
    InvalidInput,
    RevisionConflict,
    InvalidDraft,
    ProtectedLifecycle,
}

/// A deliberately non-disclosing refusal from a configuration command. Ports
/// `Problem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationProblem {
    pub kind: ConfigurationProblemKind,
    pub resource_id: Option<Uuid>,
    pub expected_revision: i64,
    pub actual_revision: i64,
}

impl ConfigurationProblem {
    pub fn unavailable() -> Self {
        Self {
            kind: ConfigurationProblemKind::NotFound,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn forbidden() -> Self {
        Self {
            kind: ConfigurationProblemKind::Forbidden,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn invalid() -> Self {
        Self {
            kind: ConfigurationProblemKind::InvalidInput,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn invalid_draft() -> Self {
        Self {
            kind: ConfigurationProblemKind::InvalidDraft,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn conflict(id: Uuid, expected: i64, actual: i64) -> Self {
        Self {
            kind: ConfigurationProblemKind::RevisionConflict,
            resource_id: Some(id),
            expected_revision: expected,
            actual_revision: actual,
        }
    }

    pub fn lifecycle() -> Self {
        Self {
            kind: ConfigurationProblemKind::ProtectedLifecycle,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }
}

/// One successful resource/MCP-server snapshot or one typed refusal. Ports
/// `Mutation`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfigurationMutationResult {
    pub resource: Option<ReusableResource>,
    pub mcp_server: Option<McpServerConfiguration>,
    pub problem: Option<ConfigurationProblem>,
}

impl ConfigurationMutationResult {
    pub fn resource(value: ReusableResource) -> Self {
        Self {
            resource: Some(value),
            mcp_server: None,
            problem: None,
        }
    }

    pub fn mcp_server(value: McpServerConfiguration) -> Self {
        Self {
            resource: None,
            mcp_server: Some(value),
            problem: None,
        }
    }

    pub fn refused(problem: ConfigurationProblem) -> Self {
        Self {
            resource: None,
            mcp_server: None,
            problem: Some(problem),
        }
    }
}
