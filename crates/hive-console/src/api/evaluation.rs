//! Evaluation definitions, their immutable versions and their runs, all read through the
//! Seaography-generated entity API. The server decides which rows a principal reads; a definition
//! document is withheld (empty) without `EVALUATION_DEFINITION.AUTHOR`.
//!
//! A list whose scope the principal cannot see is not an empty list: the project-scoped
//! operations read the project's `capabilities` alongside the rows and report "unavailable"
//! (`None`) when the view capability is absent, and the definition- or run-scoped operations
//! report it when the parent row itself is invisible.

pub use super::enums::{EvaluationRunStatus, EvaluationTargetKind};
pub use super::page::Page;
use super::page::PaginationInfo;
use crate::api::generated::{
    OrderByEnum, PageInput, PaginationInput, ProjectsFilterInput, StringFilterInput,
    TextFilterInput,
};
use crate::graphql::{execute_within, schema, GraphqlError};
use cynic::{MutationBuilder, QueryBuilder};

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

// --- the project-scoped lists ------------------------------------------------------------------

#[derive(cynic::QueryVariables, Debug)]
pub struct EvaluationDefinitionsVariables {
    pub scope: ProjectsFilterInput,
    pub filters: EvaluationDefinitionsFilterInput,
    pub order_by: EvaluationDefinitionsOrderInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "EvaluationDefinitionsVariables")]
pub struct EvaluationDefinitions {
    #[arguments(filters: $scope)]
    pub projects: ProjectScopeConnection,
    #[arguments(filters: $filters, orderBy: $order_by, pagination: $pagination)]
    pub evaluation_definitions: EvaluationDefinitionsPage,
}

