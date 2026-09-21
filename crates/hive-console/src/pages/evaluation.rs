use crate::agent_tabs::AgentTabs;
use crate::api::agent_draft::request_agent_versions;
use crate::api::console::has_capability;
use crate::api::deployment::request_deployments;
use crate::api::evaluation::*;
use crate::confirmation_dialog::ConfirmationDialog;
use crate::format::{display_time, encode, random_uuid};
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};
use std::time::Duration;

const NO_DOCUMENT: &str = "Definition content is unavailable for this authority.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Loading,
    Ready,
    Unavailable,
    Error,
}

/// A route parameter that follows the URL.
fn route_param(name: &'static str) -> Memo<String> {
    let params = use_params_map();
    Memo::new(move |_| params.read().get(name).unwrap_or_default())
}

fn can(code: &'static str, project: Memo<String>) -> Memo<bool> {
    let console = use_console();
    Memo::new(move |_| {
        console
            .context
            .with(|context| has_capability(context, code, "PROJECT", &project.get()))
    })
}

/// The three lines every evaluation page shows for its load state.
fn state_lines(
    state: RwSignal<Kind>,
    loading: &'static str,
    unavailable: &'static str,
    error: &'static str,
) -> impl IntoView {
    view! {
        {move || (state.get() == Kind::Loading).then(|| view! { <p role="status">{loading}</p> })}
        {move || (state.get() == Kind::Unavailable).then(|| view! { <p role="status">{unavailable}</p> })}
        {move || (state.get() == Kind::Error).then(|| view! { <p role="alert">{error}</p> })}
    }
}

fn alert(message: RwSignal<String>) -> impl IntoView {
    move || {
        let text = message.get();
        (!text.is_empty()).then(|| view! { <p role="alert">{text}</p> })
    }
}

fn run_rows(project: String, runs: Vec<EvaluationRunSummary>) -> impl IntoView {
    view! { <ul class="evaluation-list">{runs.into_iter().map(|run| { let href = format!("/projects/{project}/evaluations/runs/{}", run.id); view! {
    <li><div><h3><a href=href>"Run "{run.id.clone()}</a></h3>
        <p>{run.target_kind.clone()}" · created "{display_time(Some(&run.created_at))}</p></div><span>{run.lifecycle_status.clone()}</span></li> } }).collect_view()}</ul> }
}

#[component]
pub fn EvaluationListPage() -> impl IntoView {
    let project = route_param("project_id");
    let query = use_query_map();
    let navigate = use_navigate();
    let (can_author, can_view) = (
        can("EVALUATION_DEFINITION.AUTHOR", project),
        can("EVALUATION_DEFINITION.VIEW", project),
    );
    let definitions = RwSignal::new(Vec::<EvaluationDefinitionFields>::new());
    let runs = RwSignal::new(Vec::<EvaluationRunSummary>::new());
    let state = RwSignal::new(Kind::Loading);
    let (slug, message) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let serial = StoredValue::new(0_u32);
    let load = move || {
        let (id, view_definitions) = (project.get_untracked(), can_view.get_untracked());
        let current = serial.get_value() + 1;
        serial.set_value(current);
        spawn_local(async move {
            let found_runs = request_evaluation_runs(&id, None).await;
            let found_definitions = if view_definitions {
                request_evaluation_definitions(&id).await.map(Some)
            } else {
                Ok(None)
            };
            if serial.try_get_value() != Some(current) {
                return;
            }
            match (found_runs, found_definitions) {
                (Err(GraphqlError::Transport(_)), _) | (_, Err(GraphqlError::Transport(_))) => {
                    state.set(Kind::Error)
                }
                (Ok(Some(found_runs)), Ok(found_definitions))
                    if !(view_definitions
                        && found_definitions.as_ref().is_some_and(Option::is_none)) =>
                {
                    runs.set(found_runs.rows);
                    definitions.set(
                        found_definitions
                            .flatten()
                            .map(|page| page.rows)
                            .unwrap_or_default(),
                    );
                    state.set(Kind::Ready);
                }
                _ => state.set(Kind::Unavailable),
            }
        });
    };
    Effect::new(move |_| {
        let _ = (project.get(), can_view.get());
        load();
    });
    let create = move |event: ev::SubmitEvent| {
        event.prevent_default();
        let (id, name) = (
            project.get_untracked(),
            slug.get_untracked().trim().to_string(),
        );
        if !can_author.get_untracked() || name.is_empty() {
            return;
        }
        message.set(String::new());
        let navigate = navigate.clone();
        spawn_local(async move {
            match create_evaluation_definition(CreateEvaluationDefinitionInput {
                project_id: id.as_str().into(),
                slug: name,
                document: None,
                idempotency_key: random_uuid(),
            })
            .await
            {
                Err(_) => message.set("Evaluation definitions are unavailable.".to_string()),
                Ok(value) => match (value.problem(), value.definition) {
                    (Some(problem), _) => message.set(problem),
                    (None, Some(definition)) if definition.project_id == id => navigate(
                        &format!("/projects/{id}/evaluations/definitions/{}", definition.id),
                        Default::default(),
                    ),
                    _ => message.set("The created definition is unavailable.".to_string()),
                },
            }
        });
    };
    let hint = Memo::new(move |_| {
        query
            .read()
            .get("deploymentId")
            .filter(|value| !value.is_empty())
    });
    view! {
        <main class="evaluation-page" aria-labelledby="evaluations-title">
            <PageHeader title_id="evaluations-title" title="Evaluations".to_string()><button type="button" on:click=move |_| load()>"Refresh evaluations"</button></PageHeader>
            {move || hint.get().map(|hint| view! { <p role="status">"Select a definition to review deployment "{hint}". The server resolves the frozen target when you queue the run."</p> })}
            {state_lines(state, "Loading evaluation definitions and runs…", "Evaluations are unavailable.", "We could not refresh evaluations. Refresh before using a displayed value.")}
            {alert(message)}
            {move || { let create = create.clone(); can_author.get().then(|| view! { <form class="evaluation-create" on:submit=create>
                <label>"Definition slug"<input aria-label="Evaluation definition slug" prop:value=move || slug.get() on:input=move |event| slug.set(event_target_value(&event)) required /></label>
                <button type="submit">"Create definition"</button></form> }) }}
            {move || (!can_author.get() && state.get() == Kind::Ready).then(|| view! { <p role="status">"Your current project capabilities do not permit evaluation definition authoring."</p> })}
            <section aria-labelledby="evaluation-definitions-title"><h2 id="evaluation-definitions-title">"Definitions"</h2>
                {move || { let (rows, id, hint) = (definitions.get(), project.get(), hint.get()); if rows.is_empty() { view! { <p role="status">"No evaluation definitions are available for this project."</p> }.into_any() } else { view! {
                    <ul class="evaluation-list">{rows.into_iter().map(|definition| view! {
                        <li><div><h3><a href=format!("/projects/{id}/evaluations/definitions/{}{}", definition.id, hint.as_ref().map_or(String::new(), |hint| format!("?deploymentId={}", encode(hint))))>{definition.slug}</a></h3>
                            <p>"Draft revision "{definition.draft.revision}" · "{definition.draft.validation_status}</p></div>
                            <span>{definition.latest_version.map_or("No published version".to_string(), |version| format!("Published version {}", version.version_number))}</span></li> }).collect_view()}</ul> }.into_any() } }}</section>
            <section aria-labelledby="evaluation-runs-title"><h2 id="evaluation-runs-title">"Runs"</h2>
                {move || { let rows = runs.get(); if rows.is_empty() { view! { <p role="status">"No evaluation runs are available for this project."</p> }.into_any() } else { run_rows(project.get(), rows).into_any() } }}</section>
        </main>
    }
}

