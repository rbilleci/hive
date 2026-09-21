//! The organization selector and one organization's overview, both loaded through the shared
//! request state machine and both extended in place by their own "Load more" control.

use super::request::{use_request, Policy, RequestState};
use crate::api::directory::{request_organization_overview, request_organizations};
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_params_map;

#[component]
pub fn OrganizationDirectory() -> impl IntoView {
    let include_archived = RwSignal::new(false);
    let archived = Memo::new(move |_| include_archived.get());
    // The selector words every failure as an expired session, so no other refusal reaches it.
    let live = use_request(
        archived,
        |archived: bool| {
            Box::pin(async move {
                request_organizations(archived, 0)
                    .await
                    .map(Some)
                    .map_err(|_| GraphqlError::SessionExpired)
            })
        },
        Policy::on_demand(),
    );
    let loading_more = RwSignal::new(false);

    let load_more = move || {
        let Some(shown) = live
            .value_untracked()
            .filter(|_| !loading_more.get_untracked())
        else {
            return;
        };
        let Some(next_page) = shown.next_page() else {
            return;
        };
        let (archived, load) = (archived.get_untracked(), live.generation.get_value());
        loading_more.set(true);
        spawn_local(async move {
            let result = request_organizations(archived, next_page).await;
            if live.generation.try_get_value() != Some(load) {
                return;
            }
            loading_more.set(false);
            match result {
                Ok(page) => {
                    let mut shown = shown;
                    shown.extend(page);
                    live.adopt.run(shown);
                }
                Err(_) => live.state.set(RequestState::SessionError),
            }
        });
    };

    view! {
        <main class="directory" aria-labelledby="directory-title">
            <PageHeader title_id="directory-title" title="Choose an organization".to_string() />
            <label class="archive-filter">
                <input type="checkbox" prop:checked=move || include_archived.get() on:change=move |event| include_archived.set(event_target_checked(&event)) />
                "Include archived organizations"
            </label>
            {move || match live.state.get() {
                RequestState::SessionError => view! { <p role="alert">{crate::session_expired!("Sign in again to view organizations.")}</p> }.into_any(),
                RequestState::Loaded { snapshot, .. } if snapshot.value.rows.is_empty() => view! { <p role="status">"You do not have access to any organizations."</p> }.into_any(),
                RequestState::Loaded { snapshot, .. } => { let shown = snapshot.value; let more = shown.next_page().is_some(); view! {
                    <ul class="organization-list" aria-label="Accessible organizations">
                        {shown.rows.into_iter().map(|organization| view! {
                            <li><a href=format!("/organizations/{}", organization.id)>
                                <span>{organization.display_name}</span>
                                {(organization.lifecycle_status == "ARCHIVED").then(|| view! { <small>"Archived"</small> })}
                            </a></li>
                        }).collect_view()}
                    </ul>
                    {more.then(|| view! {
                        <button class="load-more" type="button" on:click=move |_| load_more() disabled=move || loading_more.get()>
                            {move || if loading_more.get() { "Loading organizations…" } else { "Load more organizations" }}</button>
                    })}
                }.into_any() }
                _ => view! { <p role="status">"Loading organizations…"</p> }.into_any(),
            }}
        </main>
    }
}

#[component]
pub fn OrganizationOverviewPage() -> impl IntoView {
    let params = use_params_map();
    let organization_id =
        Memo::new(move |_| params.read().get("organization_id").unwrap_or_default());
    let live = use_request(
        organization_id,
        |id: String| Box::pin(async move { request_organization_overview(&id, 0).await }),
        Policy::on_demand(),
    );
    let (loading_more, load_more_error) = (RwSignal::new(false), RwSignal::new(false));

    let load_more = move || {
        let Some((organization, projects)) = live
            .value_untracked()
            .filter(|_| !loading_more.get_untracked())
        else {
            return;
        };
        let Some(next_page) = projects.next_page() else {
            return;
        };
        let (id, load) = (organization_id.get_untracked(), live.generation.get_value());
        loading_more.set(true);
        load_more_error.set(false);
        spawn_local(async move {
            let result = request_organization_overview(&id, next_page).await;
            if live.generation.try_get_value() != Some(load) {
                return;
            }
            loading_more.set(false);
            match result {
                Ok(Some((_, page))) => {
                    let mut projects = projects;
                    projects.extend(page);
                    live.adopt.run((organization, projects));
                }
                Ok(None) => live.state.set(RequestState::Unavailable),
                Err(GraphqlError::SessionExpired) => live.state.set(RequestState::SessionError),
                Err(GraphqlError::Transport(_)) => load_more_error.set(true),
            }
        });
    };

    view! {
        <main class="directory organization-overview" aria-labelledby="organization-overview-title">
            {move || match live.state.get() {
                RequestState::Loading => view! { <section aria-label="Loading organization overview" class="overview-skeleton"><div></div><div></div><div></div></section> }.into_any(),
                RequestState::SessionError => view! { <p role="alert">{crate::session_expired!("Sign in again to view this organization.")}</p> }.into_any(),
                RequestState::Unavailable => view! { <p role="status">"This organization is unavailable."</p> }.into_any(),
                RequestState::Error => view! {
                    <section class="overview-error" aria-label="Organization overview error"><p role="alert">"We could not load this organization. Try again."</p>
                        <button type="button" on:click=move |_| live.retry.run(())>"Retry"</button></section>
                }.into_any(),
                RequestState::Loaded { snapshot, .. } => {
                    let (organization, projects) = snapshot.value;
                    let status = organization.lifecycle_status.clone();
                    let badge = status.clone();
                    let meta = format!("{} · ID: {}", organization.id, organization.slug);
                    let more = projects.next_page().is_some();
                    let more_label = if loading_more.get() { "Loading projects…" } else if load_more_error.get() { "Retry loading projects" } else { "Load more projects" };
                    view! {
                        <PageHeader title_id="organization-overview-title" title=organization.display_name.clone() meta=meta>
                            <span class="lifecycle-badge" data-status=badge>{status}</span>
                        </PageHeader>
                        <section aria-labelledby="projects-title">
                            <h2 id="projects-title">"Projects"</h2>
                            <p>{projects.total()}" projects"</p>
                            {projects.rows.is_empty().then(|| view! { <p role="status">"This organization has no projects."</p> })}
                            {(!projects.rows.is_empty()).then(|| view! {
                                <ul class="project-list" aria-label="Organization projects">
                                    {projects.rows.iter().map(|project| view! {
                                        <li><a class="resource-card-link" href=format!("/projects/{}", project.id)>
                                            <div><h3>{project.display_name.clone()}</h3><p>{project.id.clone()}</p><p>"ID: "{project.slug.clone()}</p></div>
                                            <span class="lifecycle-badge" data-status=project.lifecycle_status.clone()>{project.lifecycle_status.clone()}</span>
                                        </a></li>
                                    }).collect_view()}
                                </ul>
                            })}
                            {load_more_error.get().then(|| view! { <p class="overview-error" role="alert">"We could not load more projects. Try again."</p> })}
                            {more.then(|| view! { <button class="load-more" type="button" on:click=move |_| load_more() disabled=loading_more.get()>{more_label}</button> })}
                        </section>
                    }.into_any()
                }
            }}
        </main>
    }
}
