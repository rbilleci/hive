//! The project's evaluation list and an agent's: both show the definitions or runs in scope and
//! report "unavailable" rather than an empty list when the principal cannot see the scope.

use super::{alert, can, route_param, run_rows, state_lines, Kind};
use crate::agent_tabs::AgentTabs;
use crate::api::agent_draft::request_agent_versions;
use crate::api::deployment::request_deployments;
use crate::api::evaluation::*;
use crate::format::{encode, random_uuid};
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_query_map};

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
