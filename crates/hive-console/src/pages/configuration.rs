use super::request::{use_request, Policy, RequestState};
use crate::api::configuration::{
    create_resource, publish_resource, reference_kind, request_catalog, request_known,
    request_resource, request_resources, update_resource_draft, validate_resource,
    ConfigurationMutationPayload, CreateReusableResourceInput, ReferenceOption, ReusableResource,
    ReusableResourceRevisionInput, UpdateReusableResourceDraftInput,
};
use crate::code_editor::{CodeEditor, SafeMarkdownPreview};
use crate::confirmation_dialog::ConfirmationDialog;
use crate::format::{encode, joined_or, local_time};
use crate::navigation_guard::use_navigation_guard;
use crate::page_header::PageHeader;
use crate::shell::{use_console, ConsoleStore};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_location, use_navigate, use_params_map, use_query_map};
use leptos_router::NavigateOptions;

const UNAVAILABLE_ROUTE: &str = "This route is unavailable or you no longer have access.";

fn can_author(console: ConsoleStore, project: &str) -> bool {
    console.context.with(|context| {
        context.capabilities.iter().any(|capability| {
            capability.code == "CONFIGURATION.AUTHOR" && capability.scope_id.inner() == project
        })
    })
}

fn organization_of(console: ConsoleStore, project: &str) -> String {
    console.context.with(|context| {
        context
            .organizations
            .iter()
            .find(|organization| {
                organization
                    .projects
                    .iter()
                    .any(|entry| entry.id.inner() == project)
            })
            .map(|organization| organization.id.inner().to_string())
            .unwrap_or_default()
    })
}

#[component]
pub fn CatalogPage() -> impl IntoView {
    catalog_page(false)
}

#[component]
pub fn EnvironmentsPage() -> impl IntoView {
    catalog_page(true)
}

fn catalog_page(environments: bool) -> impl IntoView {
    let params = use_params_map();
    let organization_id =
        Memo::new(move |_| params.read().get("organization_id").unwrap_or_default());
    let live = use_request(
        organization_id,
        |id: String| Box::pin(async move { request_catalog(&id).await }),
        Policy::on_demand(),
    );
    view! {
        <main class="console-page-frame configuration-page" aria-labelledby="catalog-title">
            {move || view! { <PageHeader title_id="catalog-title" title=if environments { "Environments" } else { "Catalog" }.to_string()
                description="Git-owned, immutable local release metadata. Changes are not available from this console."
                description_link=(format!("/organizations/{}/audit", organization_id.get()), "Review catalog configuration audit history") /> }}
            {move || match live.state.get() {
                RequestState::Loading => view! { <p role="status">"Loading configuration…"</p> }.into_any(),
                RequestState::Unavailable => view! { <p role="alert">{UNAVAILABLE_ROUTE}</p> }.into_any(),
                RequestState::Error | RequestState::SessionError => view! { <p role="alert">"We could not load configuration. "<button type="button" on:click=move |_| live.retry.run(())>"Retry"</button></p> }.into_any(),
                RequestState::Loaded { snapshot, .. } => { let release = snapshot.value; view! {
                    <section class="configuration-release" aria-label="Catalog release"><dl>
                        <dt>"Release"</dt><dd>{release.id.clone()}</dd><dt>"Source"</dt><dd>{release.source.clone()}</dd>
                        <dt>"Source digest"</dt><dd><code>{release.source_digest.clone()}</code></dd><dt>"Released"</dt><dd>{local_time(&release.released_at)}</dd></dl></section>
                    {if environments { view! {
                        <section aria-labelledby="environment-list-title"><h2 id="environment-list-title">"Available environments"</h2><ul class="configuration-cards">
                            {release.catalog_environments.nodes.into_iter().map(|row| view! { <li><h3>{row.environment}</h3><p>"Definitions show exact availability; this view is read-only."</p></li> }).collect_view()}</ul></section>
                    }.into_any() } else { view! {
                        <section aria-labelledby="definition-list-title"><h2 id="definition-list-title">"Approved definitions"</h2><ul class="configuration-cards">
                            {release.catalog_definitions.nodes.into_iter().map(|definition| { let environments = definition.available_environments().join(", "); view! {
                                <li><h3>{definition.display_name}</h3>
                                    <p><strong>"Typed ID:"</strong>" "{format!("{}:{}@{}", definition.definition_kind, definition.identity, definition.version)}</p>
                                    <p><strong>"Definition digest:"</strong>" "<code>{definition.content_digest}</code></p>
                                    <p><strong>"Environments:"</strong>" "{environments}</p></li>
                            } }).collect_view()}</ul></section>
                    }.into_any() }}
                }.into_any() }
            }}
        </main>
    }
}

