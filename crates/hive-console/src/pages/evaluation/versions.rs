//! The immutable version surfaces: the publication review, the version history, one version's
//! detail and usage, and a comparison of two versions.

use super::{alert, can, route_param, state_lines, Kind, NO_DOCUMENT};
use crate::api::evaluation::*;
use crate::confirmation_dialog::ConfirmationDialog;
use crate::format::random_uuid;
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_query_map};

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
