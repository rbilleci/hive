//! One run and the four fact lists it carries — its cases, metrics, artifacts and audit events —
//! and the targets a published version may run against.

use super::*;

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
