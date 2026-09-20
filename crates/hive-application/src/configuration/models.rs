//! What a configuration command answers: the reusable resource or the MCP server row it left
//! behind, or one typed refusal. The rows are the persistence layer's own, so the result is
//! generic over them; reads go through the generated API.

use uuid::Uuid;

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

/// One stored resource or MCP server row, or one typed refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationMutationResult<R, M> {
    pub resource: Option<R>,
    pub mcp_server: Option<M>,
    pub problem: Option<ConfigurationProblem>,
}

impl<R, M> ConfigurationMutationResult<R, M> {
    pub fn resource(value: R) -> Self {
        Self {
            resource: Some(value),
            mcp_server: None,
            problem: None,
        }
    }

    pub fn mcp_server(value: M) -> Self {
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
