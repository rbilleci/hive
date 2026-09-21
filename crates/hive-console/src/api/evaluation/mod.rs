//! Evaluation definitions, their immutable versions and their runs, all read through the
//! Seaography-generated entity API. The server decides which rows a principal reads; a definition
//! document is withheld (empty) without `EVALUATION_DEFINITION.AUTHOR`.
//!
//! A list whose scope the principal cannot see is not an empty list: the project-scoped
//! operations read the project's `capabilities` alongside the rows and report "unavailable"
//! (`None`) when the view capability is absent, and the definition- or run-scoped operations
//! report it when the parent row itself is invisible.
//!
//! The operations are grouped by what they read: `lists` for the two project-scoped lists,
//! `definitions` for one definition and its immutable versions, `runs` for a run and its facts,
//! and `commands` for the eight mutations. Held here is what they all send and select: the
//! generated filter and order inputs, and the query fragments each operation composes.

mod commands;
mod definitions;
mod lists;
mod runs;

pub use commands::*;
pub use definitions::*;
pub use lists::*;
pub use runs::*;

pub use super::enums::{EvaluationRunStatus, EvaluationTargetKind};
pub use super::page::Page;
use super::page::PaginationInfo;
use crate::api::generated::{
    OrderByEnum, PageInput, PaginationInput, ProjectsFilterInput, StringFilterInput,
    TextFilterInput,
};
use crate::graphql::{execute_within, schema, GraphqlError};
use cynic::QueryBuilder;

/// An evaluation request that has no answer after this long is reported as a failure.
const REQUEST_TIMEOUT_MILLIS: i32 = 10_000;

/// The rows one request asks for, and the page every "load more" button extends by.
pub const PAGE_SIZE: i32 = 50;

const DEFINITION_VIEW: &str = "EVALUATION_DEFINITION.VIEW";
const RUN_VIEW: &str = "EVALUATION_RUN.VIEW";

fn page(number: i32) -> PaginationInput {
    PaginationInput::Page(PageInput {
        limit: PAGE_SIZE,
        page: number,
    })
}

fn one() -> PaginationInput {
    PaginationInput::Page(PageInput { limit: 1, page: 0 })
}

// --- the generated filter and order inputs these operations send -------------------------------

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct EvaluationDefinitionsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<TextFilterInput>,
}

/// Newest first; the key is the tie-break, so definitions created in one instant keep one order.
#[derive(cynic::InputObject, Debug, Clone)]
pub struct EvaluationDefinitionsOrderInput {
    pub created_at: OrderByEnum,
    pub id: OrderByEnum,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct EvaluationDefinitionVersionsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub definition_id: Option<TextFilterInput>,
}