fn target_key(target: &EvaluationTarget) -> String {
    format!(
        "{}:{}:{}",
        target.target_kind, target.target_id, target.environment_definition_version_id
    )
}

#[component]
pub fn EvaluationDefinitionPage() -> impl IntoView {
    let (project, definition_id) = (route_param("project_id"), route_param("definition_id"));
    let query = use_query_map();
    let navigate = use_navigate();
    let (can_author, can_publish, can_run) = (
        can("EVALUATION_DEFINITION.AUTHOR", project),
        can("EVALUATION_DEFINITION.PUBLISH", project),
        can("EVALUATION_RUN.RUN", project),
    );
    let definition = RwSignal::new(None::<EvaluationDefinitionFields>);
    let document = RwSignal::new(String::new());
    let targets = RwSignal::new(None::<Vec<EvaluationTarget>>);
    let selected = RwSignal::new(String::new());
    let state = RwSignal::new(Kind::Loading);
    let message = RwSignal::new(String::new());
    let hint = Memo::new(move |_| {
        query
            .read()
            .get("deploymentId")
            .filter(|value| !value.is_empty())
    });
    let is_hinted = move |target: &EvaluationTarget, hint: &str| {
        target.kind() == Some(EvaluationTargetKind::Deployment) && target.target_id == hint
    };
    let serial = StoredValue::new(0_u32);
    let load = move || {
        let (project_id, id, hinted) = (
            project.get_untracked(),
            definition_id.get_untracked(),
            hint.get_untracked(),
        );
        let current = serial.get_value() + 1;
        serial.set_value(current);
        spawn_local(async move {
            let value = match request_evaluation_definition(&id).await {
                _ if serial.try_get_value() != Some(current) => return,
                Err(GraphqlError::Transport(_)) => {
                    state.set(Kind::Error);
                    return;
                }
                Ok(Some(value)) if value.project_id == project_id => value,
                _ => {
                    state.set(Kind::Unavailable);
                    return;
                }
            };
            document.set(value.draft.canonical_document.clone());
            let version = value
                .latest_version
                .as_ref()
                .map(|version| version.id.clone());
            definition.set(Some(value));
            state.set(Kind::Ready);
            let Some(version) = version else { return };
            match request_evaluation_targets(&project_id, &version).await {
                _ if serial.try_get_value() != Some(current) => {}
                Err(GraphqlError::Transport(_)) => state.set(Kind::Error),
                Ok(Some(rows)) => {
                    let preferred = hinted
                        .and_then(|hint| rows.iter().find(|target| is_hinted(target, &hint)))
                        .or(rows.first())
                        .map_or_else(|| "::".to_string(), target_key);
                    selected.update(|previous| {
                        if previous.is_empty() {
                            *previous = preferred;
                        }
                    });
                    targets.set(Some(rows));
                }
                _ => {}
            }
        });
    };
    Effect::new(move |_| {
        let _ = (project.get(), definition_id.get(), hint.get());
        load();
    });

    // Runs one draft command; `done` receives the payload when the service accepted it.
    let command = move |request: std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EvaluationMutationPayload, GraphqlError>>>,
    >,
                        unavailable: &'static str,
                        done: Box<dyn FnOnce(EvaluationMutationPayload)>| {
        message.set(String::new());
        spawn_local(async move {
            match request.await {
                Err(_) => message.set(unavailable.to_string()),
                Ok(value) => match value.problem() {
                    Some(problem) => message.set(problem),
                    None => done(value),
                },
            }
        });
    };
    // The command payload is the command tier's own shape; the page re-reads the generated
    // definition after it so every displayed field comes from one read.
    let adopt = move |value: EvaluationMutationPayload| {
        if let Some(next) = value.definition {
            document.set(next.draft.canonical_document.clone());
        }
        load();
    };
    let save = move |_| {
        let Some(current) = definition
            .get_untracked()
            .filter(|_| can_author.get_untracked())
        else {
            return;
        };
        command(
            Box::pin(update_evaluation_definition_draft(
                UpdateEvaluationDefinitionDraftInput {
                    definition_id: current.id.as_str().into(),
                    expected_revision: current.draft.revision.into(),
                    document: document.get_untracked(),
                    idempotency_key: random_uuid(),
                },
            )),
            "Evaluation definitions are unavailable.",
            Box::new(adopt),
        );
    };
    let validate = move |_| {
        let Some(current) = definition
            .get_untracked()
            .filter(|_| can_author.get_untracked())
        else {
            return;
        };
        command(
            Box::pin(validate_evaluation_definition_draft(
                ValidateEvaluationDefinitionDraftInput {
                    definition_id: current.id.as_str().into(),
                    expected_revision: current.draft.revision.into(),
                    idempotency_key: random_uuid(),
                },
            )),
            "Evaluation definitions are unavailable.",
            Box::new(move |_| load()),
        );
    };
    let duplicate = {
        let navigate = navigate.clone();
        move |_| {
            let Some(current) = definition
                .get_untracked()
                .filter(|_| can_author.get_untracked())
            else {
                return;
            };
            let Some(version) = current.latest_version else {
                return;
            };
            let (navigate, project_id) = (navigate.clone(), project.get_untracked());
            command(
                Box::pin(duplicate_evaluation_definition_version_to_draft(
                    DuplicateEvaluationDefinitionVersionToDraftInput {
                        version_id: version.id.as_str().into(),
                        expected_revision: current.draft.revision.into(),
                        idempotency_key: random_uuid(),
                    },
                )),
                "Evaluation definitions are unavailable.",
                Box::new(move |value| {
                    if let Some(next) = value.definition {
                        navigate(
                            &format!("/projects/{project_id}/evaluations/definitions/{}", next.id),
                            Default::default(),
                        );
                    }
                }),
            );
        }
    };
    let start = {
        let navigate = navigate.clone();
        move |_| {
            let Some(version) = definition
                .get_untracked()
                .and_then(|current| current.latest_version)
                .filter(|_| can_run.get_untracked())
            else {
                return;
            };
            let hinted = hint.get_untracked();
            let target = targets
                .with_untracked(|rows| {
                    rows.as_ref().and_then(|rows| {
                        rows.iter()
                            .find(|target| target_key(target) == selected.get_untracked())
                            .cloned()
                    })
                })
                .filter(|target| hinted.as_ref().is_none_or(|hint| is_hinted(target, hint)))
                .and_then(|target| target.kind().map(|kind| (kind, target)));
            let Some((kind, target)) = target else {
                message.set(
                    "Select the compatible immutable deployment target for this review."
                        .to_string(),
                );
                return;
            };
            let (navigate, project_id) = (navigate.clone(), project.get_untracked());
            command(
                Box::pin(run_evaluation(RunEvaluationInput {
                    project_id: project_id.as_str().into(),
                    definition_version_id: version.id.as_str().into(),
                    target_kind: kind,
                    target_id: target.target_id.as_str().into(),
                    environment_definition_version_id: target
                        .environment_definition_version_id
                        .as_str()
                        .into(),
                    idempotency_key: random_uuid(),
                })),
                "Evaluation runs are unavailable.",
                Box::new(move |value| {
                    if let Some(run) = value.run.filter(|run| run.project_id == project_id) {
                        navigate(
                            &format!("/projects/{project_id}/evaluations/runs/{}", run.id),
                            Default::default(),
                        );
                    }
                }),
            );
        }
    };
    // A Memo, because setting `state` to the value it already holds still notifies, and the page must not remount on every reload.
    let unavailable = Memo::new(move |_| state.get() == Kind::Unavailable);
    move || {
        if unavailable.get() {
            return view! { <main class="evaluation-page"><p role="status">"This evaluation definition is unavailable."</p></main> }.into_any();
        }
        let (duplicate, start) = (duplicate.clone(), start.clone());
        view! {
            <main class="evaluation-page" aria-labelledby="evaluation-definition-title">
                {state_lines(state, "Loading evaluation definition…", "", "We could not load this evaluation definition.")}
                {alert(message)}
                {move || definition.with(Option::is_some).then(|| { let (duplicate, start) = (duplicate.clone(), start.clone()); let (project_id, id) = (project.get_untracked(), definition_id.get_untracked());
                    let latest = Memo::new(move |_| definition.with(|current| current.as_ref().and_then(|current| current.latest_version.clone())));
                    view! {
                    <PageHeader title_id="evaluation-definition-title" title=Signal::derive(move || definition.with(|current| current.as_ref().map_or(String::new(), |current| current.slug.clone())))
                        meta=Signal::derive(move || definition.with(|current| current.as_ref().map_or(String::new(), |current| format!("Draft revision {} · {}", current.draft.revision, current.draft.validation_status))))>
                        <button type="button" on:click=move |_| load()>"Refresh definition"</button></PageHeader>
                    <label class="evaluation-document">"Canonical evaluation document"<textarea aria-label="Canonical evaluation document" prop:value=move || document.get() disabled=move || !can_author.get() on:input=move |event| document.set(event_target_value(&event)) /></label>
                    {move || definition.with(|current| current.as_ref().map(|current| current.draft.diagnostics.clone())).filter(|list| !list.is_empty()).map(|list| view! {
                        <section aria-labelledby="evaluation-diagnostics-title"><h2 id="evaluation-diagnostics-title">"Validation diagnostics"</h2>
                            <ul>{list.into_iter().map(|diagnostic| view! { <li><code>{diagnostic.code}</code>" "{diagnostic.message}</li> }).collect_view()}</ul></section> })}
                    <div class="evaluation-actions">
                        {move || can_author.get().then(|| view! { <button type="button" on:click=save>"Save draft"</button><button type="button" on:click=validate>"Validate draft"</button> })}
                        {move || can_publish.get().then(|| view! { <a href=format!("/projects/{}/evaluations/definitions/{}/review", project.get(), definition_id.get())>"Review draft before publication"</a> })}
                        {move || { let duplicate = duplicate.clone(); (latest.with(Option::is_some) && can_author.get()).then(|| view! { <button type="button" on:click=duplicate>"Copy published version to draft"</button> }) }}
                    </div>
                    <p><a href=format!("/projects/{project_id}/evaluations/definitions/{id}/versions")>"View immutable history"</a></p>
                    <section aria-labelledby="evaluation-run-title"><h2 id="evaluation-run-title">"Run immutable version"</h2>
                        {move || { let start = start.clone(); match latest.get() {
                            None => view! { <p role="status">"Publish a valid immutable version before queuing an evaluation."</p> }.into_any(),
                            Some(version) => view! {
                                <p>"Version "{version.version_number}" · "<code>{version.content_digest}</code></p>
                                {move || hint.get().map(|hint| view! { <p role="status">"This review retains deployment "{hint}" as route context. The service resolves the exact frozen target before queueing."</p> })}
                                <label>"Exact immutable target"<select aria-label="Exact immutable target" prop:value=move || selected.get() disabled=move || !can_run.get() || targets.with(|rows| rows.as_ref().is_none_or(Vec::is_empty))
                                    on:change=move |event| selected.set(event_target_value(&event))>
                                    {move || { let hinted = hint.get(); targets.get().map(|rows| rows.into_iter().map(|target| { let key = target_key(&target); let chosen = key.clone(); view! {
                                        <option value=key selected=move || selected.get() == chosen disabled=hinted.as_ref().is_some_and(|hint| !is_hinted(&target, hint))>{target.display_name.clone()}" · "{target.logical_environment_class.clone()}</option> } }).collect_view()) }}</select></label>
                                {move || targets.with(|rows| rows.as_ref().is_none_or(Vec::is_empty)).then(|| view! { <p role="status">"No compatible immutable target is available."</p> })}
                                <button type="button" disabled=move || !can_run.get() || selected.with(String::is_empty) on:click=start>"Queue evaluation"</button> }.into_any(),
                        } }}</section>
                    <p><a href=format!("/projects/{project_id}/evaluations")>"Back to evaluations"</a></p> } })}
            </main>
        }.into_any()
    }
}