const PROMPT_PAGE_SIZE: usize = 10;

#[component]
pub fn PromptLibraryPage() -> impl IntoView {
    let console = use_console();
    let params = use_params_map();
    let query = use_query_map();
    let pathname = use_location().pathname;
    let navigate = use_navigate();
    let project_id = Memo::new(move |_| params.read().get("project_id").unwrap_or_default());
    let search = Memo::new(move |_| query.read().get("search").unwrap_or_default());
    let page = Memo::new(move |_| {
        query
            .read()
            .get("page")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|page| *page >= 1)
            .unwrap_or(1)
    });
    let live = use_request(
        project_id,
        |project: String| {
            Box::pin(async move { request_resources(&project, Some("PROMPT".to_string())).await })
        },
        Policy::on_demand(),
    );
    let update = Callback::new(move |(next_search, next_page): (String, usize)| {
        let mut pairs = Vec::new();
        if !next_search.is_empty() {
            pairs.push(format!("search={}", encode(&next_search)));
        }
        if next_page > 1 {
            pairs.push(format!("page={next_page}"));
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
    });
    let matching = Memo::new(move |_| {
        let needle = search.get().to_lowercase();
        live.value()
            .unwrap_or_default()
            .into_iter()
            .filter(|prompt| {
                format!("{} {}", prompt.name, prompt.identity)
                    .to_lowercase()
                    .contains(&needle)
            })
            .collect::<Vec<_>>()
    });
    let page_count =
        Memo::new(move |_| matching.with(|all| all.len().div_ceil(PROMPT_PAGE_SIZE).max(1)));
    let shown_page = Memo::new(move |_| page.get().min(page_count.get()));
    let ready = move || matches!(live.state.get(), RequestState::Loaded { .. });
    view! {
        <main class="configuration-page prompt-library" aria-labelledby="prompt-library-title">
            <PageHeader title_id="prompt-library-title" title="Prompts".to_string() description="Search drafts and open a dedicated authoring workspace.">
                {move || can_author(console, &project_id.get()).then(|| view! { <a class="primary-action" href=format!("/projects/{}/prompts/new", project_id.get())>"Create"</a> })}
            </PageHeader>
            <label class="prompt-search">"Search prompts"
                <input type="search" prop:value=move || search.get() on:input=move |event| update.run((event_target_value(&event), 1)) placeholder="Name or identity" /></label>
            {move || match live.state.get() {
                RequestState::Loading => Some(view! { <p role="status">"Loading prompts…"</p> }.into_any()),
                RequestState::Error | RequestState::SessionError => Some(view! { <p role="alert">"Prompts could not be loaded."</p> }.into_any()),
                RequestState::Unavailable => Some(view! { <p role="status">"Prompts are unavailable."</p> }.into_any()),
                RequestState::Loaded { .. } if matching.with(Vec::is_empty) => Some(view! { <p role="status">"No prompts match this search."</p> }.into_any()),
                RequestState::Loaded { .. } => None,
            }}
            {move || { let start = (shown_page.get() - 1) * PROMPT_PAGE_SIZE;
                let visible: Vec<_> = matching.get().into_iter().skip(start).take(PROMPT_PAGE_SIZE).collect();
                (ready() && !visible.is_empty()).then(|| view! { <ul class="prompt-library-list">{visible.into_iter().map(|prompt| view! {
                    <li><a href=format!("/projects/{}/prompts/{}/edit", project_id.get_untracked(), prompt.id)>
                        <span><strong>{prompt.name}</strong><small>{prompt.identity}</small></span>
                        <span class="status-with-text"><span aria-hidden="true">"●"</span>{prompt.draft.validation_status}</span></a></li>
                }).collect_view()}</ul> }) }}
            {move || { (ready() && page_count.get() > 1).then(|| view! {
                <nav class="pagination" aria-label="Prompt pages">
                    <button type="button" disabled=move || page.get() <= 1 on:click=move |_| update.run((search.get_untracked(), page.get_untracked() - 1))>"Previous"</button>
                    <span>"Page "{move || shown_page.get()}" of "{move || page_count.get()}</span>
                    <button type="button" disabled=move || page.get() >= page_count.get() on:click=move |_| update.run((search.get_untracked(), page.get_untracked() + 1))>"Next"</button>
                </nav> }) }}
        </main>
    }
}