/// Newest version first, the key last.
#[derive(cynic::InputObject, Debug, Clone)]
pub struct EvaluationDefinitionVersionsOrderInput {
    pub version_number: OrderByEnum,
    pub id: OrderByEnum,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct EvaluationRunsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub definition_version_id: Option<TextFilterInput>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub lifecycle_status: Option<StringFilterInput>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct EvaluationRunsOrderInput {
    pub created_at: OrderByEnum,
    pub id: OrderByEnum,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct EvaluationCaseRunsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<TextFilterInput>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct EvaluationCaseRunsOrderInput {
    pub ordinal: OrderByEnum,
    pub id: OrderByEnum,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct EvaluationMetricResultsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<TextFilterInput>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct EvaluationMetricResultsOrderInput {
    pub metric_code: OrderByEnum,
    pub id: OrderByEnum,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct EvaluationArtifactMetadataFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<TextFilterInput>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct EvaluationArtifactMetadataOrderInput {
    pub artifact_kind: OrderByEnum,
    pub id: OrderByEnum,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct EvaluationAuditEventsFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<TextFilterInput>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct EvaluationAuditEventsOrderInput {
    pub occurred_at: OrderByEnum,
    pub id: OrderByEnum,
}

fn definitions_newest_first() -> EvaluationDefinitionsOrderInput {
    EvaluationDefinitionsOrderInput {
        created_at: OrderByEnum::Desc,
        id: OrderByEnum::Desc,
    }
}

fn versions_newest_first() -> EvaluationDefinitionVersionsOrderInput {
    EvaluationDefinitionVersionsOrderInput {
        version_number: OrderByEnum::Desc,
        id: OrderByEnum::Desc,
    }
}

fn runs_newest_first() -> EvaluationRunsOrderInput {
    EvaluationRunsOrderInput {
        created_at: OrderByEnum::Desc,
        id: OrderByEnum::Desc,
    }
}

fn scope(project_id: &str) -> ProjectsFilterInput {
    ProjectsFilterInput {
        id: Some(TextFilterInput::eq(project_id)),
        ..Default::default()
    }
}

/// The project's capability codes, or `None` when the project itself is not visible.
#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Projects")]
pub struct ProjectCapabilities {
    pub capabilities: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectsConnection")]
pub struct ProjectScopeConnection {
    pub nodes: Vec<ProjectCapabilities>,
}

impl ProjectScopeConnection {
    fn holds(&self, code: &str) -> bool {
        self.nodes
            .first()
            .is_some_and(|node| node.capabilities.iter().any(|held| held == code))
    }
}

// --- row fragments -----------------------------------------------------------------------------

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDefinitionVersions")]
pub struct EvaluationVersionFields {
    pub id: String,
    pub definition_id: String,
    pub version_number: i32,
    pub canonical_document: String,
    pub content_digest: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDraftDiagnostic")]
pub struct EvaluationDiagnostic {
    pub code: String,
    pub message: String,
    pub path: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDefinitionDrafts")]
pub struct EvaluationDefinitionDraft {
    pub canonical_document: String,
    pub revision: i32,
    pub validation_status: String,
    pub diagnostics: Vec<EvaluationDiagnostic>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDefinitions")]
pub struct EvaluationDefinitionFields {
    pub id: String,
    pub project_id: String,
    pub slug: String,
    pub draft: EvaluationDefinitionDraft,
    pub latest_version: Option<EvaluationVersionFields>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationTargetSnapshots")]
pub struct EvaluationTargetSnapshot {
    pub agent_content_digest: String,
    pub catalog_release_digest: String,
    pub environment_content_digest: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationRuns")]
pub struct EvaluationRunSummary {
    pub id: String,
    pub project_id: String,
    pub definition_version_id: String,
    pub target_kind: String,
    pub target_id: String,
    pub environment_definition_version_id: String,
    pub lifecycle_status: String,
    pub generation: i32,
    pub outcome_category: Option<String>,
    pub created_at: String,
    pub duration_millis: Option<i32>,
    pub failure_summary: Option<String>,
    pub target: Option<EvaluationTargetSnapshot>,
    pub deployment_evidence_disposition: String,
    /// The run state machine and the two run actions, decided by the server against the
    /// requesting principal's capabilities.
    pub terminal: bool,
    pub can_cancel: bool,
    pub can_rerun: bool,
}

impl EvaluationRunSummary {
    pub fn kind(&self) -> Option<EvaluationTargetKind> {
        EvaluationTargetKind::from_wire(&self.target_kind)
    }
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationCaseRuns")]
pub struct EvaluationCaseRun {
    pub id: String,
    pub case_key: String,
    pub lifecycle_status: String,
    pub passed: Option<bool>,
    pub failure_code: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationMetricResults")]
pub struct EvaluationMetricResult {
    pub id: String,
    pub metric_code: String,
    /// A `numeric` column: the generated API answers its exact digits as text.
    pub value: String,
    pub threshold: String,
    pub passed: bool,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationArtifactMetadata")]
pub struct EvaluationArtifactMetadata {
    pub id: String,
    pub artifact_kind: String,
    pub content_digest: String,
    pub media_type: String,
    pub byte_length: i32,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationAuditEvents")]
pub struct EvaluationAuditEvent {
    pub id: String,
    pub action: String,
    pub occurred_at: String,
    pub summary: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationTargetProjections")]
pub struct EvaluationTarget {
    pub target_kind: String,
    pub target_id: String,
    pub agent_version_id: String,
    pub environment_definition_version_id: String,
    pub logical_environment_class: String,
    pub display_name: String,
}

impl EvaluationTarget {
    pub fn kind(&self) -> Option<EvaluationTargetKind> {
        EvaluationTargetKind::from_wire(&self.target_kind)
    }
}

// --- connections -------------------------------------------------------------------------------

macro_rules! connection {
    ($name:ident, $graphql:literal, $node:ty) => {
        #[derive(cynic::QueryFragment, Debug, Clone)]
        #[cynic(graphql_type = $graphql)]
        pub struct $name {
            pub nodes: Vec<$node>,
            pub pagination_info: Option<PaginationInfo>,
        }

        impl From<$name> for Page<$node> {
            fn from(connection: $name) -> Self {
                Page::new(connection.nodes, connection.pagination_info)
            }
        }
    };
}

connection!(
    EvaluationDefinitionsPage,
    "EvaluationDefinitionsConnection",
    EvaluationDefinitionFields
);
connection!(
    EvaluationVersionsPage,
    "EvaluationDefinitionVersionsConnection",
    EvaluationVersionFields
);
connection!(
    EvaluationRunsPage,
    "EvaluationRunsConnection",
    EvaluationRunSummary
);
connection!(
    EvaluationCasesPage,
    "EvaluationCaseRunsConnection",
    EvaluationCaseRun
);
connection!(
    EvaluationMetricsPage,
    "EvaluationMetricResultsConnection",
    EvaluationMetricResult
);
connection!(
    EvaluationArtifactsPage,
    "EvaluationArtifactMetadataConnection",
    EvaluationArtifactMetadata
);
connection!(
    EvaluationAuditPage,
    "EvaluationAuditEventsConnection",
    EvaluationAuditEvent
);

/// A run as the detail page holds it: the summary and four independently extended fact lists.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationRunDetail {
    pub summary: EvaluationRunSummary,
    pub cases: Page<EvaluationCaseRun>,
    pub metrics: Page<EvaluationMetricResult>,
    pub artifacts: Page<EvaluationArtifactMetadata>,
    pub audit: Page<EvaluationAuditEvent>,
}
