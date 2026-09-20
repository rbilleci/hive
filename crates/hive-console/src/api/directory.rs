//! Ports the organization and navigation operations of `core.graphql`.

use crate::graphql::{execute, schema, GraphqlError};
use cynic::QueryBuilder;

/// One `PageInfo` for every connection: serde generates a deserializer per fragment struct, so a
/// struct shared across queries is paid for once (`evidence/2026-09-19-leptos-size/analysis.md`).
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct PageInfo {
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub end_cursor: Option<String>,
    pub start_cursor: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Organization")]
pub struct OrganizationSummary {
    pub id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct OrganizationEdge {
    pub node: OrganizationSummary,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct OrganizationConnection {
    pub edges: Vec<OrganizationEdge>,
    pub page_info: PageInfo,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct AccessibleOrganizationsFilter {
    pub include_archived: Option<bool>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct AccessibleOrganizationsVariables {
    pub first: i32,
    pub after: Option<String>,
    pub filter: Option<AccessibleOrganizationsFilter>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AccessibleOrganizationsVariables")]
pub struct AccessibleOrganizations {
    #[arguments(first: $first, after: $after, filter: $filter)]
    pub accessible_organizations: OrganizationConnection,
}

pub async fn request_organizations(
    include_archived: bool,
    after: Option<String>,
) -> Result<OrganizationConnection, GraphqlError> {
    let variables = AccessibleOrganizationsVariables {
        first: 50,
        after,
        filter: Some(AccessibleOrganizationsFilter {
            include_archived: Some(include_archived),
        }),
    };
    Ok(execute(AccessibleOrganizations::build(variables))
        .await?
        .accessible_organizations)
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Project")]
pub struct ProjectSummary {
    pub id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ProjectEdge {
    pub node: ProjectSummary,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ProjectConnection {
    pub edges: Vec<ProjectEdge>,
    pub page_info: PageInfo,
    pub total_count: i32,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct OrganizationOverviewVariables {
    pub id: cynic::Id,
    pub first: i32,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "Organization",
    variables = "OrganizationOverviewVariables"
)]
pub struct OrganizationWithProjects {
    pub id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    #[arguments(first: $first, after: $after)]
    pub projects: ProjectConnection,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "OrganizationOverviewVariables")]
pub struct OrganizationOverview {
    #[arguments(id: $id)]
    pub organization: Option<OrganizationWithProjects>,
}

/// `Ok(None)` is the server's "unavailable".
pub async fn request_organization_overview(
    organization_id: &str,
    after: Option<String>,
) -> Result<Option<OrganizationWithProjects>, GraphqlError> {
    let variables = OrganizationOverviewVariables {
        id: organization_id.into(),
        first: 25,
        after,
    };
    Ok(execute(OrganizationOverview::build(variables))
        .await?
        .organization)
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct AgentDirectoryFilter {
    pub lifecycle_status: Option<String>,
    pub search: Option<String>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct NavigationAgentsVariables {
    pub id: cynic::Id,
    pub first: i32,
    pub filter: Option<AgentDirectoryFilter>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Project", variables = "NavigationAgentsVariables")]
pub struct NavigationProject {
    #[arguments(first: $first, filter: $filter)]
    pub agents: PagedAgentConnection,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "NavigationAgentsVariables")]
pub struct NavigationAgents {
    #[arguments(id: $id)]
    pub project: Option<NavigationProject>,
}

/// The first page of a project's active agents for the sidebar tree. `Ok(None)` is "unavailable".
pub async fn request_navigation_agents(
    project_id: &str,
) -> Result<Option<PagedAgentConnection>, GraphqlError> {
    let variables = NavigationAgentsVariables {
        id: project_id.into(),
        first: 25,
        filter: Some(AgentDirectoryFilter {
            lifecycle_status: Some("ACTIVE".to_string()),
            search: None,
        }),
    };
    Ok(execute(NavigationAgents::build(variables))
        .await?
        .project
        .map(|project| project.agents))
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct CostSummary {
    pub availability: String,
    pub period_start: Option<String>,
    pub period_end: Option<String>,
    pub currency: Option<String>,
    pub amount_cents: Option<i32>,
    pub data_as_of: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "ProjectDashboard")]
pub struct ProjectDashboardFields {
    pub id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub active_agents: i32,
    pub active_deployments: i32,
    pub failed_deployments: i32,
    pub pending_approvals: i32,
    pub unhealthy_resources: i32,
    pub current_period_cost: CostSummary,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ProjectDashboardVariables {
    pub id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ProjectDashboardVariables")]
pub struct ProjectDashboard {
    #[arguments(id: $id)]
    pub project_dashboard: Option<ProjectDashboardFields>,
}

/// `Ok(None)` is the server's "unavailable".
pub async fn request_dashboard(
    project_id: &str,
) -> Result<Option<ProjectDashboardFields>, GraphqlError> {
    Ok(execute(ProjectDashboard::build(ProjectDashboardVariables {
        id: project_id.into(),
    }))
    .await?
    .project_dashboard)
}

/// One page of a keyset directory, already flattened for `pages::directory::KeysetDirectory`.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryPage {
    pub owner_id: String,
    pub rows: Vec<DirectoryRow>,
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub end_cursor: Option<String>,
    pub start_cursor: Option<String>,
    pub total_count: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryRow {
    pub href: String,
    pub name: String,
    /// The cells between the name and the lifecycle badge.
    pub cells: Vec<String>,
    pub lifecycle_status: String,
}

/// What a directory request is for: the filters and the one page boundary the URL holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryRequest {
    pub owner_id: String,
    pub lifecycle: String,
    pub search: String,
    pub after: Option<String>,
    pub before: Option<String>,
    pub page_size: i32,
}

impl DirectoryRequest {
    /// Relay keyset arguments: `last`/`before` when the URL arrived via Previous, else `first`/`after`.
    fn window(&self) -> (Option<i32>, Option<String>, Option<i32>, Option<String>) {
        match &self.before {
            Some(before) => (None, None, Some(self.page_size), Some(before.clone())),
            None => (Some(self.page_size), self.after.clone(), None, None),
        }
    }

    fn filter_text(value: &str) -> Option<String> {
        (!value.is_empty()).then(|| value.to_string())
    }
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct OrganizationProjectFilter {
    pub lifecycle_status: Option<String>,
    pub search: Option<String>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct OrganizationProjectsVariables {
    pub id: cynic::Id,
    pub first: Option<i32>,
    pub after: Option<String>,
    pub last: Option<i32>,
    pub before: Option<String>,
    pub filter: Option<OrganizationProjectFilter>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "Organization",
    variables = "OrganizationProjectsVariables"
)]
pub struct OrganizationProjectList {
    pub id: cynic::Id,
    #[arguments(first: $first, after: $after, last: $last, before: $before, filter: $filter)]
    pub projects: ProjectConnection,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "OrganizationProjectsVariables")]
pub struct OrganizationProjects {
    #[arguments(id: $id)]
    pub organization: Option<OrganizationProjectList>,
}

pub async fn request_organization_projects(
    request: DirectoryRequest,
) -> Result<Option<DirectoryPage>, GraphqlError> {
    let (first, after, last, before) = request.window();
    let filter = OrganizationProjectFilter {
        lifecycle_status: DirectoryRequest::filter_text(&request.lifecycle),
        search: DirectoryRequest::filter_text(&request.search),
    };
    let variables = OrganizationProjectsVariables {
        id: request.owner_id.as_str().into(),
        first,
        after,
        last,
        before,
        filter: Some(filter),
    };
    Ok(execute(OrganizationProjects::build(variables))
        .await?
        .organization
        .map(|organization| DirectoryPage {
            owner_id: organization.id.inner().to_string(),
            rows: organization
                .projects
                .edges
                .into_iter()
                .map(|edge| DirectoryRow {
                    href: format!("/projects/{}", edge.node.id.inner()),
                    name: edge.node.display_name,
                    cells: vec![edge.node.slug],
                    lifecycle_status: edge.node.lifecycle_status,
                })
                .collect(),
            has_next_page: organization.projects.page_info.has_next_page,
            has_previous_page: organization.projects.page_info.has_previous_page,
            end_cursor: organization.projects.page_info.end_cursor,
            start_cursor: organization.projects.page_info.start_cursor,
            total_count: organization.projects.total_count,
        }))
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Agent")]
pub struct DirectoryAgent {
    pub id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub latest_published_version: Option<i32>,
    pub model: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "AgentEdge")]
pub struct DirectoryAgentEdge {
    pub node: DirectoryAgent,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "AgentConnection")]
pub struct PagedAgentConnection {
    pub edges: Vec<DirectoryAgentEdge>,
    pub page_info: PageInfo,
    pub total_count: i32,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ProjectAgentsVariables {
    pub id: cynic::Id,
    pub first: Option<i32>,
    pub after: Option<String>,
    pub last: Option<i32>,
    pub before: Option<String>,
    pub filter: Option<AgentDirectoryFilter>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Project", variables = "ProjectAgentsVariables")]
pub struct ProjectAgentList {
    pub id: cynic::Id,
    #[arguments(first: $first, after: $after, last: $last, before: $before, filter: $filter)]
    pub agents: PagedAgentConnection,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ProjectAgentsVariables")]
pub struct ProjectAgents {
    #[arguments(id: $id)]
    pub project: Option<ProjectAgentList>,
}

pub async fn request_project_agents(
    request: DirectoryRequest,
) -> Result<Option<DirectoryPage>, GraphqlError> {
    let (first, after, last, before) = request.window();
    let filter = AgentDirectoryFilter {
        lifecycle_status: DirectoryRequest::filter_text(&request.lifecycle),
        search: DirectoryRequest::filter_text(&request.search),
    };
    let variables = ProjectAgentsVariables {
        id: request.owner_id.as_str().into(),
        first,
        after,
        last,
        before,
        filter: Some(filter),
    };
    Ok(execute(ProjectAgents::build(variables))
        .await?
        .project
        .map(|project| {
            let base = format!("/projects/{}/agents", project.id.inner());
            let unpublished = || "Not published".to_string();
            DirectoryPage {
                owner_id: project.id.inner().to_string(),
                rows: project
                    .agents
                    .edges
                    .into_iter()
                    .map(|edge| DirectoryRow {
                        href: format!("{base}/{}", edge.node.id.inner()),
                        name: edge.node.display_name,
                        cells: vec![
                            edge.node.slug,
                            edge.node
                                .latest_published_version
                                .map_or_else(unpublished, |version| version.to_string()),
                            edge.node.model.unwrap_or_else(unpublished),
                        ],
                        lifecycle_status: edge.node.lifecycle_status,
                    })
                    .collect(),
                has_next_page: project.agents.page_info.has_next_page,
                has_previous_page: project.agents.page_info.has_previous_page,
                end_cursor: project.agents.page_info.end_cursor,
                start_cursor: project.agents.page_info.start_cursor,
                total_count: project.agents.total_count,
            }
        }))
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AgentDraftValidationSummary {
    pub status: String,
    pub error_count: i32,
    pub warning_count: i32,
    pub validated_at: Option<String>,
}
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AgentPublishedVersionSummary {
    pub status: String,
    pub version: Option<String>,
    pub published_at: Option<String>,
}
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AgentAliasTargetsSummary {
    pub total_count: i32,
    pub active_count: i32,
}
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AgentDeploymentSummary {
    pub status: String,
    pub observed_at: Option<String>,
}
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AgentEvaluationSummary {
    pub outcome: String,
    pub completed_at: Option<String>,
}
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AgentRuntimeHealthSummary {
    pub status: String,
    pub observed_at: Option<String>,
    pub freshness: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentOperationalView")]
pub struct AgentOperationalViewFields {
    pub id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub draft_validation: AgentDraftValidationSummary,
    pub latest_published_version: AgentPublishedVersionSummary,
    pub alias_targets: AgentAliasTargetsSummary,
    pub active_deployment: AgentDeploymentSummary,
    pub recent_evaluation: AgentEvaluationSummary,
    pub runtime_health: AgentRuntimeHealthSummary,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct AgentOperationalViewVariables {
    pub project_id: cynic::Id,
    pub agent_id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AgentOperationalViewVariables")]
pub struct AgentOperationalView {
    #[arguments(projectId: $project_id, agentId: $agent_id)]
    pub agent_operational_view: Option<AgentOperationalViewFields>,
}

pub async fn request_agent_overview(
    project_id: &str,
    agent_id: &str,
) -> Result<Option<AgentOperationalViewFields>, GraphqlError> {
    let variables = AgentOperationalViewVariables {
        project_id: project_id.into(),
        agent_id: agent_id.into(),
    };
    Ok(execute(AgentOperationalView::build(variables))
        .await?
        .agent_operational_view)
}
