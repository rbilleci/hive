//! Ports `schema/organization.rs`. `Organization` is complex (`GSR-NESTED-FIELDS`): `projects` is
//! a lazily-resolved nested connection, hand-built and folded onto the derived object rather than
//! a plain data field `#[derive(CustomOutputType)]` could express.

use crate::schema::project::{Project, ProjectConnection, ProjectEdge};
use crate::schema::scalars;
use crate::schema::scalars::Id;
use crate::schema::RequestPrincipal;
use async_graphql::dynamic::{Field, FieldFuture, InputObject, InputValue, TypeRef};
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
use seaography::{
    BuilderContext, CustomFields, CustomInputType, CustomOutputObject, CustomOutputType,
};
use uuid::Uuid;

#[allow(non_snake_case)]
mod wire {
    use super::*;

    #[derive(CustomOutputType, Clone)]
    pub struct Organization {
        pub id: Id,
        pub slug: String,
        pub displayName: String,
        pub lifecycleStatus: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct OrganizationEdge {
        pub cursor: String,
        pub node: Organization,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct OrganizationConnection {
        pub edges: Vec<OrganizationEdge>,
        pub pageInfo: seaography::PageInfo,
        pub totalCount: i32,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "OrganizationProjectFilter")]
    pub struct OrganizationProjectFilter {
        pub lifecycleStatus: Option<String>,
        pub search: Option<String>,
    }

    pub struct OrganizationQueries;

