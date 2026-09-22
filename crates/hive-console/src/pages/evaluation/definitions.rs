//! The definition page: the draft document, its diagnostics, the commands that edit and publish
//! it, and the dialog that starts a run against a published version.

use super::{alert, can, route_param, state_lines, Kind};
use crate::api::evaluation::*;
use crate::format::random_uuid;
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_query_map};

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
