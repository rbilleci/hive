//! Ports the organization selector (`Directory`) and overview (`OrganizationContext`) of `main.tsx`.

use crate::api::directory::{
    request_organization_overview, request_organizations, OrganizationSummary, ProjectSummary,
};
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_params_map;

#[derive(Clone)]
enum SelectorState {
    Loading,
    SessionError,
    Loaded {
        organizations: Vec<OrganizationSummary>,
        has_next_page: bool,
        end_cursor: Option<String>,
        loading_more: bool,
    },
}

#[component]
pub fn OrganizationDirectory() -> impl IntoView {
    let revision = use_console().revision;
    let include_archived = RwSignal::new(false);
    let state = RwSignal::new(SelectorState::Loading);
    // A response applies only to the filter and the load it was requested for.
    let generation = StoredValue::new(0_u32);
    let current = move |expected: u32| generation.try_get_value() == Some(expected);

    Effect::new(move |_| {
        let archived = include_archived.get();
        let _ = revision.get();
        let request = generation.get_value() + 1;
        generation.set_value(request);
        state.set(SelectorState::Loading);
        spawn_local(async move {
            let result = request_organizations(archived, None).await;
            if !current(request) {
                return;
            }
            state.set(match result {
                Ok(connection) => SelectorState::Loaded {
                    organizations: connection.edges.into_iter().map(|edge| edge.node).collect(),
                    has_next_page: connection.page_info.has_next_page,
                    end_cursor: connection.page_info.end_cursor,
                    loading_more: false,
                },
                Err(_) => SelectorState::SessionError,
            });
        });
    });

    let load_more = move || {
        let SelectorState::Loaded {
            organizations,
            has_next_page: true,
            end_cursor: Some(cursor),
            loading_more: false,
        } = state.get_untracked()
        else {
            return;
        };
        let (archived, request) = (include_archived.get_untracked(), generation.get_value());
        state.set(SelectorState::Loaded {
            organizations: organizations.clone(),
            has_next_page: true,
            end_cursor: Some(cursor.clone()),
            loading_more: true,
        });
        spawn_local(async move {
            let result = request_organizations(archived, Some(cursor)).await;
            if !current(request) {
                return;
            }
            state.set(match result {
                Ok(connection) => SelectorState::Loaded {
                    organizations: organizations
                        .into_iter()
                        .chain(connection.edges.into_iter().map(|edge| edge.node))
                        .collect(),
                    has_next_page: connection.page_info.has_next_page,
                    end_cursor: connection.page_info.end_cursor,
                    loading_more: false,
                },
                Err(_) => SelectorState::SessionError,
            });
        });
    };

    view! {
        <main class="directory" aria-labelledby="directory-title">
            <PageHeader title_id="directory-title" title="Choose an organization".to_string() />
            <label class="archive-filter">
                <input type="checkbox" prop:checked=move || include_archived.get() on:change=move |event| include_archived.set(event_target_checked(&event)) />
                "Include archived organizations"
            </label>
            {move || match state.get() {
                SelectorState::Loading => view! { <p role="status">"Loading organizations…"</p> }.into_any(),
                SelectorState::SessionError => view! { <p role="alert">"Your session has expired. Sign in again to view organizations."</p> }.into_any(),
                SelectorState::Loaded { organizations, .. } if organizations.is_empty() => view! { <p role="status">"You do not have access to any organizations."</p> }.into_any(),
                SelectorState::Loaded { organizations, has_next_page, loading_more, .. } => view! {
                    <ul class="organization-list" aria-label="Accessible organizations">
                        {organizations.into_iter().map(|organization| view! {
                            <li><a href=format!("/organizations/{}", organization.id.inner())>
                                <span>{organization.display_name}</span>
                                {(organization.lifecycle_status == "ARCHIVED").then(|| view! { <small>"Archived"</small> })}
                            </a></li>
                        }).collect_view()}
                    </ul>
                    {has_next_page.then(|| view! {
                        <button class="load-more" type="button" on:click=move |_| load_more() disabled=loading_more>
                            {if loading_more { "Loading organizations…" } else { "Load more organizations" }}</button>
                    })}
                }.into_any(),
            }}
        </main>
    }
}

#[derive(Clone)]
struct Overview {
    organization: OrganizationSummary,
    projects: Vec<ProjectSummary>,
    has_next_page: bool,
    end_cursor: Option<String>,
    total_count: i32,
    loading_more: bool,
    load_more_error: bool,
}

#[derive(Clone)]
enum OverviewState {
    Loading,
    SessionError,
    Unavailable,
    Error,
    Loaded(Overview),
}

