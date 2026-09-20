use crate::schema::connection::PageInfo;
use crate::schema::RequestPrincipal;
use async_graphql::{Context, InputObject, Object, SimpleObject};
use hive_application::organization::{
    AccessibleOrganizationCursor, AccessibleOrganizationQueryService,
    OrganizationOverviewQueryService, OrganizationProjectCursor,
    OrganizationProjectDirectoryQueryService,
    OrganizationProjectFilter as AppOrganizationProjectFilter,
};
use hive_persistence::organization::{
    PgAccessibleOrganizationRepository, PgOrganizationOverviewRepository,
    PgOrganizationProjectDirectoryRepository,
};
use uuid::Uuid;

/// Ports the `Organization` type. A full `#[Object]` rather than `SimpleObject`
/// because `projects` is a lazily-resolved nested connection
/// (`OrganizationProjectDirectoryResolver.resolveProjects`), not a plain data field.
pub struct Organization {
    id: Uuid,
    slug: String,
    display_name: String,
    lifecycle_status: String,
}

#[Object]
impl Organization {
    async fn id(&self) -> async_graphql::ID {
        async_graphql::ID(self.id.to_string())
    }

    async fn slug(&self) -> &str {
        &self.slug
    }

    async fn display_name(&self) -> &str {
        &self.display_name
    }

    async fn lifecycle_status(&self) -> &str {
        &self.lifecycle_status
    }

    /// Ports `OrganizationProjectDirectoryResolver.resolveProjects`.
    #[allow(clippy::too_many_arguments)]
    async fn projects(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 25)] first: Option<i32>,
        after: Option<String>,
        last: Option<i32>,
        before: Option<String>,
        filter: Option<OrganizationProjectFilter>,
    ) -> async_graphql::Result<crate::schema::project::ProjectConnection> {
        if after.is_some() && before.is_some() {
            return Err(async_graphql::Error::new(
                "Supply either forward (first/after) or backward (last/before) pagination arguments, not both.",
            ));
        }
        let principal = ctx.data::<RequestPrincipal>()?;
        let repository =
            PgOrganizationProjectDirectoryRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
        let service = OrganizationProjectDirectoryQueryService::new(repository);

        let resolved_filter = match filter {
            Some(filter) => {
                AppOrganizationProjectFilter::from(filter.lifecycle_status, filter.search)
                    .map_err(|error| async_graphql::Error::new(error.to_string()))?
            }
            None => AppOrganizationProjectFilter::none(),
        };

        let page = if let Some(before) = &before {
            service
                .find_projects_before(
                    principal.0,
                    self.id,
                    last.unwrap_or(25) as i64,
                    Some(before),
                    Some(resolved_filter.clone()),
                )
                .await
        } else {
            service
                .find_projects(
                    principal.0,
                    self.id,
                    first.unwrap_or(25) as i64,
                    after.as_deref(),
                    Some(resolved_filter.clone()),
                )
                .await
        }
        .map_err(|error| async_graphql::Error::new(error.to_string()))?;

        let edges: Vec<crate::schema::project::ProjectEdge> = page
            .projects
            .iter()
            .map(|project| crate::schema::project::ProjectEdge {
                cursor: OrganizationProjectCursor::encode(project, &resolved_filter),
                node: crate::schema::project::Project {
                    id: project.id,
                    slug: project.slug.clone(),
                    display_name: project.display_name.clone(),
                    lifecycle_status: project.lifecycle_status.clone(),
                },
            })
            .collect();

        Ok(crate::schema::project::ProjectConnection {
            edges,
            page_info: PageInfo {
                end_cursor: page.end_cursor,
                has_next_page: page.has_next_page,
                has_previous_page: page.has_previous_page,
                start_cursor: page.start_cursor,
            },
            total_count: page.total_count as i32,
        })
    }
}

#[derive(InputObject)]
pub struct OrganizationProjectFilter {
    pub lifecycle_status: Option<String>,
    pub search: Option<String>,
}

#[derive(SimpleObject)]
pub struct OrganizationEdge {
    pub cursor: String,
    pub node: Organization,
}

#[derive(SimpleObject)]
pub struct OrganizationConnection {
    pub edges: Vec<OrganizationEdge>,
    pub page_info: PageInfo,
    pub total_count: i32,
}

#[derive(InputObject, Default)]
pub struct AccessibleOrganizationsFilter {
    #[graphql(default)]
    pub include_archived: bool,
}

pub struct OrganizationQueries;

#[Object]
impl OrganizationQueries {
    /// Ports `AccessibleOrganizationResolver.resolve` + `AccessibleOrganizationQueryService.query`.
    async fn accessible_organizations(
        &self,
        ctx: &Context<'_>,
        after: Option<String>,
        filter: Option<AccessibleOrganizationsFilter>,
        #[graphql(default = 50)] first: Option<i32>,
    ) -> async_graphql::Result<OrganizationConnection> {
        let principal = ctx.data::<RequestPrincipal>()?;
        let repository =
            PgAccessibleOrganizationRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
        let service = AccessibleOrganizationQueryService::new(repository);
        let include_archived = filter.unwrap_or_default().include_archived;

        let page = service
            .query(
                principal.0,
                include_archived,
                first.unwrap_or(50) as i64,
                after.as_deref(),
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;

        let edges: Vec<OrganizationEdge> = page
            .organizations
            .into_iter()
            .map(|organization| {
                let cursor = AccessibleOrganizationCursor::encode(&organization);
                OrganizationEdge {
                    cursor,
                    node: Organization {
                        id: organization.id,
                        slug: organization.slug,
                        display_name: organization.display_name,
                        lifecycle_status: organization.lifecycle_status,
                    },
                }
            })
            .collect();
        let start_cursor = edges.first().map(|edge| edge.cursor.clone());

        Ok(OrganizationConnection {
            edges,
            page_info: PageInfo {
                end_cursor: page.end_cursor,
                has_next_page: page.has_next_page,
                has_previous_page: after.is_some(),
                start_cursor,
            },
            total_count: page.total_count as i32,
        })
    }

    /// Ports `OrganizationOverviewResolver.resolveOrganization`.
    async fn organization(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> async_graphql::Result<Option<Organization>> {
        let principal = ctx.data::<RequestPrincipal>()?;
        let repository = PgOrganizationOverviewRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
        let service = OrganizationOverviewQueryService::new(repository);
        let overview = service
            .find_organization(principal.0, id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(overview.map(|overview| Organization {
            id: overview.id,
            slug: overview.slug,
            display_name: overview.display_name,
            lifecycle_status: overview.lifecycle_status,
        }))
    }
}
