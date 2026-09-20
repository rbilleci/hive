pub mod canonical_document;
pub mod directory;
pub mod draft;
pub mod draft_service;
pub mod operational_view;
pub mod project_agent_directory;

pub use canonical_document::AgentDraftDiagnostic;
pub use directory::{Agent, AgentCursor, AgentDirectoryProject, AgentFilter, AgentPage};
pub use draft::{
    AgentDraft, AgentDraftMutationProblem, AgentDraftMutationResult, AgentDraftProblemKind,
    AgentDraftReview, AgentVersion, AgentVersionComparison,
};
pub use draft_service::{AgentDraftEditorService, AgentDraftRepository};
pub use operational_view::{
    AgentOperationalView, AgentOperationalViewQueryService, AgentOperationalViewRepository,
    AliasTargetsSummary, DeploymentSummary, DraftValidationSummary, EvaluationSummary,
    PublishedVersionSummary, RuntimeHealthSummary,
};
pub use project_agent_directory::{
    ProjectAgentDirectoryQueryService, ProjectAgentDirectoryRepository,
};

pub use directory::CursorError as AgentCursorError;
pub use draft_service::RepositoryError as AgentDraftRepositoryError;
pub use operational_view::RepositoryError as AgentOperationalViewRepositoryError;
pub use project_agent_directory::RepositoryError as ProjectAgentDirectoryRepositoryError;
