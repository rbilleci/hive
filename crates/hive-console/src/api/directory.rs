//! The organization, project and agent directories, read through Seaography's generated
//! `organizations`, `projects` and `agents` fields: `filters`, `orderBy`, page `pagination` and the
//! relation fields between them. Also the dashboard and agent overview operations.

use crate::api::generated::{
    is_uuid, like_pattern, AgentOperationalViewProjectionFilterInput, AgentVersionsOrderInput,
    AgentsFilterInput, AgentsOrderInput, OrderByEnum, OrganizationsFilterInput,
    OrganizationsOrderInput, PageInput, PaginationInput, ProjectsFilterInput, ProjectsOrderInput,
    StringFilterInput, TextFilterInput,
};
use crate::api::page::{Page, PaginationInfo};
use crate::graphql::{execute, schema, GeneratedJson, GraphqlError};
use cynic::QueryBuilder;

fn page_of(limit: i32, page: i32) -> PaginationInput {
    PaginationInput::Page(PageInput { limit, page })
}

/// Seaography applies `orderBy` columns in the entity's declaration order, not the order written.
/// The generated entities declare their primary key last, so adding `id` to any ordering makes it
/// the final tie-break: rows that share a name keep one order across pages.
fn ascending<T: Default>(set: impl FnOnce(&mut T)) -> T {
    let mut order = T::default();
    set(&mut order);
    order
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Organizations")]
pub struct OrganizationSummary {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "OrganizationsConnection")]
pub struct OrganizationRows {
    pub nodes: Vec<OrganizationSummary>,
    pub pagination_info: Option<PaginationInfo>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct AccessibleOrganizationsVariables {
    pub filters: OrganizationsFilterInput,
    pub order_by: OrganizationsOrderInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AccessibleOrganizationsVariables")]
pub struct AccessibleOrganizations {
    #[arguments(filters: $filters, orderBy: $order_by, pagination: $pagination)]
    pub organizations: OrganizationRows,
}

fn organizations_by_name() -> OrganizationsOrderInput {
    ascending(|order: &mut OrganizationsOrderInput| {
        order.display_name = Some(OrderByEnum::Asc);
        order.id = Some(OrderByEnum::Asc);
    })
}

/// The organizations the principal can see, by name. The server scopes the rows.
pub async fn request_organizations(
    include_archived: bool,
    page: i32,
) -> Result<Page<OrganizationSummary>, GraphqlError> {
    let variables = AccessibleOrganizationsVariables {
        filters: OrganizationsFilterInput {
            lifecycle_status: (!include_archived).then(|| StringFilterInput::ne("ARCHIVED")),
            ..Default::default()
        },
        order_by: organizations_by_name(),
        pagination: page_of(50, page),
    };
    let rows = execute(AccessibleOrganizations::build(variables))
        .await?
        .organizations;
    Ok(Page::new(rows.nodes, rows.pagination_info))
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Projects")]
pub struct ProjectSummary {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectsConnection")]
pub struct ProjectRows {
    pub nodes: Vec<ProjectSummary>,
    pub pagination_info: Option<PaginationInfo>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct OrganizationProjectsVariables {
    pub organization: OrganizationsFilterInput,
    pub filters: ProjectsFilterInput,
    pub order_by: ProjectsOrderInput,
    pub pagination: PaginationInput,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "Organizations",
    variables = "OrganizationProjectsVariables"
)]
pub struct OrganizationWithProjects {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    #[arguments(filters: $filters, orderBy: $order_by, pagination: $pagination)]
    pub projects: ProjectRows,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "OrganizationsConnection",
    variables = "OrganizationProjectsVariables"
)]
pub struct OrganizationsWithProjects {
    pub nodes: Vec<OrganizationWithProjects>,
}

fn projects_by_name() -> ProjectsOrderInput {
    ascending(|order: &mut ProjectsOrderInput| {
        order.display_name = Some(OrderByEnum::Asc);
        order.id = Some(OrderByEnum::Asc);
    })
}

fn organization_projects(
    organization_id: &str,
    filters: ProjectsFilterInput,
    limit: i32,
    page: i32,
) -> OrganizationProjectsVariables {
    OrganizationProjectsVariables {
        organization: OrganizationsFilterInput {
            id: Some(TextFilterInput::eq(organization_id)),
            ..Default::default()
        },
        filters,
        order_by: projects_by_name(),
        pagination: page_of(limit, page),
    }
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "OrganizationProjectsVariables")]
pub struct OrganizationOverview {
    #[arguments(filters: $organization)]
    pub organizations: OrganizationsWithProjects,
}

/// One organization and a page of its projects. `Ok(None)` is "unavailable": the principal
/// cannot see it, so the scoped read returned no row.
pub async fn request_organization_overview(
    organization_id: &str,
    page: i32,
) -> Result<Option<(OrganizationSummary, Page<ProjectSummary>)>, GraphqlError> {
    let variables =
        organization_projects(organization_id, ProjectsFilterInput::default(), 25, page);
    Ok(execute(OrganizationOverview::build(variables))
        .await?
        .organizations
        .nodes
        .into_iter()
        .next()
        .map(|organization| {
            (
                OrganizationSummary {
                    id: organization.id,
                    slug: organization.slug,
                    display_name: organization.display_name,
                    lifecycle_status: organization.lifecycle_status,
                },
                Page::new(
                    organization.projects.nodes,
                    organization.projects.pagination_info,
                ),
            )
        }))
}

/// The newest published version of an agent: Seaography's `agentVersions` relation, one row,
/// newest first.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentVersions")]
pub struct LatestVersion {
    pub version_number: i32,
    pub canonical_document: GeneratedJson,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentVersionsConnection")]
pub struct LatestVersions {
    pub nodes: Vec<LatestVersion>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Agents", variables = "ProjectAgentsVariables")]
pub struct DirectoryAgent {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    #[arguments(orderBy: $newest_first, pagination: $latest_only)]
    pub agent_versions: LatestVersions,
}

impl DirectoryAgent {
    pub fn latest_published_version(&self) -> Option<i32> {
        self.agent_versions
            .nodes
            .first()
            .map(|version| version.version_number)
    }

    /// The model reference the newest published version names.
    pub fn model(&self) -> Option<String> {
        self.agent_versions
            .nodes
            .first()?
            .canonical_document
            .0
            .get("model")?
            .get("reference")?
            .as_str()
            .map(str::to_string)
    }
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "AgentsConnection",
    variables = "ProjectAgentsVariables"
)]
pub struct AgentRows {
    pub nodes: Vec<DirectoryAgent>,
    pub pagination_info: Option<PaginationInfo>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Projects", variables = "ProjectAgentsVariables")]
pub struct ProjectWithAgents {
    pub id: String,
    #[arguments(filters: $filters, orderBy: $order_by, pagination: $pagination)]
    pub agents: AgentRows,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(
    graphql_type = "ProjectsConnection",
    variables = "ProjectAgentsVariables"
)]
pub struct ProjectsWithAgents {
    pub nodes: Vec<ProjectWithAgents>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ProjectAgentsVariables {
    pub project: ProjectsFilterInput,
    pub filters: AgentsFilterInput,
    pub order_by: AgentsOrderInput,
    pub pagination: PaginationInput,
    pub newest_first: AgentVersionsOrderInput,
    pub latest_only: PaginationInput,
}

fn project_agents(
    project_id: &str,
    filters: AgentsFilterInput,
    limit: i32,
    page: i32,
) -> ProjectAgentsVariables {
    ProjectAgentsVariables {
        project: ProjectsFilterInput {
            id: Some(TextFilterInput::eq(project_id)),
            ..Default::default()
        },
        filters,
        order_by: ascending(|order: &mut AgentsOrderInput| {
            order.display_name = Some(OrderByEnum::Asc);
            order.id = Some(OrderByEnum::Asc);
        }),
        pagination: page_of(limit, page),
        newest_first: ascending(|order: &mut AgentVersionsOrderInput| {
            order.version_number = Some(OrderByEnum::Desc);
        }),
        latest_only: page_of(1, 0),
    }
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ProjectAgentsVariables")]
pub struct NavigationAgents {
    #[arguments(filters: $project)]
    pub projects: ProjectsWithAgents,
}

/// The first page of a project's active agents for the sidebar tree. `Ok(None)` is "unavailable".
pub async fn request_navigation_agents(
    project_id: &str,
) -> Result<Option<Page<DirectoryAgent>>, GraphqlError> {
    let filters = AgentsFilterInput {
        lifecycle_status: Some(StringFilterInput::eq("ACTIVE")),
        ..Default::default()
    };
    Ok(execute(NavigationAgents::build(project_agents(
        project_id, filters, 25, 0,
    )))
    .await?
    .projects
    .nodes
    .into_iter()
    .next()
    .map(|project| Page::new(project.agents.nodes, project.agents.pagination_info)))
}

/// A row of the `project_dashboard_projection` view, read through Seaography's generated field.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "ProjectDashboardProjection")]
pub struct ProjectDashboardFields {
    pub project_id: String,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub active_agents: i32,
    pub active_deployments: i32,
    pub failed_deployments: i32,
    pub pending_approvals: i32,
    pub unhealthy_resources: i32,
    pub cost_availability: String,
    pub current_period_cost_cents: Option<i32>,
    pub cost_period_start: Option<String>,
    pub cost_period_end: Option<String>,
    pub cost_currency: Option<String>,
    pub cost_data_as_of: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectDashboardProjectionConnection")]
pub struct ProjectDashboardRows {
    pub nodes: Vec<ProjectDashboardFields>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ProjectDashboardVariables {
    pub id: String,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ProjectDashboardVariables")]
pub struct ProjectDashboard {
    #[arguments(filters: { projectId: { eq: $id } })]
    pub project_dashboard_projection: ProjectDashboardRows,
}

/// `Ok(None)` is "unavailable": the principal's row scope holds no dashboard for this project.
pub async fn request_dashboard(
    project_id: &str,
) -> Result<Option<ProjectDashboardFields>, GraphqlError> {
    Ok(execute(ProjectDashboard::build(ProjectDashboardVariables {
        id: project_id.to_string(),
    }))
    .await?
    .project_dashboard_projection
    .nodes
    .into_iter()
    .next())
}

/// One page of a directory, already flattened for the shared directory component.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryPage {
    /// The organization or project whose rows these are, as the server resolved it.
    pub owner_id: String,
    pub page: Page<DirectoryRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryRow {
    pub href: String,
    pub name: String,
    /// The cells between the name and the lifecycle badge.
    pub cells: Vec<String>,
    pub lifecycle_status: String,
}

/// What a directory request is for: the filters and the page the URL holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryRequest {
    pub owner_id: String,
    pub lifecycle: String,
    pub search: String,
    pub page: i32,
    pub page_size: i32,
}

impl DirectoryRequest {
    fn lifecycle_filter(&self) -> Option<StringFilterInput> {
        (!self.lifecycle.is_empty()).then(|| StringFilterInput::eq(&self.lifecycle))
    }

    /// A case-insensitive literal match on the display name.
    fn search_filter(&self) -> Option<StringFilterInput> {
        (!self.search.is_empty()).then(|| StringFilterInput::ilike(&like_pattern(&self.search)))
    }
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "OrganizationProjectsVariables")]
pub struct OrganizationProjects {
    #[arguments(filters: $organization)]
    pub organizations: OrganizationsWithProjects,
}

pub async fn request_organization_projects(
    request: DirectoryRequest,
) -> Result<Option<DirectoryPage>, GraphqlError> {
    let filters = ProjectsFilterInput {
        lifecycle_status: request.lifecycle_filter(),
        display_name: request.search_filter(),
        ..Default::default()
    };
    let variables =
        organization_projects(&request.owner_id, filters, request.page_size, request.page);
    Ok(execute(OrganizationProjects::build(variables))
        .await?
        .organizations
        .nodes
        .into_iter()
        .next()
        .map(|organization| DirectoryPage {
            owner_id: organization.id,
            page: Page::new(
                organization
                    .projects
                    .nodes
                    .into_iter()
                    .map(|project| DirectoryRow {
                        href: format!("/projects/{}", project.id),
                        name: project.display_name,
                        cells: vec![project.slug],
                        lifecycle_status: project.lifecycle_status,
                    })
                    .collect(),
                organization.projects.pagination_info,
            ),
        }))
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ProjectAgentsVariables")]
pub struct ProjectAgents {
    #[arguments(filters: $project)]
    pub projects: ProjectsWithAgents,
}

pub async fn request_project_agents(
    request: DirectoryRequest,
) -> Result<Option<DirectoryPage>, GraphqlError> {
    let filters = AgentsFilterInput {
        lifecycle_status: request.lifecycle_filter(),
        display_name: request.search_filter(),
        ..Default::default()
    };
    let variables = project_agents(&request.owner_id, filters, request.page_size, request.page);
    Ok(execute(ProjectAgents::build(variables))
        .await?
        .projects
        .nodes
        .into_iter()
        .next()
        .map(|project| {
            let base = format!("/projects/{}/agents", project.id);
            let unpublished = || "Not published".to_string();
            DirectoryPage {
                owner_id: project.id,
                page: Page::new(
                    project
                        .agents
                        .nodes
                        .into_iter()
                        .map(|agent| DirectoryRow {
                            href: format!("{base}/{}", agent.id),
                            cells: vec![
                                agent.slug.clone(),
                                agent
                                    .latest_published_version()
                                    .map_or_else(unpublished, |version| version.to_string()),
                                agent.model().unwrap_or_else(unpublished),
                            ],
                            name: agent.display_name,
                            lifecycle_status: agent.lifecycle_status,
                        })
                        .collect(),
                    project.agents.pagination_info,
                ),
            }
        }))
}

/// One agent's row of the `agent_operational_view_projection` view. The view fills every status
/// and count; the columns are nullable only because a view's columns always are.
#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "AgentOperationalViewProjection")]
pub struct AgentOperationalViewFields {
    pub agent_id: String,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub draft_validation_status: Option<String>,
    pub draft_error_count: Option<i32>,
    pub draft_warning_count: Option<i32>,
    pub draft_validated_at: Option<String>,
    pub published_version_status: Option<String>,
    pub published_version: Option<String>,
    pub published_at: Option<String>,
    pub alias_target_count: Option<i32>,
    pub active_alias_target_count: Option<i32>,
    pub deployment_status: Option<String>,
    pub deployment_observed_at: Option<String>,
    pub evaluation_outcome: Option<String>,
    pub evaluation_completed_at: Option<String>,
    pub runtime_health: Option<String>,
    pub runtime_observed_at: Option<String>,
    pub runtime_freshness: Option<String>,
}

impl AgentOperationalViewFields {
    pub fn has_published_version(&self) -> bool {
        self.published_version.is_some()
    }
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "AgentOperationalViewProjectionConnection")]
pub struct AgentOperationalViewRows {
    pub nodes: Vec<AgentOperationalViewFields>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct AgentOperationalViewVariables {
    pub filters: AgentOperationalViewProjectionFilterInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AgentOperationalViewVariables")]
pub struct AgentOperationalView {
    #[arguments(filters: $filters)]
    pub agent_operational_view_projection: AgentOperationalViewRows,
}

/// The agent's operational summaries, or `None` when the route's project does not own the agent
/// or the principal cannot see it. The key filter selects at most one row.
pub async fn request_agent_overview(
    project_id: &str,
    agent_id: &str,
) -> Result<Option<AgentOperationalViewFields>, GraphqlError> {
    if !is_uuid(project_id) || !is_uuid(agent_id) {
        return Ok(None);
    }
    let variables = AgentOperationalViewVariables {
        filters: AgentOperationalViewProjectionFilterInput {
            project_id: Some(TextFilterInput::eq(project_id)),
            agent_id: Some(TextFilterInput::eq(agent_id)),
        },
    };
    Ok(execute(AgentOperationalView::build(variables))
        .await?
        .agent_operational_view_projection
        .nodes
        .into_iter()
        .next())
}