#[component]
pub fn EvaluationVersionHistoryPage() -> impl IntoView {
    let (project, definition_id) = (route_param("project_id"), route_param("definition_id"));
    let state = RwSignal::new(Kind::Loading);
    let versions = RwSignal::new(None::<Page<EvaluationVersionFields>>);
    let load = move |number: i32| {
        let id = definition_id.get_untracked();
        spawn_local(async move {
            let appending = number > 0;
            match request_evaluation_version_history(&id, number).await {
                Err(GraphqlError::Transport(_)) => state.set(Kind::Error),
                Ok(Some(page)) => {
                    versions.update(|shown| match shown {
                        Some(shown) if appending => shown.extend(page),
                        _ => *shown = Some(page),
                    });
                    state.set(Kind::Ready);
                }
                _ => state.set(Kind::Unavailable),
            }
        });
    };
    Effect::new(move |_| {
        let _ = definition_id.get();
        load(0);
    });
    let base = move || {
        format!(
            "/projects/{}/evaluations/definitions/{}",
            project.get(),
            definition_id.get()
        )
    };
    view! {
        <main class="evaluation-page" aria-labelledby="evaluation-version-history-title">
            <PageHeader title_id="evaluation-version-history-title" title="Immutable evaluation versions".to_string() />
            {state_lines(state, "Loading immutable version history…", "This version history is unavailable.", "We could not load immutable version history.")}
            {move || (state.get() == Kind::Ready).then(|| { let page = versions.get().unwrap_or_else(|| Page::new(Vec::new(), None));
                let compare = (page.rows.len() > 1).then(|| format!("{}/versions/compare?left={}&right={}", base(), page.rows[0].id, page.rows[1].id));
                let more = page.next_page();
                view! {
                    <p>{if page.rows.is_empty() { "No immutable version is available." } else { "Each row is an immutable publication fact." }}</p>
                    <ul class="evaluation-list">{page.rows.into_iter().map(|version| view! { <li><a href=format!("{}/versions/{}", base(), version.id)>"Version "{version.version_number}</a><code>{version.content_digest}</code></li> }).collect_view()}</ul>
                    {more.map(|number| view! { <button type="button" on:click=move |_| load(number)>"Load more immutable versions"</button> })}
                    {compare.map(|href| view! { <p><a href=href>"Compare the two latest immutable versions"</a></p> })} } })}
            <p><a href=base>"Back to evaluation definition"</a></p>
        </main>
    }
}