pub async fn request_evaluation_definitions(
    project_id: &str,
) -> Result<Option<Page<EvaluationDefinitionFields>>, GraphqlError> {
    let data = execute_within(
        EvaluationDefinitions::build(EvaluationDefinitionsVariables {
            scope: scope(project_id),
            filters: EvaluationDefinitionsFilterInput {
                project_id: Some(TextFilterInput::eq(project_id)),
                ..Default::default()
            },
            order_by: definitions_newest_first(),
            pagination: page(0),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?;
    Ok(data
        .projects
        .holds(DEFINITION_VIEW)
        .then(|| data.evaluation_definitions.into()))
}

#[derive(cynic::QueryVariables, Debug)]
pub struct EvaluationRunsVariables {
    pub scope: ProjectsFilterInput,
    pub filters: EvaluationRunsFilterInput,
    pub order_by: EvaluationRunsOrderInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "EvaluationRunsVariables")]
pub struct EvaluationRuns {
    #[arguments(filters: $scope)]
    pub projects: ProjectScopeConnection,
    #[arguments(filters: $filters, orderBy: $order_by, pagination: $pagination)]
    pub evaluation_runs: EvaluationRunsPage,
}

pub async fn request_evaluation_runs(
    project_id: &str,
    status: Option<EvaluationRunStatus>,
) -> Result<Option<Page<EvaluationRunSummary>>, GraphqlError> {
    let data = execute_within(
        EvaluationRuns::build(EvaluationRunsVariables {
            scope: scope(project_id),
            filters: EvaluationRunsFilterInput {
                project_id: Some(TextFilterInput::eq(project_id)),
                lifecycle_status: status.map(|status| StringFilterInput::eq(status.as_str())),
                ..Default::default()
            },
            order_by: runs_newest_first(),
            pagination: page(0),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?;
    Ok(data
        .projects
        .holds(RUN_VIEW)
        .then(|| data.evaluation_runs.into()))
}

// --- one definition, one version ---------------------------------------------------------------

#[derive(cynic::QueryVariables, Debug)]
pub struct DefinitionVariables {
    pub filters: EvaluationDefinitionsFilterInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "DefinitionVariables")]
pub struct EvaluationDefinition {
    #[arguments(filters: $filters, pagination: $pagination)]
    pub evaluation_definitions: EvaluationDefinitionsPage,
}

pub async fn request_evaluation_definition(
    definition_id: &str,
) -> Result<Option<EvaluationDefinitionFields>, GraphqlError> {
    if !crate::api::generated::is_uuid(definition_id) {
        return Ok(None);
    }
    Ok(execute_within(
        EvaluationDefinition::build(DefinitionVariables {
            filters: EvaluationDefinitionsFilterInput {
                id: Some(TextFilterInput::eq(definition_id)),
                ..Default::default()
            },
            pagination: one(),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?
    .evaluation_definitions
    .nodes
    .into_iter()
    .next())
}

#[derive(cynic::QueryVariables, Debug)]
pub struct VersionVariables {
    pub filters: EvaluationDefinitionVersionsFilterInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "VersionVariables")]
pub struct EvaluationDefinitionVersion {
    #[arguments(filters: $filters, pagination: $pagination)]
    pub evaluation_definition_versions: EvaluationVersionsPage,
}

pub async fn request_evaluation_version(
    version_id: &str,
) -> Result<Option<EvaluationVersionFields>, GraphqlError> {
    if !crate::api::generated::is_uuid(version_id) {
        return Ok(None);
    }
    Ok(execute_within(
        EvaluationDefinitionVersion::build(VersionVariables {
            filters: EvaluationDefinitionVersionsFilterInput {
                id: Some(TextFilterInput::eq(version_id)),
                ..Default::default()
            },
            pagination: one(),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?
    .evaluation_definition_versions
    .nodes
    .into_iter()
    .next())
}

// --- the immutable history, its comparison and its usage ---------------------------------------

#[derive(cynic::QueryVariables, Debug)]
pub struct EvaluationDefinitionVersionsVariables {
    pub scope: EvaluationDefinitionsFilterInput,
    pub scope_pagination: PaginationInput,
    pub filters: EvaluationDefinitionVersionsFilterInput,
    pub order_by: EvaluationDefinitionVersionsOrderInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "EvaluationDefinitions")]
pub struct DefinitionExists {
    /// Only the row's presence is read: it says whether the definition is visible at all.
    #[allow(dead_code)]
    pub id: String,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "EvaluationDefinitionsConnection")]
pub struct DefinitionScopeConnection {
    pub nodes: Vec<DefinitionExists>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    graphql_type = "Query",
    variables = "EvaluationDefinitionVersionsVariables"
)]
pub struct EvaluationDefinitionVersions {
    #[arguments(filters: $scope, pagination: $scope_pagination)]
    pub evaluation_definitions: DefinitionScopeConnection,
    #[arguments(filters: $filters, orderBy: $order_by, pagination: $pagination)]
    pub evaluation_definition_versions: EvaluationVersionsPage,
}

pub async fn request_evaluation_version_history(
    definition_id: &str,
    page_number: i32,
) -> Result<Option<Page<EvaluationVersionFields>>, GraphqlError> {
    if !crate::api::generated::is_uuid(definition_id) {
        return Ok(None);
    }
    let data = execute_within(
        EvaluationDefinitionVersions::build(EvaluationDefinitionVersionsVariables {
            scope: EvaluationDefinitionsFilterInput {
                id: Some(TextFilterInput::eq(definition_id)),
                ..Default::default()
            },
            scope_pagination: one(),
            filters: EvaluationDefinitionVersionsFilterInput {
                definition_id: Some(TextFilterInput::eq(definition_id)),
                ..Default::default()
            },
            order_by: versions_newest_first(),
            pagination: page(page_number),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?;
    Ok((!data.evaluation_definitions.nodes.is_empty())
        .then(|| data.evaluation_definition_versions.into()))
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDefinitionVersionComparison")]
pub struct VersionComparisonFields {
    pub left: EvaluationVersionFields,
    pub right: EvaluationVersionFields,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ComparisonVariables {
    pub filters: EvaluationDefinitionVersionsFilterInput,
    pub pagination: PaginationInput,
    pub right_version_id: String,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    graphql_type = "EvaluationDefinitionVersions",
    variables = "ComparisonVariables"
)]
pub struct ComparedVersion {
    #[arguments(rightVersionId: $right_version_id)]
    pub comparison: Option<VersionComparisonFields>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    graphql_type = "EvaluationDefinitionVersionsConnection",
    variables = "ComparisonVariables"
)]
pub struct ComparedVersionConnection {
    pub nodes: Vec<ComparedVersion>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ComparisonVariables")]
pub struct EvaluationDefinitionVersionComparison {
    #[arguments(filters: $filters, pagination: $pagination)]
    pub evaluation_definition_versions: ComparedVersionConnection,
}

/// Both sides of a comparison, or `None` when either is not visible or they belong to different
/// definitions.
pub async fn request_evaluation_version_comparison(
    left: &str,
    right: &str,
) -> Result<Option<(EvaluationVersionFields, EvaluationVersionFields)>, GraphqlError> {
    if !crate::api::generated::is_uuid(left) || !crate::api::generated::is_uuid(right) {
        return Ok(None);
    }
    Ok(execute_within(
        EvaluationDefinitionVersionComparison::build(ComparisonVariables {
            filters: EvaluationDefinitionVersionsFilterInput {
                id: Some(TextFilterInput::eq(left)),
                ..Default::default()
            },
            pagination: one(),
            right_version_id: right.to_string(),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?
    .evaluation_definition_versions
    .nodes
    .into_iter()
    .next()
    .and_then(|node| node.comparison)
    .map(|comparison| (comparison.left, comparison.right)))
}

#[derive(cynic::QueryVariables, Debug)]
pub struct EvaluationDefinitionVersionUsageVariables {
    pub scope: EvaluationDefinitionVersionsFilterInput,
    pub scope_pagination: PaginationInput,
    pub filters: EvaluationRunsFilterInput,
    pub order_by: EvaluationRunsOrderInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "EvaluationDefinitionVersions")]
pub struct VersionExists {
    /// Only the row's presence is read; see `DefinitionExists`.
    #[allow(dead_code)]
    pub id: String,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "EvaluationDefinitionVersionsConnection")]
pub struct VersionScopeConnection {
    pub nodes: Vec<VersionExists>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    graphql_type = "Query",
    variables = "EvaluationDefinitionVersionUsageVariables"
)]
pub struct EvaluationDefinitionVersionUsage {
    #[arguments(filters: $scope, pagination: $scope_pagination)]
    pub evaluation_definition_versions: VersionScopeConnection,
    #[arguments(filters: $filters, orderBy: $order_by, pagination: $pagination)]
    pub evaluation_runs: EvaluationRunsPage,
}

pub async fn request_evaluation_version_usage(
    version_id: &str,
    page_number: i32,
) -> Result<Option<Page<EvaluationRunSummary>>, GraphqlError> {
    if !crate::api::generated::is_uuid(version_id) {
        return Ok(None);
    }
    let data = execute_within(
        EvaluationDefinitionVersionUsage::build(EvaluationDefinitionVersionUsageVariables {
            scope: EvaluationDefinitionVersionsFilterInput {
                id: Some(TextFilterInput::eq(version_id)),
                ..Default::default()
            },
            scope_pagination: one(),
            filters: EvaluationRunsFilterInput {
                definition_version_id: Some(TextFilterInput::eq(version_id)),
                ..Default::default()
            },
            order_by: runs_newest_first(),
            pagination: page(page_number),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?;
    Ok(
        (!data.evaluation_definition_versions.nodes.is_empty())
            .then(|| data.evaluation_runs.into()),
    )
}

// --- the run detail and its four fact lists ----------------------------------------------------

#[derive(cynic::QueryVariables, Debug)]
pub struct EvaluationRunVariables {
    pub run: EvaluationRunsFilterInput,
    pub run_pagination: PaginationInput,
    pub cases: EvaluationCaseRunsFilterInput,
    pub cases_order: EvaluationCaseRunsOrderInput,
    pub metrics: EvaluationMetricResultsFilterInput,
    pub metrics_order: EvaluationMetricResultsOrderInput,
    pub artifacts: EvaluationArtifactMetadataFilterInput,
    pub artifacts_order: EvaluationArtifactMetadataOrderInput,
    pub audit: EvaluationAuditEventsFilterInput,
    pub audit_order: EvaluationAuditEventsOrderInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "EvaluationRunVariables")]
pub struct EvaluationRun {
    #[arguments(filters: $run, pagination: $run_pagination)]
    pub evaluation_runs: EvaluationRunsPage,
    #[arguments(filters: $cases, orderBy: $cases_order, pagination: $pagination)]
    pub evaluation_case_runs: EvaluationCasesPage,
    #[arguments(filters: $metrics, orderBy: $metrics_order, pagination: $pagination)]
    pub evaluation_metric_results: EvaluationMetricsPage,
    #[arguments(filters: $artifacts, orderBy: $artifacts_order, pagination: $pagination)]
    pub evaluation_artifact_metadata: EvaluationArtifactsPage,
    #[arguments(filters: $audit, orderBy: $audit_order, pagination: $pagination)]
    pub evaluation_audit_events: EvaluationAuditPage,
}

fn run_variables(run_id: &str, page_number: i32) -> EvaluationRunVariables {
    let run = || Some(TextFilterInput::eq(run_id));
    EvaluationRunVariables {
        run: EvaluationRunsFilterInput {
            id: run(),
            ..Default::default()
        },
        run_pagination: one(),
        cases: EvaluationCaseRunsFilterInput { run_id: run() },
        cases_order: EvaluationCaseRunsOrderInput {
            ordinal: OrderByEnum::Asc,
            id: OrderByEnum::Asc,
        },
        metrics: EvaluationMetricResultsFilterInput { run_id: run() },
        metrics_order: EvaluationMetricResultsOrderInput {
            metric_code: OrderByEnum::Asc,
            id: OrderByEnum::Asc,
        },
        artifacts: EvaluationArtifactMetadataFilterInput { run_id: run() },
        artifacts_order: EvaluationArtifactMetadataOrderInput {
            artifact_kind: OrderByEnum::Asc,
            id: OrderByEnum::Asc,
        },
        audit: EvaluationAuditEventsFilterInput { run_id: run() },
        audit_order: EvaluationAuditEventsOrderInput {
            occurred_at: OrderByEnum::Desc,
            id: OrderByEnum::Desc,
        },
        pagination: page(page_number),
    }
}

pub async fn request_evaluation_run(
    run_id: &str,
) -> Result<Option<EvaluationRunDetail>, GraphqlError> {
    if !crate::api::generated::is_uuid(run_id) {
        return Ok(None);
    }
    let data = execute_within(
        EvaluationRun::build(run_variables(run_id, 0)),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?;
    Ok(data
        .evaluation_runs
        .nodes
        .into_iter()
        .next()
        .map(|summary| EvaluationRunDetail {
            summary,
            cases: data.evaluation_case_runs.into(),
            metrics: data.evaluation_metric_results.into(),
            artifacts: data.evaluation_artifact_metadata.into(),
            audit: data.evaluation_audit_events.into(),
        }))
}

/// The next page of one of a run's fact lists: `EvaluationRunCases` and its three siblings.
macro_rules! run_facts {
    ($d:tt, $root:ident, $variables:ident, $variables_name:literal, $field:ident, $filter:ty, $order:ty, $connection:ty, $node:ty, $call:ident, $order_value:expr) => {
        #[derive(cynic::QueryVariables, Debug)]
        pub struct $variables {
            pub filters: $filter,
            pub order_by: $order,
            pub pagination: PaginationInput,
        }

        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "Query", variables = $variables_name)]
        pub struct $root {
            #[arguments(filters: $d filters, orderBy: $d order_by, pagination: $d pagination)]
            pub $field: $connection,
        }

        pub async fn $call(
            run_id: &str,
            page_number: i32,
        ) -> Result<Option<Page<$node>>, GraphqlError> {
            if !crate::api::generated::is_uuid(run_id) {
                return Ok(None);
            }
            let data = execute_within(
                $root::build($variables {
                    filters: <$filter>::from(TextFilterInput::eq(run_id)),
                    order_by: $order_value,
                    pagination: page(page_number),
                }),
                REQUEST_TIMEOUT_MILLIS,
            )
            .await?;
            Ok(Some(data.$field.into()))
        }
    };
}

impl From<TextFilterInput> for EvaluationCaseRunsFilterInput {
    fn from(run_id: TextFilterInput) -> Self {
        Self {
            run_id: Some(run_id),
        }
    }
}

impl From<TextFilterInput> for EvaluationMetricResultsFilterInput {
    fn from(run_id: TextFilterInput) -> Self {
        Self {
            run_id: Some(run_id),
        }
    }
}

impl From<TextFilterInput> for EvaluationArtifactMetadataFilterInput {
    fn from(run_id: TextFilterInput) -> Self {
        Self {
            run_id: Some(run_id),
        }
    }
}

impl From<TextFilterInput> for EvaluationAuditEventsFilterInput {
    fn from(run_id: TextFilterInput) -> Self {
        Self {
            run_id: Some(run_id),
        }
    }
}

run_facts!($, EvaluationRunCases, RunCasesVariables, "RunCasesVariables", evaluation_case_runs, EvaluationCaseRunsFilterInput, EvaluationCaseRunsOrderInput, EvaluationCasesPage, EvaluationCaseRun, request_evaluation_run_cases, EvaluationCaseRunsOrderInput { ordinal: OrderByEnum::Asc, id: OrderByEnum::Asc });
run_facts!($, EvaluationRunMetrics, RunMetricsVariables, "RunMetricsVariables", evaluation_metric_results, EvaluationMetricResultsFilterInput, EvaluationMetricResultsOrderInput, EvaluationMetricsPage, EvaluationMetricResult, request_evaluation_run_metrics, EvaluationMetricResultsOrderInput { metric_code: OrderByEnum::Asc, id: OrderByEnum::Asc });
run_facts!($, EvaluationRunArtifacts, RunArtifactsVariables, "RunArtifactsVariables", evaluation_artifact_metadata, EvaluationArtifactMetadataFilterInput, EvaluationArtifactMetadataOrderInput, EvaluationArtifactsPage, EvaluationArtifactMetadata, request_evaluation_run_artifacts, EvaluationArtifactMetadataOrderInput { artifact_kind: OrderByEnum::Asc, id: OrderByEnum::Asc });
run_facts!($, EvaluationRunAudit, RunAuditVariables, "RunAuditVariables", evaluation_audit_events, EvaluationAuditEventsFilterInput, EvaluationAuditEventsOrderInput, EvaluationAuditPage, EvaluationAuditEvent, request_evaluation_run_audit, EvaluationAuditEventsOrderInput { occurred_at: OrderByEnum::Desc, id: OrderByEnum::Desc });

// --- the compatible targets of a published version ---------------------------------------------

#[derive(cynic::QueryVariables, Debug)]
pub struct TargetsVariables {
    pub scope: ProjectsFilterInput,
    pub definition_version_id: String,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Projects", variables = "TargetsVariables")]
pub struct ProjectTargets {
    #[arguments(definitionVersionId: $definition_version_id)]
    pub compatible_evaluation_targets: Vec<EvaluationTarget>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "ProjectsConnection", variables = "TargetsVariables")]
pub struct ProjectTargetsConnection {
    pub nodes: Vec<ProjectTargets>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "TargetsVariables")]
pub struct EvaluationTargets {
    #[arguments(filters: $scope)]
    pub projects: ProjectTargetsConnection,
}

/// Every target the published version may run against here, in one answer: the set is bounded by
/// the project's own agent versions and deployments, so it is not paged.
pub async fn request_evaluation_targets(
    project_id: &str,
    definition_version_id: &str,
) -> Result<Option<Vec<EvaluationTarget>>, GraphqlError> {
    if !crate::api::generated::is_uuid(definition_version_id) {
        return Ok(None);
    }
    Ok(execute_within(
        EvaluationTargets::build(TargetsVariables {
            scope: scope(project_id),
            definition_version_id: definition_version_id.to_string(),
        }),
        REQUEST_TIMEOUT_MILLIS,
    )
    .await?
    .projects
    .nodes
    .into_iter()
    .next()
    .map(|project| project.compatible_evaluation_targets))
}

// --- the commands ------------------------------------------------------------------------------

/// The one problem type every command payload lists its refusals with.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Problem")]
pub struct EvaluationProblemFields {
    pub code: String,
    pub message: String,
}

/// The generated `EvaluationDefinitions` row a command answers with, and its draft.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDefinitions")]
pub struct CommandDefinitionFields {
    pub id: String,
    pub project_id: String,
    pub draft: CommandDraftFields,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDefinitionDrafts")]
pub struct CommandDraftFields {
    pub revision: i32,
    pub validation_status: String,
    pub canonical_document: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationRuns")]
pub struct CommandRunFields {
    pub id: String,
    pub project_id: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EvaluationMutationPayload {
    pub definition: Option<CommandDefinitionFields>,
    pub run: Option<CommandRunFields>,
    pub problems: Vec<EvaluationProblemFields>,
}

impl EvaluationMutationPayload {
    /// The first refusal, if the command was refused.
    pub fn problem(&self) -> Option<String> {
        self.problems.first().map(|problem| problem.message.clone())
    }
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct CreateEvaluationDefinitionInput {
    pub project_id: cynic::Id,
    pub slug: String,
    pub document: Option<String>,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdateEvaluationDefinitionDraftInput {
    pub definition_id: cynic::Id,
    pub expected_revision: i64,
    pub document: String,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct ValidateEvaluationDefinitionDraftInput {
    pub definition_id: cynic::Id,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct PublishEvaluationDefinitionDraftInput {
    pub definition_id: cynic::Id,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct DuplicateEvaluationDefinitionVersionToDraftInput {
    pub version_id: cynic::Id,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct RunEvaluationInput {
    pub project_id: cynic::Id,
    pub definition_version_id: cynic::Id,
    pub target_kind: EvaluationTargetKind,
    pub target_id: cynic::Id,
    pub environment_definition_version_id: cynic::Id,
    pub idempotency_key: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct CancelEvaluationInput {
    pub run_id: cynic::Id,
    pub expected_generation: i64,
    pub idempotency_key: String,
    pub reason: Option<String>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct RerunEvaluationInput {
    pub run_id: cynic::Id,
    pub idempotency_key: String,
}

macro_rules! evaluation_mutation {
    ($d:tt, $root:ident, $variables:ident, $variables_name:literal, $input_type:ty, $field:ident) => {
        #[derive(cynic::QueryVariables, Debug)]
        pub struct $variables {
            pub input: $input_type,
        }

        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "Mutation", variables = $variables_name)]
        pub struct $root {
            #[arguments(input: $d input)]
            pub $field: EvaluationMutationPayload,
        }

        pub async fn $field(input: $input_type) -> Result<EvaluationMutationPayload, GraphqlError> {
            Ok(
                execute_within($root::build($variables { input }), REQUEST_TIMEOUT_MILLIS)
                    .await?
                    .$field,
            )
        }
    };
}

evaluation_mutation!($, CreateEvaluationDefinition, CreateEvaluationDefinitionVariables, "CreateEvaluationDefinitionVariables", CreateEvaluationDefinitionInput, create_evaluation_definition);
evaluation_mutation!($, UpdateEvaluationDefinitionDraft, UpdateEvaluationDefinitionDraftVariables, "UpdateEvaluationDefinitionDraftVariables", UpdateEvaluationDefinitionDraftInput, update_evaluation_definition_draft);
evaluation_mutation!($, ValidateEvaluationDefinitionDraft, ValidateEvaluationDefinitionDraftVariables, "ValidateEvaluationDefinitionDraftVariables", ValidateEvaluationDefinitionDraftInput, validate_evaluation_definition_draft);
evaluation_mutation!($, PublishEvaluationDefinitionDraft, PublishEvaluationDefinitionDraftVariables, "PublishEvaluationDefinitionDraftVariables", PublishEvaluationDefinitionDraftInput, publish_evaluation_definition_draft);
evaluation_mutation!($, DuplicateEvaluationDefinitionVersionToDraft, DuplicateEvaluationDefinitionVersionToDraftVariables, "DuplicateEvaluationDefinitionVersionToDraftVariables", DuplicateEvaluationDefinitionVersionToDraftInput, duplicate_evaluation_definition_version_to_draft);
evaluation_mutation!($, RunEvaluation, RunEvaluationVariables, "RunEvaluationVariables", RunEvaluationInput, run_evaluation);
evaluation_mutation!($, CancelEvaluation, CancelEvaluationVariables, "CancelEvaluationVariables", CancelEvaluationInput, cancel_evaluation);
evaluation_mutation!($, RerunEvaluation, RerunEvaluationVariables, "RerunEvaluationVariables", RerunEvaluationInput, rerun_evaluation);
