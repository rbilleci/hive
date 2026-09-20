//! Ports `OrganizationProjectDirectory.tsx` and `ProjectAgentDirectory.tsx`, which differ only in
//! their nouns, columns, and query. The URL owns the lifecycle filter, the literal search, the page
//! size, and the page number (`page`, from 1). The filter controls
//! and any loaded page stay mounted across a refetch, so a keystroke in the search field never drops
//! focus. The search debounces 300 ms before it reaches the URL; the selects apply immediately.

use crate::api::directory::{
    request_organization_projects, request_project_agents, DirectoryPage, DirectoryRequest,
};
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_location, use_navigate, use_params_map, use_query_map};
use leptos_router::NavigateOptions;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

const PAGE_SIZE_OPTIONS: [i32; 3] = [10, 25, 50];
const DEFAULT_PAGE_SIZE: i32 = 10;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);

type Fetch = fn(
    DirectoryRequest,
) -> Pin<Box<dyn Future<Output = Result<Option<DirectoryPage>, GraphqlError>>>>;

#[derive(Clone, Copy)]
struct DirectoryKind {
    /// The route parameter naming the owner, and the class and id prefix (`project`, `agent`).
    owner_param: &'static str,
    css: &'static str,
    title: &'static str,
    /// "project directory" / "project agent directory", as the accessible labels spell it.
    subject: &'static str,
    region_label: &'static str,
    pages_label: &'static str,
    noun: &'static str,
    session_message: &'static str,
    unavailable_message: &'static str,
    empty_message: &'static str,
    lifecycles: &'static [(&'static str, &'static str)],
    columns: &'static [&'static str],
    create_label: &'static str,
    create_suffix: &'static str,
    owner_prefix: &'static str,
    fetch: Fetch,
}

#[derive(Clone, PartialEq)]
enum DirectoryState {
    Loading,
    SessionError,
    Unavailable,
    Error,
    Loaded {
        page: DirectoryPage,
        refreshing: bool,
        refresh_error: bool,
    },
}

#[component]
pub fn OrganizationProjectDirectory() -> impl IntoView {
    keyset_directory(DirectoryKind {
        owner_param: "organization_id",
        css: "project",
        title: "Projects",
        subject: "project directory",
        region_label: "Organization project directory",
        pages_label: "Project pages",
        noun: "projects",
        session_message: "Your session has expired. Sign in again to view this organization.",
        unavailable_message: "This organization is unavailable.",
        empty_message: "This organization has no projects.",
        lifecycles: &[("ACTIVE", "Active"), ("ARCHIVED", "Archived")],
        columns: &["Name", "ID", "Lifecycle"],
        create_label: "Create project",
        create_suffix: "/projects/new",
        owner_prefix: "/organizations/",
        fetch: |request| Box::pin(request_organization_projects(request)),
    })
}

#[component]
pub fn ProjectAgentDirectory() -> impl IntoView {
    keyset_directory(DirectoryKind {
        owner_param: "project_id",
        css: "agent",
        title: "Agents",
        subject: "project agent directory",
        region_label: "Project agent directory",
        pages_label: "Agent pages",
        noun: "agents",
        session_message: "Your session has expired. Sign in again to view this project’s agents.",
        unavailable_message: "This project is unavailable.",
        empty_message: "This project has no agents.",
        lifecycles: &[
            ("ACTIVE", "Active"),
            ("DEPRECATED", "Deprecated"),
            ("ARCHIVED", "Archived"),
        ],
        columns: &["Name", "ID", "Last published version", "Model", "Lifecycle"],
        create_label: "Create agent",
        create_suffix: "/agents/new",
        owner_prefix: "/projects/",
        fetch: |request| Box::pin(request_project_agents(request)),
    })
}

fn capitalized(text: &str) -> String {
    let mut letters = text.chars();
    letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default()
}