#[component]
pub fn EvaluationPublicationReviewPage() -> impl IntoView {
    let (project, definition_id) = (route_param("project_id"), route_param("definition_id"));
    let navigate = use_navigate();
    let can_publish = can("EVALUATION_DEFINITION.PUBLISH", project);
    let definition = RwSignal::new(None::<EvaluationDefinitionFields>);
    let (message, state) = (RwSignal::new(String::new()), RwSignal::new(Kind::Loading));
    Effect::new(move |_| {
        let (project_id, id) = (project.get(), definition_id.get());
        spawn_local(async move {
            match request_evaluation_definition(&id).await {
                Err(GraphqlError::Transport(_)) => state.set(Kind::Error),
                Ok(Some(value)) if value.project_id == project_id => {
                    definition.set(Some(value));
                    state.set(Kind::Ready);
                }
                _ => state.set(Kind::Unavailable),
            }
        });
    });
    let publish = move |_| {
        let Some(current) = definition
            .get_untracked()
            .filter(|_| can_publish.get_untracked())
        else {
            return;
        };
        let (navigate, project_id) = (navigate.clone(), project.get_untracked());
        spawn_local(async move {
            match publish_evaluation_definition_draft(PublishEvaluationDefinitionDraftInput {
                definition_id: current.id.as_str().into(),
                expected_revision: current.draft.revision.into(),
                idempotency_key: random_uuid(),
            })
            .await
            {
                Err(_) => message.set("The publication command is unavailable.".to_string()),
                Ok(value) => match value.problem() {
                    Some(problem) => message.set(problem),
                    None => navigate(
                        &format!(
                            "/projects/{project_id}/evaluations/definitions/{}/versions",
                            current.id
                        ),
                        Default::default(),
                    ),
                },
            }
        });
    };
    view! {
        <main class="evaluation-page" aria-labelledby="evaluation-publication-review-title">
            <PageHeader title_id="evaluation-publication-review-title" title="Review evaluation publication".to_string() />
            {state_lines(state, "Loading the publication review…", "This publication review is unavailable.", "We could not load this publication review.")}
            {alert(message)}
            {move || definition.get().map(|current| { let publish = publish.clone(); let valid = current.draft.validation_status == "VALID"; let back = format!("/projects/{}/evaluations/definitions/{}", project.get_untracked(), current.id); view! {
                <p>"The service publishes the displayed canonical draft as one immutable version after validation."</p>
                <dl><dt>"Draft revision"</dt><dd>{current.draft.revision}</dd><dt>"Validation status"</dt><dd>{current.draft.validation_status.clone()}</dd>
                    <dt>"Previous immutable version"</dt><dd>{current.latest_version.as_ref().map_or("None".to_string(), |version| format!("Version {}", version.version_number))}</dd></dl>
                <pre aria-label="Publication review document">{current.draft.canonical_document.clone()}</pre>
                {can_publish.get().then(|| view! { <ConfirmationDialog title="Publish immutable evaluation version" on_close=Callback::new(|()| ())>
                    <p>"Publishing records an immutable version and retains the draft for later revision."</p>
                    <button type="button" disabled=!valid on:click=publish>"Confirm publication"</button><a href=back>"Return to draft"</a></ConfirmationDialog> })} } })}
        </main>
    }
}