#[component]
pub fn OrganizationOverviewPage() -> impl IntoView {
    let revision = use_console().revision;
    let params = use_params_map();
    let organization_id =
        Memo::new(move |_| params.read().get("organization_id").unwrap_or_default());
    let state = RwSignal::new(OverviewState::Loading);
    let attempt = RwSignal::new(0_u32);
    let generation = StoredValue::new(0_u32);
    let current = move |expected: u32| generation.try_get_value() == Some(expected);

    Effect::new(move |_| {
        let id = organization_id.get();
        attempt.track();
        let _ = revision.get();
        let request = generation.get_value() + 1;
        generation.set_value(request);
        state.set(OverviewState::Loading);
        spawn_local(async move {
            let result = request_organization_overview(&id, None).await;
            if !current(request) {
                return;
            }
            state.set(match result {
                Ok(Some(overview)) => OverviewState::Loaded(Overview {
                    organization: OrganizationSummary {
                        id: overview.id,
                        slug: overview.slug,
                        display_name: overview.display_name,
                        lifecycle_status: overview.lifecycle_status,
                    },
                    projects: overview
                        .projects
                        .edges
                        .into_iter()
                        .map(|edge| edge.node)
                        .collect(),
                    has_next_page: overview.projects.page_info.has_next_page,
                    end_cursor: overview.projects.page_info.end_cursor,
                    total_count: overview.projects.total_count,
                    loading_more: false,
                    load_more_error: false,
                }),
                Ok(None) => OverviewState::Unavailable,
                Err(GraphqlError::SessionExpired) => OverviewState::SessionError,
                Err(GraphqlError::Transport(_)) => OverviewState::Error,
            });
        });
    });

    let load_more = move || {
        let OverviewState::Loaded(loaded) = state.get_untracked() else {
            return;
        };
        let (true, Some(cursor), false) = (
            loaded.has_next_page,
            loaded.end_cursor.clone(),
            loaded.loading_more,
        ) else {
            return;
        };
        let (id, request) = (organization_id.get_untracked(), generation.get_value());
        state.set(OverviewState::Loaded(Overview {
            loading_more: true,
            load_more_error: false,
            ..loaded.clone()
        }));
        spawn_local(async move {
            let result = request_organization_overview(&id, Some(cursor)).await;
            if !current(request) {
                return;
            }
            state.set(match result {
                Ok(Some(overview)) => OverviewState::Loaded(Overview {
                    projects: loaded
                        .projects
                        .into_iter()
                        .chain(overview.projects.edges.into_iter().map(|edge| edge.node))
                        .collect(),
                    has_next_page: overview.projects.page_info.has_next_page,
                    end_cursor: overview.projects.page_info.end_cursor,
                    total_count: overview.projects.total_count,
                    loading_more: false,
                    load_more_error: false,
                    organization: loaded.organization,
                }),
                Ok(None) => OverviewState::Unavailable,
                Err(GraphqlError::SessionExpired) => OverviewState::SessionError,
                Err(GraphqlError::Transport(_)) => OverviewState::Loaded(Overview {
                    loading_more: false,
                    load_more_error: true,
                    ..loaded
                }),
            });
        });
    };

    view! {
        <main class="directory organization-overview" aria-labelledby="organization-overview-title">
            {move || match state.get() {
                OverviewState::Loading => view! { <section aria-label="Loading organization overview" class="overview-skeleton"><div></div><div></div><div></div></section> }.into_any(),
                OverviewState::SessionError => view! { <p role="alert">"Your session has expired. Sign in again to view this organization."</p> }.into_any(),
                OverviewState::Unavailable => view! { <p role="status">"This organization is unavailable."</p> }.into_any(),
                OverviewState::Error => view! {
                    <section class="overview-error" aria-label="Organization overview error"><p role="alert">"We could not load this organization. Try again."</p>
                        <button type="button" on:click=move |_| attempt.update(|value| *value += 1)>"Retry"</button></section>
                }.into_any(),
                OverviewState::Loaded(loaded) => {
                    let status = loaded.organization.lifecycle_status.clone();
                    let badge = status.clone();
                    let meta = format!("{} · ID: {}", loaded.organization.id.inner(), loaded.organization.slug);
                    let more_label = if loaded.loading_more { "Loading projects…" } else if loaded.load_more_error { "Retry loading projects" } else { "Load more projects" };
                    view! {
                        <PageHeader title_id="organization-overview-title" title=loaded.organization.display_name.clone() meta=meta>
                            <span class="lifecycle-badge" data-status=badge>{status}</span>
                        </PageHeader>
                        <section aria-labelledby="projects-title">
                            <h2 id="projects-title">"Projects"</h2>
                            <p>{loaded.total_count}" projects"</p>
                            {loaded.projects.is_empty().then(|| view! { <p role="status">"This organization has no projects."</p> })}
                            {(!loaded.projects.is_empty()).then(|| view! {
                                <ul class="project-list" aria-label="Organization projects">
                                    {loaded.projects.iter().map(|project| view! {
                                        <li><a class="resource-card-link" href=format!("/projects/{}", project.id.inner())>
                                            <div><h3>{project.display_name.clone()}</h3><p>{project.id.inner().to_string()}</p><p>"ID: "{project.slug.clone()}</p></div>
                                            <span class="lifecycle-badge" data-status=project.lifecycle_status.clone()>{project.lifecycle_status.clone()}</span>
                                        </a></li>
                                    }).collect_view()}
                                </ul>
                            })}
                            {loaded.load_more_error.then(|| view! { <p class="overview-error" role="alert">"We could not load more projects. Try again."</p> })}
                            {loaded.has_next_page.then(|| view! { <button class="load-more" type="button" on:click=move |_| load_more() disabled=loaded.loading_more>{more_label}</button> })}
                        </section>
                    }.into_any()
                }
            }}
        </main>
    }
}
