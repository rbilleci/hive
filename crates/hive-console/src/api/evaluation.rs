//! Ports `evaluation.graphql` and `evaluationApi.ts`: definitions, immutable versions, and runs.
//! Every reader returns `Ok(None)` for "unavailable": the server found nothing this principal may see.

pub use super::enums::{EvaluationOutcomeCategory, EvaluationRunStatus, EvaluationTargetKind};
use crate::graphql::{execute_within, schema, GraphqlError};
use cynic::{MutationBuilder, QueryBuilder};

/// An evaluation request that has no answer after this long is reported as a failure.
const REQUEST_TIMEOUT_MILLIS: i32 = 10_000;

/// A service that predates evaluations rejects these documents at validation; a reader reports
/// that as "unavailable" (`None`) rather than as a failed request.
fn supported<T>(result: Result<T, GraphqlError>) -> Result<Option<T>, GraphqlError> {
    match result {
        Err(GraphqlError::Transport(message))
            if ["Cannot query field", "FieldUndefined", "is undefined"]
                .iter()
                .any(|marker| message.contains(marker))
                && [
                    "evaluationDefinition",
                    "evaluationRun",
                    "EvaluationDefinition",
                    "EvaluationRun",
                ]
                .iter()
                .any(|name| message.contains(name)) =>
        {
            Ok(None)
        }
        other => other.map(Some),
    }
}

/// One bounded page of a connection, as the pages consume it.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub rows: Vec<T>,
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Appends the next page and adopts its cursor.
    pub fn extend(&mut self, next: Page<T>) {
        self.rows.extend(next.rows);
        self.has_next_page = next.has_next_page;
        self.end_cursor = next.end_cursor;
    }
}

