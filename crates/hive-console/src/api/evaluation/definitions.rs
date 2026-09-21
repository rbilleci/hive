//! One definition and its immutable versions: the definition with its draft, one version, the
//! version history, a comparison of two versions, and a version's usage. Each reports
//! "unavailable" when the parent row it is scoped to is itself invisible.

use super::*;

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