/// Typed references as the prompt editor accepts them: separated by whitespace or commas.
fn references(value: &str) -> Vec<String> {
    value
        .split(|letter: char| letter.is_whitespace() || letter == ',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorState {
    Loading,
    Ready,
    Saving,
    Unavailable,
    Error,
}

#[derive(Clone, PartialEq, Default)]
struct PromptDocument {
    name: String,
    content: String,
    dependencies: String,
}

impl PromptDocument {
    fn of(resource: &ReusableResource) -> Self {
        Self {
            name: resource.name.clone(),
            content: resource.draft.content.clone(),
            dependencies: resource.draft.dependencies().join(", "),
        }
    }
}

#[component]
pub fn PromptEditorPage() -> impl IntoView {
    let console = use_console();
    let params = use_params_map();
    let navigate = use_navigate();
    let project_id = Memo::new(move |_| params.read().get("project_id").unwrap_or_default());
    let resource_id = Memo::new(move |_| params.read().get("resource_id"));
    let resource = RwSignal::new(None::<ReusableResource>);
    let state = RwSignal::new(if resource_id.get_untracked().is_some() {
        EditorState::Loading
    } else {
        EditorState::Ready
    });
    let (name, content, dependencies) = (
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
    );
    let saved = RwSignal::new(PromptDocument::default());
    let problem = RwSignal::new(String::new());
    let review = RwSignal::new(false);
    let dirty = Memo::new(move |_| {
        state.get() == EditorState::Ready
            && saved.with(|document| {
                document.name != name.get()
                    || document.content != content.get()
                    || document.dependencies != dependencies.get()
            })
    });
    let guard = use_navigation_guard(dirty.into());
    let leave = navigate.clone();

    let adopt = move |next: ReusableResource| {
        let document = PromptDocument::of(&next);
        name.set(document.name.clone());
        content.set(document.content.clone());
        dependencies.set(document.dependencies.clone());
        saved.set(document);
        resource.set(Some(next));
    };
    Effect::new(move |_| {
        let (project, id) = (project_id.get(), resource_id.get());
        // A revision bump must not reload over unsaved text, so it is not tracked here.
        let Some(id) = id else { return };
        if resource
            .with_untracked(|current| current.as_ref().is_some_and(|current| current.id == id))
        {
            return;
        }
        state.set(EditorState::Loading);
        spawn_local(async move {
            match request_resource(&project, &id).await {
                Ok(Some(next)) if next.resource_kind == "PROMPT" => {
                    adopt(next);
                    state.set(EditorState::Ready);
                }
                Ok(_) => state.set(EditorState::Unavailable),
                Err(_) => state.set(EditorState::Error),
            }
        });
    });
    let unload = window_event_listener(ev::beforeunload, move |event| {
        if dirty.try_get_untracked().unwrap_or(false) {
            event.prevent_default();
        }
    });
    on_cleanup(move || unload.remove());

    // Applies a mutation payload; returns the resource when the server accepted the change.
    let apply = move |payload: ConfigurationMutationPayload| -> Option<ReusableResource> {
        state.set(EditorState::Ready);
        match (payload.problems.first(), payload.resource) {
            (None, Some(next)) => {
                adopt(next.clone());
                problem.set(String::new());
                Some(next)
            }
            (first, _) => {
                problem.set(
                    first.map_or("The prompt could not be updated.".to_string(), |first| {
                        first.message.clone()
                    }),
                );
                None
            }
        }
    };
    let save = move || {
        let project = project_id.get_untracked();
        if !can_author(console, &project) || state.get_untracked() != EditorState::Ready {
            return;
        }
        state.set(EditorState::Saving);
        problem.set(String::new());
        let navigate = navigate.clone();
        let (text, listed) = (
            content.get_untracked(),
            references(&dependencies.get_untracked()),
        );
        spawn_local(async move {
            let result = match resource.get_untracked() {
                None => create_resource(CreateReusableResourceInput {
                    project_id: project.as_str().into(),
                    kind: "PROMPT".to_string(),
                    name: name.get_untracked(),
                    content: text,
                    dependencies: listed,
                })
                .await
                .map(|payload| (payload, true)),
                Some(current) => update_resource_draft(UpdateReusableResourceDraftInput {
                    project_id: project.as_str().into(),
                    resource_id: current.id.as_str().into(),
                    expected_revision: current.current_draft_revision,
                    content: text,
                    dependencies: listed,
                })
                .await
                .map(|payload| (payload, false)),
            };
            match result {
                Ok((payload, created)) => {
                    if let (Some(next), true) = (apply(payload), created) {
                        navigate(
                            &format!("/projects/{project}/prompts/{}/edit", next.id),
                            NavigateOptions {
                                replace: true,
                                ..Default::default()
                            },
                        );
                    }
                }
                Err(_) => {
                    problem.set("The prompt could not be saved.".to_string());
                    state.set(EditorState::Ready);
                }
            }
        });
    };
    let act = move |publish: bool| {
        let Some(current) = resource
            .get_untracked()
            .filter(|_| !dirty.get_untracked() && state.get_untracked() == EditorState::Ready)
        else {
            problem.set("Save your changes before continuing.".to_string());
            return;
        };
        problem.set(String::new());
        let input = ReusableResourceRevisionInput {
            project_id: project_id.get_untracked().as_str().into(),
            resource_id: current.id.as_str().into(),
            expected_revision: current.current_draft_revision,
        };
        spawn_local(async move {
            let result = if publish {
                publish_resource(input).await
            } else {
                validate_resource(input).await
            };
            match result {
                Ok(payload) => {
                    apply(payload);
                }
                Err(_) => problem.set(format!(
                    "The prompt could not be {}.",
                    if publish { "published" } else { "validated" }
                )),
            }
        });
    };

    let title = Signal::derive(move || {
        resource.with(|current| {
            current
                .as_ref()
                .map_or("Create prompt".to_string(), |current| {
                    format!("Edit {}", current.name)
                })
        })
    });
    let status = Signal::derive(move || {
        if dirty.get() {
            "Unsaved".to_string()
        } else {
            resource.with(|current| {
                current.as_ref().map_or("New".to_string(), |current| {
                    current.draft.validation_status.clone()
                })
            })
        }
    });
    let has_resource = move || resource.with(Option::is_some);
    let shape = Memo::new(move |_| {
        (
            can_author(console, &project_id.get()),
            matches!(state.get(), EditorState::Ready | EditorState::Saving),
            state.get() == EditorState::Loading,
            state.get() == EditorState::Unavailable,
        )
    });

    move || {
        let (save, act_validate, act_publish, leave) = (save.clone(), act, act, leave.clone());
        match shape.get() {
            (false, ..) => view! { <main class="prompt-editor-page"><h1>{move || title.get()}</h1><p role="status">"Prompt authoring is unavailable."</p></main> }.into_any(),
            (_, _, true, _) => view! { <main class="prompt-editor-page"><p role="status">"Loading prompt editor…"</p></main> }.into_any(),
            (_, _, _, true) => view! { <main class="prompt-editor-page"><p role="status">"This prompt is unavailable."</p></main> }.into_any(),
            (_, false, ..) => view! { <main class="prompt-editor-page"><p role="alert">"The prompt editor could not be loaded."</p></main> }.into_any(),
            _ => view! {
                <main class="prompt-editor-page" aria-labelledby="prompt-editor-title">
                    <PageHeader title_id="prompt-editor-title" title=title status=status>
                        <div class="prompt-editor-actions" aria-label="Prompt actions">
                            <button type="button" on:click=move |_| save() disabled=move || state.get() == EditorState::Saving || name.with(|value| value.trim().is_empty()) || content.with(|value| value.trim().is_empty())>"Save"</button>
                            <button type="button" on:click=move |_| act_validate(false) disabled=move || !has_resource()>"Validate"</button>
                            <button type="button" on:click=move |_| review.set(true) disabled=move || !has_resource()>"Review"</button>
                            <button type="button" on:click=move |_| act_publish(true) disabled=move || dirty.get() || !resource.with(|current| current.as_ref().is_some_and(|current| current.draft.validation_status == "VALID"))>"Publish"</button>
                        </div>
                    </PageHeader>
                    {move || { let text = problem.get(); (!text.is_empty()).then(|| view! { <p class="configuration-problem" role="alert">{text}</p> }) }}
                    <div class="prompt-editor-layout">
                        <section class="prompt-source-panel" aria-label="Prompt source">
                            <label>"Prompt name"<input required prop:value=move || name.get() disabled=has_resource on:input=move |event| name.set(event_target_value(&event)) /></label>
                            <div class="source-preview-split">
                                <div><CodeEditor label="Prompt source" value=content language=Signal::derive(|| "markdown".to_string()) on_change=Callback::new(move |text: String| content.set(text)) described_by="prompt-source-guidance" />
                                    <p id="prompt-source-guidance">"Markdown source is stored as text and validated by the server."</p></div>
                                <SafeMarkdownPreview source=content />
                            </div>
                        </section>
                        <aside class="prompt-inspector" aria-label="Prompt inspector">
                            <section><h2>"Diagnostics"</h2>{move || { let items = resource.with(|current| current.as_ref().map(|current| current.draft.diagnostics()).unwrap_or_default());
                                if items.is_empty() { view! { <p role="status">"No diagnostics."</p> }.into_any() } else { view! { <ul>{items.into_iter().map(|item| view! { <li>{item}</li> }).collect_view()}</ul> }.into_any() } }}</section>
                            <section><h2>"Dependencies"</h2><label>"Typed references"
                                <textarea rows="4" prop:value=move || dependencies.get() on:input=move |event| dependencies.set(event_target_value(&event)) placeholder="model:local-safe-chat@v2" /></label></section>
                            <section><h2>"Usage"</h2><p>{move || resource.with(|current| joined_or(current.as_ref().map_or(&[][..], |current| &current.dependent_resources), "No resources currently use this prompt."))}</p></section>
                            <section><h2>"Versions"</h2><p>{move || resource.with(|current| current.as_ref().map_or(0, |current| current.versions().len()))}" published"</p>
                                {move || resource.with(|current| current.as_ref().and_then(|current| current.current_published_version)).map(|version| view! { <p>"Latest: version "{version}</p> })}</section>
                        </aside>
                    </div>
                    {move || review.get().then(|| view! {
                        <ConfirmationDialog title="Review prompt" on_close=Callback::new(move |()| review.set(false))>
                            <p>"Review the exact source, dependencies, validation state, and usage before publishing."</p>
                            <dl><dt>"Draft"</dt><dd>"r"{resource.with(|current| current.as_ref().map(|current| current.current_draft_revision))}</dd>
                                <dt>"Validation"</dt><dd>{resource.with(|current| current.as_ref().map(|current| current.draft.validation_status.clone()))}</dd>
                                <dt>"Dependencies"</dt><dd>{joined_or(&references(&dependencies.get()), "None")}</dd></dl>
                            <button type="button" on:click=move |_| review.set(false)>"Continue editing"</button>
                        </ConfirmationDialog>
                    })}
                    {move || { let leave = leave.clone(); guard.blocked().then(|| view! {
                        <ConfirmationDialog title="Leave unsaved prompt?" on_close=Callback::new(move |()| guard.reset())>
                            <p>"Your unsaved prompt changes will be lost."</p><div class="prompt-editor-actions">
                                <button type="button" on:click=move |_| guard.reset()>"Keep editing"</button>
                                <button type="button" on:click=move |_| guard.proceed(&leave)>"Discard and leave"</button></div>
                        </ConfirmationDialog>
                    }) }}
                </main>
            }.into_any(),
        }
    }
}

struct ResourceKind {
    code: &'static str,
    title: &'static str,
    singular: &'static str,
    default_content: &'static str,
}

const POLICY: ResourceKind = ResourceKind {
    code: "POLICY",
    title: "Policies",
    singular: "policy",
    default_content: "severity:LOW\nscope:project",
};
const MODEL_PROFILE: ResourceKind = ResourceKind {
    code: "MODEL_PROFILE",
    title: "Model profiles",
    singular: "model profile",
    default_content: "maxTokens:512\nenvironment:DEVELOPMENT",
};

#[component]
pub fn PoliciesPage() -> impl IntoView {
    reusable_resource_page(&POLICY)
}

#[component]
pub fn ModelProfilesPage() -> impl IntoView {
    reusable_resource_page(&MODEL_PROFILE)
}

/// The typed reference of a resource's own published version, which it may not depend on.
fn own_reference(resource: Option<&ReusableResource>) -> String {
    resource
        .and_then(|resource| {
            resource.current_published_version.map(|version| {
                format!(
                    "{}:{}@v{version}",
                    reference_kind(&resource.resource_kind),
                    resource.identity
                )
            })
        })
        .unwrap_or_default()
}

fn reusable_resource_page(kind: &'static ResourceKind) -> impl IntoView {
    let console = use_console();
    let params = use_params_map();
    let project_id = Memo::new(move |_| params.read().get("project_id").unwrap_or_default());
    let resources = RwSignal::new(None::<Vec<ReusableResource>>);
    let options = RwSignal::new(Vec::<ReferenceOption>::new());
    let unavailable = RwSignal::new(false);
    let selected = RwSignal::new(None::<ReusableResource>);
    let error = RwSignal::new(String::new());
    let (name, content) = (
        RwSignal::new(String::new()),
        RwSignal::new(kind.default_content.to_string()),
    );
    let dependencies = RwSignal::new(Vec::<String>::new());
    let author = Memo::new(move |_| can_author(console, &project_id.get()));

    Effect::new(move |_| {
        let project = project_id.get();
        let organization = organization_of(console, &project);
        resources.set(None);
        unavailable.set(false);
        error.set(String::new());
        spawn_local(async move {
            match request_known(&project, &organization).await {
                Ok(known) => {
                    options.set(known.options);
                    let entries: Vec<_> = known
                        .resources
                        .into_iter()
                        .filter(|resource| resource.resource_kind == kind.code)
                        .collect();
                    selected.update(|current| {
                        *current = current.as_ref().and_then(|current| {
                            entries.iter().find(|entry| entry.id == current.id).cloned()
                        })
                    });
                    resources.set(Some(entries));
                }
                Err(_) => {
                    resources.set(Some(Vec::new()));
                    unavailable.set(true);
                    error.set("Configuration could not be loaded.".to_string());
                }
            }
        });
    });

    let apply = move |payload: ConfigurationMutationPayload| {
        if let Some(first) = payload.problems.first() {
            error.set(first.message.clone());
            return;
        }
        let Some(next) = payload.resource else { return };
        resources.update(|all| {
            let list = all.get_or_insert_with(Vec::new);
            list.retain(|entry| entry.id != next.id);
            list.insert(0, next.clone());
        });
        dependencies.set(next.draft.dependencies());
        selected.set(Some(next));
        error.set(String::new());
    };
    let run = move |request: std::pin::Pin<
        Box<
            dyn std::future::Future<
                Output = Result<ConfigurationMutationPayload, crate::graphql::GraphqlError>,
            >,
        >,
    >| {
        spawn_local(async move {
            match request.await {
                Ok(payload) => apply(payload),
                Err(_) => error.set("Configuration could not be saved.".to_string()),
            }
        });
    };
    let create = move |event: ev::SubmitEvent| {
        event.prevent_default();
        run(Box::pin(create_resource(CreateReusableResourceInput {
            project_id: project_id.get_untracked().as_str().into(),
            kind: kind.code.to_string(),
            name: name.get_untracked(),
            content: content.get_untracked(),
            dependencies: dependencies.get_untracked(),
        })));
    };
    let update = move || {
        if let Some(current) = selected.get_untracked() {
            run(Box::pin(update_resource_draft(
                UpdateReusableResourceDraftInput {
                    project_id: project_id.get_untracked().as_str().into(),
                    resource_id: current.id.as_str().into(),
                    expected_revision: current.current_draft_revision,
                    content: content.get_untracked(),
                    dependencies: dependencies.get_untracked(),
                },
            )));
        }
    };
    let revision_action = move |publish: bool| {
        if let Some(current) = selected.get_untracked() {
            let input = ReusableResourceRevisionInput {
                project_id: project_id.get_untracked().as_str().into(),
                resource_id: current.id.as_str().into(),
                expected_revision: current.current_draft_revision,
            };
            if publish {
                run(Box::pin(publish_resource(input)));
            } else {
                run(Box::pin(validate_resource(input)));
            }
        }
    };
    let choose = move |resource: ReusableResource| {
        name.set(resource.name.clone());
        content.set(resource.draft.content.clone());
        dependencies.set(resource.draft.dependencies());
        selected.set(Some(resource));
        error.set(String::new());
    };
    let create_new = move || {
        selected.set(None);
        name.set(String::new());
        content.set(kind.default_content.to_string());
        dependencies.set(if kind.code == "MODEL_PROFILE" {
            options.with_untracked(|all| {
                all.iter()
                    .filter(|option| option.kind == "model")
                    .take(1)
                    .map(|option| option.value.clone())
                    .collect()
            })
        } else {
            Vec::new()
        });
        error.set(String::new());
    };
    let selectable = move || {
        let own = selected.with(|current| own_reference(current.as_ref()));
        options
            .get()
            .into_iter()
            .filter(|option| option.value != own)
            .collect::<Vec<_>>()
    };

    view! {
        <main class="console-page-frame configuration-page" aria-labelledby="resource-title">
            <PageHeader title_id="resource-title" title=kind.title.to_string()
                description="Project drafts and immutable versions are application-managed. The approved local catalog is Git-owned and read-only.">
                {move || (author.get() && !unavailable.get()).then(|| view! { <button class="primary-action" type="button" on:click=move |_| create_new()>"New "{kind.singular}</button> })}
            </PageHeader>
            {move || { let text = error.get(); (!text.is_empty()).then(|| view! { <p class="configuration-problem" role="alert">{text}</p> }) }}
            {move || match resources.get() {
                None => view! { <p role="status">"Loading configuration…"</p> }.into_any(),
                Some(_) if unavailable.get() => view! { <p role="alert">{UNAVAILABLE_ROUTE}</p> }.into_any(),
                Some(list) if list.is_empty() => view! { <p role="status">"No "{kind.title.to_lowercase()}" are available."</p> }.into_any(),
                Some(list) => view! { <section aria-labelledby="resource-list-title"><h2 id="resource-list-title">"Resources"</h2><ul class="configuration-list">
                    {list.into_iter().map(|resource| { let (id, picked) = (resource.id.clone(), resource.clone()); view! {
                        <li><button type="button" on:click=move |_| choose(picked.clone()) aria-pressed=move || selected.with(|current| current.as_ref().is_some_and(|current| current.id == id)).to_string()>
                            {resource.name.clone()}<span>{resource.draft.validation_status.clone()}" · draft r"{resource.current_draft_revision}</span></button></li> } }).collect_view()}
                </ul></section> }.into_any(),
            }}
            {move || (author.get() && !unavailable.get()).then(|| view! {
                <form class="configuration-form" on:submit=create aria-label=move || if selected.with(Option::is_some) { "Reusable resource draft" } else { "Create reusable resource" }>
                    <h2>{move || if selected.with(Option::is_some) { "Draft and publication review" } else { "Create resource" }}</h2>
                    <label>"Name"<input required prop:value=move || selected.with(|current| current.as_ref().map(|current| current.name.clone())).unwrap_or_else(|| name.get())
                        disabled=move || selected.with(Option::is_some) on:input=move |event| name.set(event_target_value(&event)) /></label>
                    <CodeEditor label="Content" value=content language=Signal::derive(|| "text".to_string()) on_change=Callback::new(move |text: String| content.set(text)) />
                    <fieldset class="selection-fieldset"><legend>"Typed dependencies"</legend>
                        {move || { let all = selectable();
                            if all.is_empty() { return view! { <p role="status">"No approved dependencies are available in this project."</p> }.into_any(); }
                            all.into_iter().map(|option| { let (checked, toggled) = (option.value.clone(), option.value.clone()); view! {
                                <label><input type="checkbox" prop:checked=move || dependencies.with(|all| all.contains(&checked))
                                    on:change=move |event| { let (on, value) = (event_target_checked(&event), toggled.clone()); dependencies.update(|all| { all.retain(|entry| *entry != value); if on { all.push(value); all.sort(); } }); } />
                                    <span>{option.label}</span></label> } }).collect_view().into_any() }}
                    </fieldset>
                    {move || match selected.get() {
                        Some(current) => view! {
                            <p><strong>"Draft:"</strong>" r"{current.current_draft_revision}" · "<span role="status">{current.draft.validation_status.clone()}</span>" · "<code>{current.draft.content_digest.clone()}</code></p>
                            <div class="configuration-actions"><button type="button" on:click=move |_| update()>"Save draft"</button>
                                <button type="button" on:click=move |_| revision_action(false)>"Validate draft"</button>
                                <button type="button" on:click=move |_| revision_action(true) disabled=current.draft.validation_status != "VALID">"Publish immutable version"</button></div>
                        }.into_any(),
                        None => view! { <button type="submit" disabled=move || name.with(|value| value.trim().is_empty()) || content.with(|value| value.trim().is_empty())>"Create draft"</button> }.into_any(),
                    }}
                </form>
            })}
            {move || selected.get().map(|current| view! {
                <section aria-labelledby="version-history-title"><h2 id="version-history-title">"Publication review and history"</h2>
                    <p><strong>"Typed ID:"</strong>" "{format!("{}:{}{}", reference_kind(&current.resource_kind), current.identity, current.current_published_version.map_or(String::new(), |version| format!("@v{version}")))}</p>
                    <p><strong>"Dependency usage:"</strong>" "{joined_or(&current.dependent_resources, "No current or published resource depends on this version.")}</p>
                    {if current.versions().is_empty() { view! { <p role="status">"No immutable version has been published."</p> }.into_any() } else { view! {
                        <ul class="configuration-cards">{current.versions().to_vec().into_iter().map(|version| { let dependencies = version.dependencies(); view! {
                            <li><h3>"Version "{version.version}</h3><p><code>{version.content_digest}</code></p><p>"Dependencies: "{joined_or(&dependencies, "None")}</p><p>"Published "{local_time(&version.published_at)}</p></li>
                        } }).collect_view()}</ul> }.into_any() }}
                </section>
            })}
            {move || (!author.get()).then(|| view! { <p role="status">"Read-only: your current capability does not allow authoring."</p> })}
        </main>
    }
}