#[component]
pub fn EvaluationVersionUsagePage() -> impl IntoView {
    let (project, definition_id, version_id) = (
        route_param("project_id"),
        route_param("definition_id"),
        route_param("version_id"),
    );
    let state = RwSignal::new(Kind::Loading);
    let runs = RwSignal::new(None::<Page<EvaluationRunSummary>>);
    let load = move |number: i32| {
        let id = version_id.get_untracked();
        spawn_local(async move {
            let appending = number > 0;
            match request_evaluation_version_usage(&id, number).await {
                Err(GraphqlError::Transport(_)) => state.set(Kind::Error),
                Ok(Some(page)) => {
                    runs.update(|shown| match shown {
                        Some(shown) if appending => shown.extend(page),
                        _ => *shown = Some(page),
                    });
                    state.set(Kind::Ready);
                }
                _ => state.set(Kind::Unavailable),
            }
        });
    };
    Effect::new(move |_| {
        let _ = version_id.get();
        load(0);
    });
    view! {
        <main class="evaluation-page" aria-labelledby="evaluation-version-usage-title">
            <PageHeader title_id="evaluation-version-usage-title" title="Evaluation version usage".to_string() />
            {state_lines(state, "Loading bounded run usage…", "This version usage is unavailable.", "We could not load this version usage.")}
            {move || (state.get() == Kind::Ready).then(|| runs.get()).flatten().map(|page| { let more = page.next_page(); let id = project.get(); view! {
                <ul class="evaluation-list">{page.rows.into_iter().map(|run| { let href = format!("/projects/{id}/evaluations/runs/{}", run.id); view! { <li><a href=href>"Run "{run.id.clone()}</a><span>{run.lifecycle_status.clone()}</span></li> } }).collect_view()}</ul>
                {more.map(|number| view! { <button type="button" on:click=move |_| load(number)>"Load more run usage"</button> })} } })}
            <p><a href=move || format!("/projects/{}/evaluations/definitions/{}/versions/{}", project.get(), definition_id.get(), version_id.get())>"Back to immutable version"</a></p>
        </main>
    }
}

#[component]
pub fn EvaluationVersionDetailPage() -> impl IntoView {
    let (project, definition_id, version_id) = (
        route_param("project_id"),
        route_param("definition_id"),
        route_param("version_id"),
    );
    let can_author = can("EVALUATION_DEFINITION.AUTHOR", project);
    let version = RwSignal::new(None::<EvaluationVersionFields>);
    let (state, dialog, message) = (
        RwSignal::new(Kind::Loading),
        RwSignal::new(false),
        RwSignal::new(String::new()),
    );
    Effect::new(move |_| {
        let (definition, id) = (definition_id.get(), version_id.get());
        spawn_local(async move {
            match request_evaluation_version(&id).await {
                Err(GraphqlError::Transport(_)) => state.set(Kind::Error),
                Ok(Some(found)) if found.definition_id == definition => {
                    version.set(Some(found));
                    state.set(Kind::Ready);
                }
                _ => state.set(Kind::Unavailable),
            }
        });
    });
    let duplicate = move |_| {
        let Some(current) = version.get_untracked() else {
            return;
        };
        let definition = definition_id.get_untracked();
        spawn_local(async move {
            let Ok(Some(draft)) = request_evaluation_definition(&definition).await else {
                message.set("The current draft is unavailable.".to_string());
                return;
            };
            match duplicate_evaluation_definition_version_to_draft(
                DuplicateEvaluationDefinitionVersionToDraftInput {
                    version_id: current.id.as_str().into(),
                    expected_revision: draft.draft.revision.into(),
                    idempotency_key: random_uuid(),
                },
            )
            .await
            {
                Err(_) => message.set("The duplication command is unavailable.".to_string()),
                Ok(value) => match value.problem() {
                    Some(problem) => message.set(problem),
                    None => dialog.set(false),
                },
            }
        });
    };
    let base = move || {
        format!(
            "/projects/{}/evaluations/definitions/{}/versions",
            project.get(),
            definition_id.get()
        )
    };
    view! {
        <main class="evaluation-page" aria-labelledby="evaluation-version-detail-title">
            <PageHeader title_id="evaluation-version-detail-title" title="Immutable evaluation version".to_string() />
            {state_lines(state, "Loading immutable version…", "This immutable version is unavailable.", "We could not load this immutable version.")}
            {alert(message)}
            {move || version.get().map(|current| view! {
                <p>"Version "{current.version_number}" · "<code>{current.content_digest}</code></p>
                <pre aria-label="Read-only evaluation definition">{if current.canonical_document.is_empty() { NO_DOCUMENT.to_string() } else { current.canonical_document.clone() }}</pre>
                <p><a href=format!("{}/{}/usage", base(), current.id)>"View bounded run usage"</a></p>
                {can_author.get().then(|| view! { <button type="button" on:click=move |_| dialog.set(true)>"Copy this immutable version to draft"</button> })} })}
            <p><a href=base>"Back to immutable history"</a></p>
            {move || dialog.get().then(|| view! { <ConfirmationDialog title="Copy immutable version to draft" on_close=Callback::new(move |()| dialog.set(false))>
                <p>"The service writes a new draft revision from this selected immutable version."</p>
                <button type="button" on:click=duplicate>"Confirm copy"</button><button type="button" on:click=move |_| dialog.set(false)>"Keep current draft"</button></ConfirmationDialog> })}
        </main>
    }
}