/// The evaluation connections share one shape: `edges { node }`, `hasNextPage`, `endCursor`.
macro_rules! connection {
    ($connection:ident, $edge:ident, $node:ty) => {
        #[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
        pub struct $edge {
            pub node: $node,
        }

        #[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
        pub struct $connection {
            pub edges: Vec<$edge>,
            pub has_next_page: bool,
            pub end_cursor: Option<String>,
        }

        impl From<$connection> for Page<$node> {
            fn from(connection: $connection) -> Self {
                Page {
                    rows: connection.edges.into_iter().map(|edge| edge.node).collect(),
                    has_next_page: connection.has_next_page,
                    end_cursor: connection.end_cursor,
                }
            }
        }
    };
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDefinitionVersion")]
pub struct EvaluationVersionFields {
    pub id: cynic::Id,
    pub definition_id: cynic::Id,
    pub number: i64,
    pub canonical_document: String,
    pub content_digest: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EvaluationDiagnostic {
    pub code: String,
    pub message: String,
    pub path: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EvaluationDefinitionDraft {
    pub canonical_document: String,
    pub revision: i64,
    pub validation_status: String,
    pub diagnostics: Vec<EvaluationDiagnostic>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDefinition")]
pub struct EvaluationDefinitionFields {
    pub id: cynic::Id,
    pub project_id: cynic::Id,
    pub slug: String,
    pub draft: EvaluationDefinitionDraft,
    pub latest_version: Option<EvaluationVersionFields>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EvaluationTargetSnapshot {
    pub agent_content_digest: String,
    pub catalog_release_digest: String,
    pub environment_content_digest: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationRun")]
pub struct EvaluationRunSummary {
    pub id: cynic::Id,
    pub project_id: cynic::Id,
    pub definition_version_id: cynic::Id,
    pub target_kind: EvaluationTargetKind,
    pub target_id: cynic::Id,
    pub environment_definition_version_id: cynic::Id,
    pub lifecycle_status: EvaluationRunStatus,
    pub generation: i64,
    pub outcome_category: Option<EvaluationOutcomeCategory>,
    pub created_at: String,
    pub duration_millis: Option<i64>,
    pub failure_summary: Option<String>,
    pub target: Option<EvaluationTargetSnapshot>,
    pub deployment_evidence_disposition: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EvaluationCaseRun {
    pub id: cynic::Id,
    pub key: String,
    pub lifecycle_status: String,
    pub passed: Option<bool>,
    pub failure_code: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EvaluationMetricResult {
    pub id: cynic::Id,
    pub code: String,
    pub value: f64,
    pub threshold: f64,
    pub passed: bool,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EvaluationArtifactMetadata {
    pub id: cynic::Id,
    pub kind: String,
    pub content_digest: String,
    pub media_type: String,
    pub byte_length: i64,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EvaluationAuditEvent {
    pub id: cynic::Id,
    pub action: String,
    pub occurred_at: String,
    pub summary: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EvaluationTarget {
    pub kind: EvaluationTargetKind,
    pub id: cynic::Id,
    pub agent_version_id: cynic::Id,
    pub environment_definition_version_id: cynic::Id,
    pub logical_environment_class: String,
    pub display_name: String,
}

connection!(
    EvaluationDefinitionConnection,
    EvaluationDefinitionEdge,
    EvaluationDefinitionFields
);
connection!(
    EvaluationDefinitionVersionConnection,
    EvaluationDefinitionVersionEdge,
    EvaluationVersionFields
);
connection!(
    EvaluationRunConnection,
    EvaluationRunEdge,
    EvaluationRunSummary
);
connection!(
    EvaluationTargetConnection,
    EvaluationTargetEdge,
    EvaluationTarget
);
connection!(
    EvaluationCaseRunConnection,
    EvaluationCaseRunEdge,
    EvaluationCaseRun
);
connection!(
    EvaluationMetricResultConnection,
    EvaluationMetricResultEdge,
    EvaluationMetricResult
);
connection!(
    EvaluationArtifactMetadataConnection,
    EvaluationArtifactMetadataEdge,
    EvaluationArtifactMetadata
);
connection!(
    EvaluationAuditEventConnection,
    EvaluationAuditEventEdge,
    EvaluationAuditEvent
);

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationRun")]
pub struct EvaluationRunFields {
    #[cynic(spread)]
    pub summary: EvaluationRunSummary,
    #[arguments(first: 50)]
    pub cases: EvaluationCaseRunConnection,
    #[arguments(first: 50)]
    pub metrics: EvaluationMetricResultConnection,
    #[arguments(first: 50)]
    pub artifacts: EvaluationArtifactMetadataConnection,
    #[arguments(first: 50)]
    pub audit: EvaluationAuditEventConnection,
}

/// A run as the detail page holds it: the summary and four independently extended fact lists.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationRunDetail {
    pub summary: EvaluationRunSummary,
    pub cases: Page<EvaluationCaseRun>,
    pub metrics: Page<EvaluationMetricResult>,
    pub artifacts: Page<EvaluationArtifactMetadata>,
    pub audit: Page<EvaluationAuditEvent>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ProjectPageVariables {
    pub project_id: cynic::Id,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ProjectPageVariables")]
pub struct EvaluationDefinitions {
    #[arguments(projectId: $project_id, first: 50, after: $after)]
    pub evaluation_definitions: Option<EvaluationDefinitionConnection>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ProjectPageVariables")]
pub struct EvaluationRuns {
    #[arguments(projectId: $project_id, first: 50, after: $after)]
    pub evaluation_runs: Option<EvaluationRunConnection>,
}

pub async fn request_evaluation_definitions(
    project_id: &str,
) -> Result<Option<Page<EvaluationDefinitionFields>>, GraphqlError> {
    let variables = ProjectPageVariables {
        project_id: project_id.into(),
        after: None,
    };
    Ok(supported(
        execute_within(
            EvaluationDefinitions::build(variables),
            REQUEST_TIMEOUT_MILLIS,
        )
        .await,
    )?
    .and_then(|data| data.evaluation_definitions)
    .map(Into::into))
}

pub async fn request_evaluation_runs(
    project_id: &str,
) -> Result<Option<Page<EvaluationRunSummary>>, GraphqlError> {
    let variables = ProjectPageVariables {
        project_id: project_id.into(),
        after: None,
    };
    Ok(
        supported(execute_within(EvaluationRuns::build(variables), REQUEST_TIMEOUT_MILLIS).await)?
            .and_then(|data| data.evaluation_runs)
            .map(Into::into),
    )
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DefinitionVariables {
    pub definition_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "DefinitionVariables")]
pub struct EvaluationDefinition {
    #[arguments(definitionId: $definition_id)]
    pub evaluation_definition: Option<EvaluationDefinitionFields>,
}

pub async fn request_evaluation_definition(
    definition_id: &str,
) -> Result<Option<EvaluationDefinitionFields>, GraphqlError> {
    let variables = DefinitionVariables {
        definition_id: definition_id.into(),
    };
    Ok(supported(
        execute_within(
            EvaluationDefinition::build(variables),
            REQUEST_TIMEOUT_MILLIS,
        )
        .await,
    )?
    .and_then(|data| data.evaluation_definition))
}

#[derive(cynic::QueryVariables, Debug)]
pub struct VersionVariables {
    pub version_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "VersionVariables")]
pub struct EvaluationDefinitionVersion {
    #[arguments(versionId: $version_id)]
    pub evaluation_definition_version: Option<EvaluationVersionFields>,
}

pub async fn request_evaluation_version(
    version_id: &str,
) -> Result<Option<EvaluationVersionFields>, GraphqlError> {
    let variables = VersionVariables {
        version_id: version_id.into(),
    };
    Ok(supported(
        execute_within(
            EvaluationDefinitionVersion::build(variables),
            REQUEST_TIMEOUT_MILLIS,
        )
        .await,
    )?
    .and_then(|data| data.evaluation_definition_version))
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DefinitionPageVariables {
    pub definition_id: cynic::Id,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "DefinitionPageVariables")]
pub struct EvaluationDefinitionVersions {
    #[arguments(definitionId: $definition_id, first: 50, after: $after)]
    pub evaluation_definition_versions: Option<EvaluationDefinitionVersionConnection>,
}

pub async fn request_evaluation_version_history(
    definition_id: &str,
    after: Option<String>,
) -> Result<Option<Page<EvaluationVersionFields>>, GraphqlError> {
    let variables = DefinitionPageVariables {
        definition_id: definition_id.into(),
        after,
    };
    Ok(supported(
        execute_within(
            EvaluationDefinitionVersions::build(variables),
            REQUEST_TIMEOUT_MILLIS,
        )
        .await,
    )?
    .and_then(|data| data.evaluation_definition_versions)
    .map(Into::into))
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationDefinitionVersionComparison")]
pub struct VersionComparisonFields {
    pub left: Option<EvaluationVersionFields>,
    pub right: Option<EvaluationVersionFields>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ComparisonVariables {
    pub left_version_id: cynic::Id,
    pub right_version_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ComparisonVariables")]
pub struct EvaluationDefinitionVersionComparison {
    #[arguments(leftVersionId: $left_version_id, rightVersionId: $right_version_id)]
    pub evaluation_definition_version_comparison: Option<VersionComparisonFields>,
}

/// Both sides of a comparison, or `None` when either is not visible.
pub async fn request_evaluation_version_comparison(
    left: &str,
    right: &str,
) -> Result<Option<(EvaluationVersionFields, EvaluationVersionFields)>, GraphqlError> {
    let variables = ComparisonVariables {
        left_version_id: left.into(),
        right_version_id: right.into(),
    };
    Ok(supported(
        execute_within(
            EvaluationDefinitionVersionComparison::build(variables),
            REQUEST_TIMEOUT_MILLIS,
        )
        .await,
    )?
    .and_then(|data| data.evaluation_definition_version_comparison)
    .and_then(|comparison| comparison.left.zip(comparison.right)))
}

#[derive(cynic::QueryVariables, Debug)]
pub struct VersionPageVariables {
    pub version_id: cynic::Id,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "VersionPageVariables")]
pub struct EvaluationDefinitionVersionUsage {
    #[arguments(versionId: $version_id, first: 50, after: $after)]
    pub evaluation_definition_version_usage: Option<EvaluationRunConnection>,
}

pub async fn request_evaluation_version_usage(
    version_id: &str,
    after: Option<String>,
) -> Result<Option<Page<EvaluationRunSummary>>, GraphqlError> {
    let variables = VersionPageVariables {
        version_id: version_id.into(),
        after,
    };
    Ok(supported(
        execute_within(
            EvaluationDefinitionVersionUsage::build(variables),
            REQUEST_TIMEOUT_MILLIS,
        )
        .await,
    )?
    .and_then(|data| data.evaluation_definition_version_usage)
    .map(Into::into))
}

#[derive(cynic::QueryVariables, Debug)]
pub struct RunVariables {
    pub run_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "RunVariables")]
pub struct EvaluationRun {
    #[arguments(runId: $run_id)]
    pub evaluation_run: Option<EvaluationRunFields>,
}

pub async fn request_evaluation_run(
    run_id: &str,
) -> Result<Option<EvaluationRunDetail>, GraphqlError> {
    let variables = RunVariables {
        run_id: run_id.into(),
    };
    Ok(
        supported(execute_within(EvaluationRun::build(variables), REQUEST_TIMEOUT_MILLIS).await)?
            .and_then(|data| data.evaluation_run)
            .map(|run| EvaluationRunDetail {
                summary: run.summary,
                cases: run.cases.into(),
                metrics: run.metrics.into(),
                artifacts: run.artifacts.into(),
                audit: run.audit.into(),
            }),
    )
}

#[derive(cynic::QueryVariables, Debug)]
pub struct TargetsVariables {
    pub project_id: cynic::Id,
    pub definition_version_id: cynic::Id,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "TargetsVariables")]
pub struct EvaluationTargets {
    #[arguments(projectId: $project_id, definitionVersionId: $definition_version_id, first: 50, after: $after)]
    pub evaluation_targets: Option<EvaluationTargetConnection>,
}

pub async fn request_evaluation_targets(
    project_id: &str,
    definition_version_id: &str,
    after: Option<String>,
) -> Result<Option<Page<EvaluationTarget>>, GraphqlError> {
    let variables = TargetsVariables {
        project_id: project_id.into(),
        definition_version_id: definition_version_id.into(),
        after,
    };
    Ok(supported(
        execute_within(EvaluationTargets::build(variables), REQUEST_TIMEOUT_MILLIS).await,
    )?
    .and_then(|data| data.evaluation_targets)
    .map(Into::into))
}

#[derive(cynic::QueryVariables, Debug)]
pub struct RunPageVariables {
    pub run_id: cynic::Id,
    pub after: Option<String>,
}

/// The next page of one of a run's fact lists: `EvaluationRunCases` and its three siblings.
macro_rules! run_facts {
    ($d:tt, $root:ident, $slice:ident, $field:ident, $connection:ty, $node:ty, $call:ident) => {
        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "EvaluationRun", variables = "RunPageVariables")]
        pub struct $slice {
            #[arguments(first: 50, after: $d after)]
            pub $field: $connection,
        }

        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "Query", variables = "RunPageVariables")]
        pub struct $root {
            #[arguments(runId: $d run_id)]
            pub evaluation_run: Option<$slice>,
        }

        pub async fn $call(
            run_id: &str,
            after: String,
        ) -> Result<Option<Page<$node>>, GraphqlError> {
            let variables = RunPageVariables {
                run_id: run_id.into(),
                after: Some(after),
            };
            Ok(
                supported(execute_within($root::build(variables), REQUEST_TIMEOUT_MILLIS).await)?
                    .and_then(|data| data.evaluation_run)
                    .map(|run| run.$field.into()),
            )
        }
    };
}

run_facts!($, EvaluationRunCases, RunCasesSlice, cases, EvaluationCaseRunConnection, EvaluationCaseRun, request_evaluation_run_cases);
run_facts!($, EvaluationRunMetrics, RunMetricsSlice, metrics, EvaluationMetricResultConnection, EvaluationMetricResult, request_evaluation_run_metrics);
run_facts!($, EvaluationRunArtifacts, RunArtifactsSlice, artifacts, EvaluationArtifactMetadataConnection, EvaluationArtifactMetadata, request_evaluation_run_artifacts);
run_facts!($, EvaluationRunAudit, RunAuditSlice, audit, EvaluationAuditEventConnection, EvaluationAuditEvent, request_evaluation_run_audit);

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "EvaluationProblem")]
pub struct EvaluationProblemFields {
    pub message: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct EvaluationMutationPayload {
    pub definition: Option<EvaluationDefinitionFields>,
    pub run: Option<EvaluationRunSummary>,
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