fn keyset_directory(kind: DirectoryKind) -> impl IntoView {
    let revision = use_console().revision;
    let params = use_params_map();
    let query = use_query_map();
    let pathname = use_location().pathname;
    let navigate = use_navigate();
    let owner_id = Memo::new(move |_| params.read().get(kind.owner_param).unwrap_or_default());
    let lifecycle = Memo::new(move |_| {
        query
            .read()
            .get("lifecycle")
            .filter(|value| kind.lifecycles.iter().any(|(code, _)| code == value))
            .unwrap_or_default()
    });
    let url_search = Memo::new(move |_| query.read().get("search").unwrap_or_default());
    let page_size = Memo::new(move |_| {
        query
            .read()
            .get("pageSize")
            .and_then(|value| value.parse().ok())
            .filter(|size| PAGE_SIZE_OPTIONS.contains(size))
            .unwrap_or(DEFAULT_PAGE_SIZE)
    });
    let request = Memo::new(move |_| DirectoryRequest {
        owner_id: owner_id.get(),
        lifecycle: lifecycle.get(),
        search: url_search.get(),
        // The URL counts pages from 1; Seaography counts from 0.
        page: query
            .read()
            .get("page")
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|page| *page >= 1)
            .map_or(0, |page| page - 1),
        page_size: page_size.get(),
    });

    let state = RwSignal::new(DirectoryState::Loading);
    let attempt = RwSignal::new(0_u32);
    let generation = StoredValue::new(0_u32);
    let latest = StoredValue::new(None::<DirectoryPage>);
    let search_input = RwSignal::new(url_search.get_untracked());
    let debounce = StoredValue::new(None::<TimeoutHandle>);
    let cancel_debounce = move || {
        if let Some(handle) = debounce.try_update_value(Option::take).flatten() {
            handle.clear();
        }
    };
    on_cleanup(cancel_debounce);
    // A URL-driven change (back/forward, a lifecycle change) shows in the field; the field's own
    // input never passes through here, so this cannot fight the debounce.
    Effect::new(move |_| search_input.set(url_search.get()));

    Effect::new(move |_| {
        let wanted = request.get();
        attempt.track();
        let _ = revision.get();
        let current = generation.get_value() + 1;
        generation.set_value(current);
        if wanted.owner_id.is_empty() {
            state.set(DirectoryState::Unavailable);
            return;
        }
        state.set(match latest.get_value() {
            Some(page) => DirectoryState::Loaded {
                page,
                refreshing: true,
                refresh_error: false,
            },
            None => DirectoryState::Loading,
        });
        spawn_local(async move {
            let result = (kind.fetch)(wanted).await;
            if generation.try_get_value() != Some(current) {
                return;
            }
            state.set(match result {
                Ok(Some(page)) => {
                    latest.set_value(Some(page.clone()));
                    DirectoryState::Loaded {
                        page,
                        refreshing: false,
                        refresh_error: false,
                    }
                }
                Ok(None) => {
                    latest.set_value(None);
                    DirectoryState::Unavailable
                }
                Err(GraphqlError::SessionExpired) => {
                    latest.set_value(None);
                    DirectoryState::SessionError
                }
                Err(GraphqlError::Transport(_)) => match latest.get_value() {
                    Some(page) => DirectoryState::Loaded {
                        page,
                        refreshing: false,
                        refresh_error: true,
                    },
                    None => DirectoryState::Error,
                },
            });
        });
    });

    // Rewrites the query string in place. Parameters keep one order, so a URL is comparable.
    let set_query = move |lifecycle: String, search: String, size: i32, page: i32| {
        generation.update_value(|value| *value += 1);
        let encode = |value: &str| String::from(js_sys::encode_uri_component(value));
        let mut pairs = Vec::new();
        if !lifecycle.is_empty() {
            pairs.push(format!("lifecycle={}", encode(&lifecycle)));
        }
        if !search.is_empty() {
            pairs.push(format!("search={}", encode(&search)));
        }
        if size != DEFAULT_PAGE_SIZE {
            pairs.push(format!("pageSize={size}"));
        }
        if page > 0 {
            pairs.push(format!("page={}", page + 1));
        }
        let target = if pairs.is_empty() {
            pathname.get_untracked()
        } else {
            format!("{}?{}", pathname.get_untracked(), pairs.join("&"))
        };
        navigate(
            &target,
            NavigateOptions {
                replace: true,
                scroll: false,
                ..Default::default()
            },
        );
        // The same URL again (a retried Next after a failure) would not rerun the request by itself.
        attempt.update(|value| *value += 1);
    };
    let update_filters = {
        let set_query = set_query.clone();
        move |lifecycle: String, search: String| {
            set_query(lifecycle, search, page_size.get_untracked(), 0)
        }
    };
    let on_search_input = {
        let update_filters = update_filters.clone();
        move |value: String| {
            search_input.set(value.clone());
            cancel_debounce();
            let update_filters = update_filters.clone();
            debounce.set_value(
                set_timeout_with_handle(
                    move || update_filters(lifecycle.get_untracked(), value),
                    SEARCH_DEBOUNCE,
                )
                .ok(),
            );
        }
    };
    // A page-size change moves every page boundary, so it returns to page 1.
    let update_page_size = {
        let set_query = set_query.clone();
        move |size: i32| {
            set_query(
                lifecycle.get_untracked(),
                url_search.get_untracked(),
                size,
                0,
            )
        }
    };
    let go_to_page = {
        let set_query = set_query.clone();
        move |page: Option<i32>| {
            if let Some(page) = page {
                set_query(
                    lifecycle.get_untracked(),
                    url_search.get_untracked(),
                    page_size.get_untracked(),
                    page,
                );
            }
        }
    };

    let page = Memo::new(move |_| match state.get() {
        DirectoryState::Loaded { page, .. } => Some(page),
        _ => None,
    });
    let flags = Memo::new(move |_| match state.get() {
        DirectoryState::Loaded {
            refreshing,
            refresh_error,
            ..
        } => (refreshing, refresh_error),
        _ => (false, false),
    });
    let shape = Memo::new(move |_| std::mem::discriminant(&state.get()));
    let css = kind.css;
    let class = move |suffix: &str| format!("{css}-directory{suffix}");
    let title_id = format!("{}-directory-title", kind.css);
    let main_class = format!("directory {}", class(""));

    let loaded = {
        let (update_filters, on_search_input, update_page_size, go_to_page) = (
            update_filters.clone(),
            on_search_input.clone(),
            update_page_size.clone(),
            go_to_page.clone(),
        );
        move || {
            let (update_filters, on_search_input, update_page_size) = (
                update_filters.clone(),
                on_search_input.clone(),
                update_page_size.clone(),
            );
            let (previous, next) = (go_to_page.clone(), go_to_page.clone());
            let create_href = move || {
                format!(
                    "{}{}{}",
                    kind.owner_prefix,
                    page.with(|value| value
                        .as_ref()
                        .map(|value| value.owner_id.clone())
                        .unwrap_or_default()),
                    kind.create_suffix
                )
            };
            let table = move || {
                page.get().map(|value| {
                if value.rows.is_empty() {
                    let filtered = !lifecycle.get().is_empty() || !url_search.get().is_empty();
                    let message = if filtered { format!("No {} match these filters.", kind.noun) } else { kind.empty_message.to_string() };
                    return view! { <p role="status">{message}</p> }.into_any();
                }
                view! {
                    <div class="directory-table-scroll" role="region" aria-label=kind.region_label tabindex="0"><table>
                        <thead><tr>{kind.columns.iter().map(|column| view! { <th scope="col">{*column}</th> }).collect_view()}</tr></thead>
                        <tbody>{value.rows.into_iter().map(|row| view! {
                            <tr><th scope="row"><a href=row.href>{row.name}</a></th>
                                {row.cells.into_iter().map(|cell| view! { <td>{cell}</td> }).collect_view()}
                                <td><span class="lifecycle-badge" data-status=row.lifecycle_status.clone()>{row.lifecycle_status.clone()}</span></td></tr>
                        }).collect_view()}</tbody>
                    </table></div>
                }.into_any()
            })
            };
            view! {
                <PageHeader title_id=if kind.css == "project" { "project-directory-title" } else { "agent-directory-title" } title=kind.title.to_string()>
                    <a class="primary-action" href=create_href>{kind.create_label}</a>
                </PageHeader>
                <section class=class("-filters") aria-label=format!("{} filters", capitalized(kind.subject))>
                    <label>"Lifecycle"
                        <select prop:value=move || lifecycle.get() on:change=move |event| update_filters(event_target_value(&event), url_search.get_untracked())>
                            <option value="">"All lifecycles"</option>
                            {kind.lifecycles.iter().map(|(code, label)| view! { <option value=*code selected=move || lifecycle.get() == *code>{*label}</option> }).collect_view()}
                        </select>
                    </label>
                    <label>{format!("Search {}", kind.noun)}
                        <input type="search" prop:value=move || search_input.get() on:input=move |event| on_search_input(event_target_value(&event)) />
                    </label>
                    <label>"Rows per page"
                        <select prop:value=move || page_size.get().to_string() on:change=move |event| update_page_size(event_target_value(&event).parse().unwrap_or(DEFAULT_PAGE_SIZE))>
                            {PAGE_SIZE_OPTIONS.iter().map(|option| view! { <option value=option.to_string() selected=move || page_size.get() == *option>{*option}</option> }).collect_view()}
                        </select>
                    </label>
                    <Show when=move || flags.get().0><span role="status" class=class("-refreshing")>"Refreshing…"</span></Show>
                </section>
                <Show when=move || flags.get().1><p class=class("-error") role="alert">"We could not refresh this list. Results shown may be stale."</p></Show>
                {table}
                <p class="directory-count">{move || page.with(|value| value.as_ref().map(|value| format!("{} of {} {} shown", value.rows.len(), value.total_count, kind.noun)))}</p>
                <nav class="pagination" aria-label=kind.pages_label>
                    <button type="button" on:click=move |_| previous(page.with_untracked(|value| value.as_ref().filter(|value| value.has_previous_page()).map(|value| value.page - 1)))
                        disabled=move || !page.with(|value| value.as_ref().is_some_and(|value| value.has_previous_page()))>"Previous"</button>
                    <button type="button" on:click=move |_| next(page.with_untracked(|value| value.as_ref().filter(|value| value.has_next_page()).map(|value| value.page + 1)))
                        disabled=move || !page.with(|value| value.as_ref().is_some_and(|value| value.has_next_page()))>"Next"</button>
                </nav>
            }
        }
    };

    view! {
        <main class=main_class aria-labelledby=title_id>
            {move || {
                let _ = shape.get();
                match state.get_untracked() {
                    DirectoryState::Loading => view! { <section aria-label=format!("Loading {}", kind.subject) class=class("-skeleton")><div></div><div></div><div></div></section> }.into_any(),
                    DirectoryState::SessionError => view! { <p role="alert">{kind.session_message}</p> }.into_any(),
                    DirectoryState::Unavailable => view! { <p role="status">{kind.unavailable_message}</p> }.into_any(),
                    DirectoryState::Error => view! {
                        <section class=class("-error") aria-label=format!("{} error", capitalized(kind.subject))>
                            <p role="alert">{format!("We could not load this {}. Try again.", kind.subject)}</p>
                            <button type="button" on:click=move |_| attempt.update(|value| *value += 1)>"Retry"</button></section>
                    }.into_any(),
                    DirectoryState::Loaded { .. } => loaded().into_any(),
                }
            }}
        </main>
    }
}