#[component]
pub fn EvaluationVersionComparisonPage() -> impl IntoView {
    let (project, definition_id) = (route_param("project_id"), route_param("definition_id"));
    let query = use_query_map();
    let sides = Memo::new(move |_| {
        let read = query.read();
        read.get("left")
            .filter(|id| !id.is_empty())
            .zip(read.get("right").filter(|id| !id.is_empty()))
    });
    let state = RwSignal::new(Kind::Loading);
    let comparison = RwSignal::new(None::<(EvaluationVersionFields, EvaluationVersionFields)>);
    Effect::new(move |_| {
        let Some((left, right)) = sides.get() else {
            state.set(Kind::Unavailable);
            return;
        };
        spawn_local(async move {
            match request_evaluation_version_comparison(&left, &right).await {
                Err(GraphqlError::Transport(_)) => state.set(Kind::Error),
                Ok(Some(found)) => {
                    comparison.set(Some(found));
                    state.set(Kind::Ready);
                }
                _ => state.set(Kind::Unavailable),
            }
        });
    });
    let side = |version: EvaluationVersionFields| view! { <article><h2>"Version "{version.version_number}</h2><pre>{if version.canonical_document.is_empty() { NO_DOCUMENT.to_string() } else { version.canonical_document }}</pre></article> };
    view! {
        <main class="evaluation-page" aria-labelledby="evaluation-version-comparison-title">
            <PageHeader title_id="evaluation-version-comparison-title" title="Immutable evaluation version comparison".to_string() />
            {state_lines(state, "Loading immutable version comparison…", "The selected immutable versions are unavailable.", "We could not compare these immutable versions.")}
            {move || comparison.get().map(|(left, right)| view! { <section aria-label="Read-only evaluation version comparison">{side(left)}{side(right)}</section> })}
            <p><a href=move || format!("/projects/{}/evaluations/definitions/{}/versions", project.get(), definition_id.get())>"Back to immutable history"</a></p>
        </main>
    }
}

