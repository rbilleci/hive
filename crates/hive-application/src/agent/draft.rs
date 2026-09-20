//! What an agent draft command answers: the draft (and, for a publication, the version) it left
//! behind, or one typed refusal. The draft and the version are the persistence layer's own rows,
//! so the result is generic over them; reads go through the generated API.

use uuid::Uuid;

/// Ports `AgentDraftMutationProblem.Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDraftProblemKind {
    NotFound,
    Forbidden,
    RevisionConflict,
    InvalidDocument,
    InvalidDraft,
    WarningAcknowledgementRequired,
}

/// A deliberately non-disclosing refusal from an agent draft command. Ports
/// `AgentDraftMutationProblem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDraftMutationProblem {
    pub kind: AgentDraftProblemKind,
    pub resource_id: Option<Uuid>,
    pub expected_revision: i64,
    pub actual_revision: i64,
}

impl AgentDraftMutationProblem {
    pub fn not_found() -> Self {
        Self {
            kind: AgentDraftProblemKind::NotFound,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn forbidden() -> Self {
        Self {
            kind: AgentDraftProblemKind::Forbidden,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn conflict(resource_id: Uuid, expected_revision: i64, actual_revision: i64) -> Self {
        Self {
            kind: AgentDraftProblemKind::RevisionConflict,
            resource_id: Some(resource_id),
            expected_revision,
            actual_revision,
        }
    }

    pub fn invalid_document() -> Self {
        Self {
            kind: AgentDraftProblemKind::InvalidDocument,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn invalid_draft() -> Self {
        Self {
            kind: AgentDraftProblemKind::InvalidDraft,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn warning_acknowledgement_required() -> Self {
        Self {
            kind: AgentDraftProblemKind::WarningAcknowledgementRequired,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }
}

/// One successful draft/version snapshot or one typed refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDraftMutationResult<D, V> {
    pub agent_draft: Option<D>,
    pub agent_version: Option<V>,
    pub problem: Option<AgentDraftMutationProblem>,
}

impl<D, V> AgentDraftMutationResult<D, V> {
    pub fn success(draft: D) -> Self {
        Self {
            agent_draft: Some(draft),
            agent_version: None,
            problem: None,
        }
    }

    pub fn published(draft: D, version: V) -> Self {
        Self {
            agent_draft: Some(draft),
            agent_version: Some(version),
            problem: None,
        }
    }

    pub fn refused(problem: AgentDraftMutationProblem) -> Self {
        Self {
            agent_draft: None,
            agent_version: None,
            problem: Some(problem),
        }
    }
}
