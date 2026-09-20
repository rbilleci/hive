//! Ports the agent draft/version records: `AgentDraft`, `AgentDraftMutationProblem`,
//! `AgentDraftMutationResult`, `AgentDraftReview`, `AgentVersion`,
//! `AgentVersionComparison`.

use crate::agent::canonical_document::AgentDraftDiagnostic;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Ports `AgentDraft`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDraft {
    pub agent_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub document: String,
    pub revision: i64,
    pub validation_status: String,
    pub validation_diagnostics: Vec<AgentDraftDiagnostic>,
    pub validated_at: Option<DateTime<Utc>>,
    pub can_update: bool,
    pub can_publish: bool,
    pub latest_version: Option<i64>,
}

/// Ports `AgentVersion`: an immutable published configuration, not a deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentVersion {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub number: i64,
    pub slug: String,
    pub display_name: String,
    pub canonical_document: String,
    pub content_digest: String,
    pub dependencies: Vec<String>,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub published_by: Uuid,
    pub published_at: DateTime<Utc>,
}

/// Ports `AgentVersionComparison`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentVersionComparison {
    pub from: AgentVersion,
    pub to: AgentVersion,
    pub changed_sections: Vec<String>,
}

/// Ports `AgentDraftReview`: pre-publish review facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDraftReview {
    pub draft: AgentDraft,
    pub canonical_document: String,
    pub content_digest: String,
    pub dependencies: Vec<String>,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub changed_sections: Vec<String>,
    pub diagnostics: Vec<AgentDraftDiagnostic>,
}

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

/// One successful draft/version snapshot or one typed refusal. Ports
/// `AgentDraftMutationResult`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentDraftMutationResult {
    pub agent_draft: Option<AgentDraft>,
    pub agent_version: Option<AgentVersion>,
    pub problem: Option<AgentDraftMutationProblem>,
}

impl AgentDraftMutationResult {
    pub fn success(draft: AgentDraft) -> Self {
        Self {
            agent_draft: Some(draft),
            agent_version: None,
            problem: None,
        }
    }

    pub fn published(draft: AgentDraft, version: AgentVersion) -> Self {
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