#[component]
pub fn EvaluationRunPage() -> impl IntoView {
    let (project, run_id) = (route_param("project_id"), route_param("run_id"));
    let navigate = use_navigate();
    let run = RwSignal::new(None::<EvaluationRunDetail>);
    let (state, message, cancel_dialog) = (
        RwSignal::new(Kind::Loading),
        RwSignal::new(String::new()),
        RwSignal::new(false),
    );
    // Which run actions are offered is the server's answer, taken against the run's lifecycle and
    // this principal's capabilities; the page only renders it.
    let offered = move |available: fn(&EvaluationRunSummary) -> bool| {
        move || run.with(|run| run.as_ref().is_some_and(|run| available(&run.summary)))
    };
    let can_cancel = offered(|run| run.can_cancel);
    let can_rerun = offered(|run| run.can_rerun);
    let generation = StoredValue::new(0_u32);
    let load = Callback::new(move |done: Callback<()>| {
        let (project_id, id) = (project.get_untracked(), run_id.get_untracked());
        let current = generation.get_value() + 1;
        generation.set_value(current);
        spawn_local(async move {
            let result = request_evaluation_run(&id).await;
            if generation.try_get_value() == Some(current) {
                match result {
                    Err(GraphqlError::Transport(_)) => state.set(Kind::Error),
                    Ok(Some(value)) if value.summary.project_id == project_id => {
                        run.set(Some(value));
                        state.set(Kind::Ready);
                    }
                    _ => state.set(Kind::Unavailable),
                }
            }
            done.run(());
        });
    });
    let nothing = Callback::new(|()| ());
    Effect::new(move |_| {
        let _ = (project.get(), run_id.get());
        load.run(nothing);
    });

    // While the run is active the visible, online page polls every three seconds.
    let timer = StoredValue::new(None::<TimeoutHandle>);
    let clear_timer = move || {
        if let Some(handle) = timer.try_update_value(Option::take).flatten() {
            handle.clear();
        }
    };
    let active =
        Memo::new(move |_| run.with(|run| run.as_ref().is_some_and(|run| !run.summary.terminal)));
    let visible_online = || {
        document().visibility_state() == web_sys::VisibilityState::Visible
            && window().navigator().on_line()
    };
    let schedule = StoredValue::new(None::<Callback<()>>);
    schedule.set_value(Some(Callback::new(move |()| {
        clear_timer();
        if !active.try_get_untracked().unwrap_or(false) || !visible_online() {
            return;
        }
        let again = schedule.get_value().unwrap_or(nothing);
        timer.set_value(
            set_timeout_with_handle(move || load.run(again), Duration::from_secs(3)).ok(),
        );
    })));
    let again = move || schedule.try_get_value().flatten().unwrap_or(nothing);
    Effect::new(move |_| {
        let _ = active.get();
        again().run(());
    });
    let refresh = move || {
        if active.get_untracked() && visible_online() {
            load.run(again());
        }
    };
    let on_focus = window_event_listener(ev::focus, move |_| refresh());
    let on_online = window_event_listener(ev::online, move |_| refresh());
    crate::dom::on_document_event("visibilitychange", refresh);
    on_cleanup(move || {
        clear_timer();
        on_focus.remove();
        on_online.remove();
    });

    let cancel = move |_| {
        let Some(current) = run.get_untracked().filter(|_| can_cancel()) else {
            return;
        };
        message.set(String::new());
        spawn_local(async move {
            match cancel_evaluation(CancelEvaluationInput {
                run_id: current.summary.id.as_str().into(),
                expected_generation: current.summary.generation.into(),
                idempotency_key: random_uuid(),
                reason: Some("Canceled from the local evaluation detail.".to_string()),
            })
            .await
            {
                Err(_) => message.set("Evaluation runs are unavailable.".to_string()),
                Ok(value) => match value.problem() {
                    Some(problem) => message.set(problem),
                    None => {
                        if value.run.is_some() {
                            cancel_dialog.set(false);
                            load.run(nothing);
                        }
                    }
                },
            }
        });
    };
    let rerun = move |_| {
        let Some(current) = run.get_untracked().filter(|_| can_rerun()) else {
            return;
        };
        message.set(String::new());
        let (navigate, project_id) = (navigate.clone(), project.get_untracked());
        spawn_local(async move {
            match rerun_evaluation(RerunEvaluationInput {
                run_id: current.summary.id.as_str().into(),
                idempotency_key: random_uuid(),
            })
            .await
            {
                Err(_) => message.set("Evaluation runs are unavailable.".to_string()),
                Ok(value) => match (value.problem(), value.run) {
                    (Some(problem), _) => message.set(problem),
                    (None, Some(next)) if next.project_id == project_id => navigate(
                        &format!("/projects/{project_id}/evaluations/runs/{}", next.id),
                        Default::default(),
                    ),
                    _ => {}
                },
            }
        });
    };
    /// Loads the next page of one fact list and appends it to the displayed run.
    macro_rules! more {
        ($field:ident, $request:ident, $unavailable:literal) => {
            move |_| {
                let Some((id, number)) = run.with_untracked(|run| {
                    run.as_ref().and_then(|run| {
                        run.$field
                            .next_page()
                            .map(|number| (run.summary.id.clone(), number))
                    })
                }) else {
                    return;
                };
                spawn_local(async move {
                    match $request(&id, number).await {
                        Ok(Some(next)) => run.update(|run| {
                            if let Some(run) = run {
                                run.$field.extend(next);
                            }
                        }),
                        _ => message.set($unavailable.to_string()),
                    }
                });
            }
        };
    }
    let more_cases = more!(
        cases,
        request_evaluation_run_cases,
        "Case facts are unavailable."
    );
    let more_metrics = more!(
        metrics,
        request_evaluation_run_metrics,
        "Metric facts are unavailable."
    );
    let more_artifacts = more!(
        artifacts,
        request_evaluation_run_artifacts,
        "Artifact facts are unavailable."
    );
    let more_audit = more!(
        audit,
        request_evaluation_run_audit,
        "Audit facts are unavailable."
    );

    // A Memo, because setting `state` to the value it already holds still notifies, and the page must not remount on every reload.
    let unavailable = Memo::new(move |_| state.get() == Kind::Unavailable);
    move || {
        if unavailable.get() {
            return view! { <main class="evaluation-page"><p role="status">"This evaluation run is unavailable."</p></main> }.into_any();
        }
        let rerun = rerun.clone();
        view! {
            <main class="evaluation-page" aria-labelledby="evaluation-run-detail-title">
                {state_lines(state, "Loading evaluation run…", "", "We could not refresh this evaluation run.")}
                {alert(message)}
                {move || run.with(Option::is_some).then(|| { let rerun = rerun.clone(); let project_id = project.get_untracked(); view! {
                    <PageHeader title_id="evaluation-run-detail-title" title="Evaluation run".to_string()
                        meta=Signal::derive(move || run.with(|run| run.as_ref().map_or(String::new(), |run| format!("{}{}", run.summary.lifecycle_status, run.summary.outcome_category.clone().map_or(String::new(), |outcome| format!(" · {outcome}"))))))>
                        <button type="button" on:click=move |_| load.run(nothing)>"Refresh run"</button>
                        {move || can_cancel().then(|| view! { <button type="button" on:click=move |_| cancel_dialog.set(true)>"Cancel evaluation"</button> })}
                        {move || { let rerun = rerun.clone(); can_rerun().then(|| view! { <button type="button" on:click=rerun>"Rerun immutable target"</button> }) }}
                    </PageHeader>
                    <p role="status">{move || if active.get() { "This visible page polls the bounded run projection while local execution remains active." } else { "This evaluation run reached a terminal state." }}</p>
                    {move || run.get().map(|run| { let summary = run.summary;
                        let (more_cases_page, more_metrics_page, more_artifacts_page, more_audit_page) =
                            (run.cases.next_page(), run.metrics.next_page(), run.artifacts.next_page(), run.audit.next_page());
                        let audit_href = format!("/projects/{project_id}/audit?resourceType=EVALUATION_RUN&resourceId={}", encode(&summary.id)); view! {
                        <section class="evaluation-facts" aria-labelledby="evaluation-run-facts-title"><h2 id="evaluation-run-facts-title">"Frozen target and environment"</h2><dl>
                            <dt>"Target kind"</dt><dd>{summary.target_kind}</dd><dt>"Target ID"</dt><dd><code>{summary.target_id}</code></dd>
                            <dt>"Definition version"</dt><dd><code>{summary.definition_version_id}</code></dd><dt>"Environment version"</dt><dd><code>{summary.environment_definition_version_id}</code></dd>
                            <dt>"Duration"</dt><dd>{summary.duration_millis.map_or("Not recorded".to_string(), |millis| format!("{millis} milliseconds"))}</dd>
                            <dt>"Failure summary"</dt><dd>{summary.failure_summary.unwrap_or_else(|| "Not recorded".to_string())}</dd>
                            <dt>"Deployment evidence"</dt><dd>{summary.deployment_evidence_disposition}</dd>
                            {summary.target.map(|target| view! { <dt>"Agent content"</dt><dd><code>{target.agent_content_digest}</code></dd><dt>"Environment content"</dt><dd><code>{target.environment_content_digest}</code></dd>
                                <dt>"Catalog release"</dt><dd><code>{target.catalog_release_digest}</code></dd> })}</dl></section>
                        <section aria-labelledby="evaluation-case-title"><h2 id="evaluation-case-title">"Case projection"</h2>
                            <ul>{run.cases.rows.into_iter().map(|item| view! { <li><strong>{item.case_key}</strong>" · "{item.lifecycle_status}" · "{match item.passed { None => "Not completed", Some(true) => "Passed", Some(false) => "Failed" }}
                                {item.failure_code.filter(|code| !code.is_empty()).map(|code| format!(" · {code}"))}</li> }).collect_view()}</ul>
                            {more_cases_page.is_some().then(|| view! { <button type="button" on:click=more_cases>"Load more cases"</button> })}</section>
                        <section aria-labelledby="evaluation-metric-title"><h2 id="evaluation-metric-title">"Metric projection"</h2>
                            <ul>{run.metrics.rows.into_iter().map(|item| view! { <li><strong>{item.metric_code}</strong>" · value "{item.value}" · threshold "{item.threshold}" · "{if item.passed { "Passed" } else { "Failed" }}</li> }).collect_view()}</ul>
                            {more_metrics_page.is_some().then(|| view! { <button type="button" on:click=more_metrics>"Load more metrics"</button> })}</section>
                        <section aria-labelledby="evaluation-artifact-title"><h2 id="evaluation-artifact-title">"Artifact metadata"</h2>
                            <ul>{run.artifacts.rows.into_iter().map(|item| view! { <li>{item.artifact_kind}" · "<code>{item.content_digest}</code>" · "{item.media_type}" · "{item.byte_length}" bytes"</li> }).collect_view()}</ul>
                            {more_artifacts_page.is_some().then(|| view! { <button type="button" on:click=more_artifacts>"Load more artifacts"</button> })}</section>
                        <section aria-labelledby="evaluation-audit-title"><h2 id="evaluation-audit-title">"Audit projection"</h2>
                            <ol>{run.audit.rows.into_iter().map(|item| view! { <li>{display_time(Some(&item.occurred_at))}" · "{item.action}" · "{item.summary}</li> }).collect_view()}</ol>
                            {more_audit_page.is_some().then(|| view! { <button type="button" on:click=more_audit>"Load more audit facts"</button> })}</section>
                        <p><a href=audit_href>"Review evaluation audit history"</a></p> } })}
                    <p><a href=format!("/projects/{}/evaluations", project.get_untracked())>"Back to evaluations"</a></p> } })}
                {move || cancel_dialog.get().then(|| view! { <ConfirmationDialog title="Cancel evaluation" on_close=Callback::new(move |()| cancel_dialog.set(false))>
                    <p>"The service records an immutable cancellation outcome when the active run still owns this generation."</p>
                    <button type="button" on:click=cancel>"Cancel evaluation"</button><button type="button" on:click=move |_| cancel_dialog.set(false)>"Keep evaluation running"</button></ConfirmationDialog> })}
            </main>
        }.into_any()
    }
}

