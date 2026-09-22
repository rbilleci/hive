//! The two project-scoped lists: a project's evaluation definitions and its runs. Both report
//! "unavailable" rather than an empty list when the project's own view capability is absent.

use super::*;

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