    #[CustomFields]
    impl OrganizationQueries {
        // Ports `OrganizationOverviewResolver.resolveOrganization`.
        async fn organization(
            ctx: &async_graphql::Context<'_>,
            id: Id,
        ) -> async_graphql::Result<Option<Organization>> {
            let principal = ctx.data::<RequestPrincipal>()?;
            let repository = PgOrganizationOverviewRepository::new(
                ctx.data::<sea_orm::DatabaseConnection>()?.clone(),
            );
            let service = OrganizationOverviewQueryService::new(repository);
            let overview = service
                .find_organization(principal.0, &id.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(overview.map(|overview| Organization {
                id: overview.id.to_string().into(),
                slug: overview.slug,
                displayName: overview.display_name,
                lifecycleStatus: overview.lifecycle_status,
            }))
        }
    }
}

pub use wire::{
    Organization, OrganizationConnection, OrganizationEdge, OrganizationProjectFilter,
    OrganizationQueries,
};

/// `AccessibleOrganizationsFilter`'s one field carries a default (`GSR-DEFAULTS`), which
/// `#[derive(CustomInputType)]` cannot express; hand-built like the root field that uses it.
fn accessible_organizations_filter() -> InputObject {
    InputObject::new("AccessibleOrganizationsFilter").field(
        InputValue::new("includeArchived", TypeRef::named(TypeRef::BOOLEAN)).default_value(false),
    )
}

/// `accessibleOrganizations(after, filter, first: Int = 50)` (`GSR-DEFAULTS`). Ports
/// `AccessibleOrganizationResolver.resolve` + `AccessibleOrganizationQueryService.query`.
fn accessible_organizations_field() -> Field {
    Field::new(
        "accessibleOrganizations",
        TypeRef::named_nn("OrganizationConnection"),
        |ctx| {
            FieldFuture::new(async move {
                let principal = ctx.ctx.data::<RequestPrincipal>()?;
                let after = scalars::optional_string(ctx.args.get("after"))?;
                let include_archived = match scalars::defined(ctx.args.get("filter")) {
                    Some(filter) => {
                        scalars::optional_boolean(filter.object()?.get("includeArchived"))?
                            .unwrap_or(false)
                    }
                    None => false,
                };
                let first = ctx.args.try_get("first")?.i64()?;

                let repository = PgAccessibleOrganizationRepository::new(
                    ctx.ctx.data::<sea_orm::DatabaseConnection>()?.clone(),
                );
                let service = AccessibleOrganizationQueryService::new(repository);
                let page = service
                    .query(principal.0, include_archived, first, after.as_deref())
                    .await
                    .map_err(|error| async_graphql::Error::new(error.to_string()))?;

                let edges: Vec<wire::OrganizationEdge> = page
                    .organizations
                    .into_iter()
                    .map(|organization| {
                        let cursor = AccessibleOrganizationCursor::encode(&organization);
                        wire::OrganizationEdge {
                            cursor,
                            node: wire::Organization {
                                id: organization.id.to_string().into(),
                                slug: organization.slug,
                                displayName: organization.display_name,
                                lifecycleStatus: organization.lifecycle_status,
                            },
                        }
                    })
                    .collect();
                let start_cursor = edges.first().map(|edge| edge.cursor.clone());

                let connection = wire::OrganizationConnection {
                    edges,
                    pageInfo: seaography::PageInfo {
                        has_previous_page: after.is_some(),
                        has_next_page: page.has_next_page,
                        start_cursor,
                        end_cursor: page.end_cursor,
                    },
                    totalCount: page.total_count as i32,
                };
                Ok(connection.gql_field_value(context()))
            })
        },
    )
    .argument(InputValue::new("after", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new(
        "filter",
        TypeRef::named("AccessibleOrganizationsFilter"),
    ))
    .argument(InputValue::new("first", TypeRef::named(TypeRef::INT)).default_value(50i32))
}

/// `Organization.projects(after, before, filter, first: Int = 25, last)` (`GSR-DEFAULTS`). Ports
/// `OrganizationProjectDirectoryResolver.resolveProjects`.
fn projects_field() -> Field {
    Field::new(
        "projects",
        TypeRef::named_nn("ProjectConnection"),
        |ctx| {
            FieldFuture::new(async move {
                let organization = ctx.parent_value.try_downcast_ref::<wire::Organization>()?;
                let organization_id = Uuid::parse_str(&organization.id.0)
                    .map_err(|error| async_graphql::Error::new(error.to_string()))?;

                let after = scalars::optional_string(ctx.args.get("after"))?;
                let before = scalars::optional_string(ctx.args.get("before"))?;
                if after.is_some() && before.is_some() {
                    return Err(async_graphql::Error::new(
                        "Supply either forward (first/after) or backward (last/before) pagination arguments, not both.",
                    ));
                }
                let first = scalars::optional_i64(ctx.args.get("first"))?;
                let last = scalars::optional_i64(ctx.args.get("last"))?;
                let filter = match scalars::defined(ctx.args.get("filter")) {
                    Some(filter) => {
                        let filter =
                            OrganizationProjectFilter::parse_value(context(), Some(filter))?;
                        AppOrganizationProjectFilter::from(filter.lifecycleStatus, filter.search)
                            .map_err(|error| async_graphql::Error::new(error.to_string()))?
                    }
                    None => AppOrganizationProjectFilter::none(),
                };

                let principal = ctx.ctx.data::<RequestPrincipal>()?;
                let repository = PgOrganizationProjectDirectoryRepository::new(
                    ctx.ctx.data::<sea_orm::DatabaseConnection>()?.clone(),
                );
                let service = OrganizationProjectDirectoryQueryService::new(repository);

                let page = if let Some(before) = &before {
                    service
                        .find_projects_before(
                            principal.0,
                            organization_id,
                            last.unwrap_or(25),
                            Some(before),
                            Some(filter.clone()),
                        )
                        .await
                } else {
                    service
                        .find_projects(
                            principal.0,
                            organization_id,
                            first.unwrap_or(25),
                            after.as_deref(),
                            Some(filter.clone()),
                        )
                        .await
                }
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;

                let edges: Vec<ProjectEdge> = page
                    .projects
                    .iter()
                    .map(|project| ProjectEdge {
                        cursor: OrganizationProjectCursor::encode(project, &filter),
                        node: Project {
                            id: project.id.to_string().into(),
                            slug: project.slug.clone(),
                            displayName: project.display_name.clone(),
                            lifecycleStatus: project.lifecycle_status.clone(),
                        },
                    })
                    .collect();

                let connection = ProjectConnection {
                    edges,
                    pageInfo: seaography::PageInfo {
                        has_previous_page: page.has_previous_page,
                        has_next_page: page.has_next_page,
                        start_cursor: page.start_cursor,
                        end_cursor: page.end_cursor,
                    },
                    totalCount: page.total_count as i32,
                };
                Ok(connection.gql_field_value(context()))
            })
        },
    )
    .argument(InputValue::new("after", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new("before", TypeRef::named(TypeRef::STRING)))
    .argument(InputValue::new(
        "filter",
        TypeRef::named("OrganizationProjectFilter"),
    ))
    .argument(InputValue::new("first", TypeRef::named(TypeRef::INT)).default_value(25i32))
    .argument(InputValue::new("last", TypeRef::named(TypeRef::INT)))
}

/// `&'static BuilderContext` for use inside a resolver closure, which cannot capture `mod.rs`'s
/// private `CONTEXT` directly across the module boundary.
fn context() -> &'static BuilderContext {
    crate::schema::context()
}

/// This module's contribution to the schema: the complex `Organization` object (data fields +
/// `projects`), the input types its filters need, `accessibleOrganizations`, and `organization(id)`.
pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_query::<OrganizationQueries>();
    builder.inputs.push(accessible_organizations_filter());
    builder
        .outputs
        .push(Organization::basic_object(context()).field(projects_field()));
    builder.register_custom_output::<OrganizationEdge>();
    builder.register_custom_output::<OrganizationConnection>();
    builder.register_custom_input::<OrganizationProjectFilter>();
    builder.queries.push(accessible_organizations_field());
}