/// Runs that target one of this agent's versions or deployments. `EvaluationRunFilter` has no agent
/// field, so the page joins the project's runs against the agent's versions and deployments.
#[component]
pub fn AgentEvaluationsPage() -> impl IntoView {
    let (project, agent) = (route_param("project_id"), route_param("agent_id"));
    let state = RwSignal::new(Kind::Loading);
    let runs = RwSignal::new(None::<Vec<EvaluationRunSummary>>);
    let serial = StoredValue::new(0_u32);
    let load = move || {
        let (project_id, agent_id) = (project.get_untracked(), agent.get_untracked());
        let current = serial.get_value() + 1;
        serial.set_value(current);
        state.set(Kind::Loading);
        runs.set(None);
        spawn_local(async move {
            let versions = request_agent_versions(&project_id, &agent_id).await;
            let deployments = request_deployments(&project_id, Some(&agent_id)).await;
            if serial.try_get_value() != Some(current) {
                return;
            }
            let (versions, deployments) = match (versions, deployments) {
                (Err(GraphqlError::Transport(_)), _) | (_, Err(GraphqlError::Transport(_))) => {
                    state.set(Kind::Error);
                    return;
                }
                (Ok(Some(versions)), Ok(Some(deployments))) => (versions, deployments),
                _ => {
                    state.set(Kind::Unavailable);
                    return;
                }
            };
            let found = request_evaluation_runs(&project_id, None).await;
            if serial.try_get_value() != Some(current) {
                return;
            }
            match found {
                Err(GraphqlError::Transport(_)) => state.set(Kind::Error),
                Ok(Some(page)) => {
                    runs.set(Some(
                        page.rows
                            .into_iter()
                            .filter(|run| match run.kind() {
                                Some(EvaluationTargetKind::AgentVersion) => {
                                    versions.iter().any(|version| version.id == run.target_id)
                                }
                                Some(EvaluationTargetKind::Deployment) => deployments
                                    .iter()
                                    .any(|deployment| deployment.id == run.target_id),
                                None => false,
                            })
                            .collect(),
                    ));
                    state.set(Kind::Ready);
                }
                _ => state.set(Kind::Unavailable),
            }
        });
    };
    Effect::new(move |_| {
        let _ = (project.get(), agent.get());
        load();
    });
    view! {
        <main class="directory agent-scoped-page" aria-labelledby="agent-evaluations-title">
            {move || view! { <AgentTabs project_id=project.get() agent_id=agent.get() active="evaluations" /> }}
            {move || view! { <PageHeader title_id="agent-evaluations-title" title="Evaluations".to_string()
                description="Runs targeting one of this agent's immutable versions or deployments. Evaluation queueing remains on the project-wide"
                description_link=(format!("/projects/{}/evaluations", project.get()), "Evaluations") description_tail=" page.">
                <button type="button" on:click=move |_| load()>"Refresh evaluations"</button></PageHeader> }}
            {move || (state.get() == Kind::Unavailable).then(|| view! { <p role="status">"Evaluations are unavailable."</p> })}
            {move || (state.get() == Kind::Error).then(|| view! { <p role="alert">"We could not refresh this agent's evaluation runs."</p> })}
            {move || (state.get() == Kind::Loading && runs.with(Option::is_none)).then(|| view! { <p role="status">"Loading this agent's evaluation runs…"</p> })}
            {move || runs.get().map(|rows| if rows.is_empty() { view! { <p role="status">"No evaluation run has targeted this agent's versions or deployments."</p> }.into_any() } else { run_rows(project.get(), rows).into_any() })}
        </main>
    }
}
