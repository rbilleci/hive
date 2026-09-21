//! Ports `DeploymentPages.tsx` and the deployments half of `AgentScopedPages.tsx`.

use crate::agent_tabs::AgentTabs;
use crate::api::agent_draft::{request_agent_version, AgentVersionFields};
use crate::api::console::has_capability;
use crate::api::deployment::{
    cancel_deployment, deploy_agent_version, promote_deployment, request_deployment,
    request_deployment_environments, request_deployment_preview, request_deployments,
    retry_deployment, rollback_deployment, ApprovalEvidenceKind, CancelDeploymentInput,
    DeployAgentVersionInput, Deployment, DeploymentLifecycleStatus, DeploymentListItem,
    DeploymentMutationPayload, DeploymentPreviewFields, DeploymentRuntimeHealthStatus,
    DeploymentStrategy, DeploymentTimelineEvent, EnvironmentDefinitionVersion,
    LogicalEnvironmentClass, PromoteDeploymentInput, RetryDeploymentInput, RollbackDeploymentInput,
};
use crate::confirmation_dialog::ConfirmationDialog;
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map};
use std::time::Duration;

pub fn display_time(value: Option<&str>) -> String {
    value.map_or("Not recorded".to_string(), |value| {
        String::from(
            js_sys::Date::new(&value.into())
                .to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED),
        )
    })
}

pub fn random_uuid() -> String {
    window()
        .crypto()
        .map(|crypto| crypto.random_uuid())
        .unwrap_or_default()
}

fn encode(value: &str) -> String {
    String::from(js_sys::encode_uri_component(value))
}

fn is_terminal(status: DeploymentLifecycleStatus) -> bool {
    use DeploymentLifecycleStatus::{Active, Canceled, Failed, RolledBack};
    matches!(status, Active | Failed | Canceled | RolledBack)
}

fn joined_or<T: ToString>(values: &[T], empty: &str) -> String {
    if values.is_empty() {
        empty.to_string()
    } else {
        values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn message_role(message: &str) -> &'static str {
    if message.contains("could") {
        "alert"
    } else {
        "status"
    }
}

#[component]
pub fn DeploymentRequestPage() -> impl IntoView {
    let console = use_console();
    let params = use_params_map();
    let navigate = use_navigate();
    let route = Memo::new(move |_| {
        let read = params.read();
        (
            read.get("project_id").unwrap_or_default(),
            read.get("agent_id").unwrap_or_default(),
            read.get("version_id").unwrap_or_default(),
        )
    });
    let can_request = Memo::new(move |_| {
        console.context.with(|context| {
            has_capability(context, "DEPLOYMENT.REQUEST", "PROJECT", &route.get().0)
        })
    });
    let version = RwSignal::new(None::<AgentVersionFields>);
    let environments = RwSignal::new(Vec::<EnvironmentDefinitionVersion>::new());
    let environment_id = RwSignal::new(String::new());
    let strategy = RwSignal::new(DeploymentStrategy::Rolling);
    let preview = RwSignal::new(None::<DeploymentPreviewFields>);
    let message = RwSignal::new("Loading immutable deployment inputs…".to_string());
    let idempotency_key = RwSignal::new(random_uuid());
    let submitting = RwSignal::new(false);
    // Every visit to a route gets a number; a response belongs to the visit that requested it.
    let visit = StoredValue::new(0_u32);
    let current = move |expected: u32| visit.try_get_value() == Some(expected);

    Effect::new(move |_| {
        let (project, agent, id) = route.get();
        let allowed = can_request.get();
        let this = visit.get_value() + 1;
        visit.set_value(this);
        version.set(None);
        environments.set(Vec::new());
        environment_id.set(String::new());
        strategy.set(DeploymentStrategy::Rolling);
        preview.set(None);
        message.set("Loading immutable deployment inputs…".to_string());
        idempotency_key.set(random_uuid());
        submitting.set(false);
        let (wanted_agent, wanted_id) = (agent.clone(), id.clone());
        spawn_local(async move {
            let result = request_agent_version(&project, &agent, &id).await;
            if !current(this) {
                return;
            }
            match result {
                Err(GraphqlError::SessionExpired) => {
                    message.set("Your session has expired.".to_string())
                }
                Err(GraphqlError::Transport(_)) => {
                    message.set("We could not load this immutable version.".to_string())
                }
                Ok(Some(value)) if value.id == wanted_id && value.agent_id == wanted_agent => {
                    version.set(Some(value));
                    message.set(String::new());
                }
                Ok(_) => message.set("This immutable version is unavailable.".to_string()),
            }
        });
        if !allowed {
            return;
        }
        let id = route.get_untracked().2;
        spawn_local(async move {
            let result = request_deployment_environments(&id).await;
            if !current(this) {
                return;
            }
            match result {
                Err(GraphqlError::SessionExpired) => {
                    message.set("Your session has expired.".to_string())
                }
                Err(GraphqlError::Transport(_)) => {
                    message.set("We could not load immutable environment definitions.".to_string())
                }
                Ok(None) => message.set("Deployment environments are unavailable.".to_string()),
                Ok(Some(list)) => {
                    environment_id.set(
                        list.first()
                            .map(|first| first.id.clone())
                            .unwrap_or_default(),
                    );
                    environments.set(list);
                }
            }
        });
    });

    // The preview is recomputed for each environment and strategy; a stale answer is dropped.
    let preview_serial = StoredValue::new(0_u32);
    Effect::new(move |_| {
        let (environment, chosen) = (environment_id.get(), strategy.get());
        let this = visit.get_value();
        let serial = preview_serial.get_value() + 1;
        preview_serial.set_value(serial);
        preview.set(None);
        if environment.is_empty() || !can_request.get_untracked() {
            return;
        }
        let id = route.get_untracked().2;
        spawn_local(async move {
            let result = request_deployment_preview(&id, &environment, chosen).await;
            if !current(this) || preview_serial.try_get_value() != Some(serial) {
                return;
            }
            match result {
                Err(GraphqlError::SessionExpired) => {
                    message.set("Your session has expired.".to_string())
                }
                Err(GraphqlError::Transport(_)) => {
                    message.set("We could not prepare the deployment preview.".to_string())
                }
                Ok(Some(value))
                    if value.environment_definition_version.id.inner() == environment
                        && value.strategy == chosen =>
                {
                    preview.set(Some(value))
                }
                Ok(_) => message.set("The deployment preview is unavailable.".to_string()),
            }
        });
    });

    let submit = move |event: ev::SubmitEvent| {
        event.prevent_default();
        let (project, _, id) = route.get_untracked();
        if preview.with_untracked(Option::is_none)
            || environment_id.with_untracked(String::is_empty)
            || !can_request.get_untracked()
            || submitting.get_untracked()
        {
            return;
        }
        let this = visit.get_value();
        submitting.set(true);
        message.set(String::new());
        let input = DeployAgentVersionInput {
            agent_version_id: id.as_str().into(),
            environment_definition_version_id: environment_id.get_untracked().as_str().into(),
            strategy: strategy.get_untracked(),
            idempotency_key: Some(idempotency_key.get_untracked()),
        };
        let navigate = navigate.clone();
        spawn_local(async move {
            let result = deploy_agent_version(input).await;
            if !current(this) {
                return;
            }
            match result {
                Err(GraphqlError::SessionExpired) => {
                    message.set("Your session has expired.".to_string())
                }
                Err(GraphqlError::Transport(_)) => message.set(
                    "We could not request this deployment. Reusing the same key remains safe."
                        .to_string(),
                ),
                Ok(DeploymentMutationPayload { problems, .. }) if !problems.is_empty() => {
                    message.set(problems[0].message.clone())
                }
                Ok(DeploymentMutationPayload {
                    deployment: Some(deployment),
                    ..
                }) if deployment.project_id == project => navigate(
                    &format!("/projects/{project}/deployments/{}", deployment.id),
                    Default::default(),
                ),
                Ok(_) => message.set("The requested deployment is unavailable.".to_string()),
            }
            submitting.set(false);
        });
    };

    view! {
        <main class="deployment-page" aria-labelledby="deployment-request-title">
            <PageHeader title_id="deployment-request-title" title="Request deployment".to_string() />
            {move || { let text = message.get(); (!text.is_empty()).then(|| view! { <p role=message_role(&text)>{text.clone()}</p> }) }}
            {move || version.get().map(|value| { let submit = submit.clone(); let (project, agent, id) = route.get_untracked(); view! {
                <p>{value.display_name()}" · immutable v"{value.version_number}</p>
                {move || (!can_request.get()).then(|| view! { <p role="status">"Your current project capabilities do not permit deployment requests."</p> })}
                <form class="deployment-request-form" on:submit=submit>
                    <label>"Environment definition"<select prop:value=move || environment_id.get() disabled=move || !can_request.get() || submitting.get() || environments.with(Vec::is_empty) on:change=move |event| environment_id.set(event_target_value(&event))>
                        {move || environments.get().into_iter().map(|environment| { let id = environment.id.clone(); let chosen = id.clone(); view! {
                            <option value=id selected=move || environment_id.get() == chosen>{environment.display_name}" · "{environment.stable_definition_id}"@"{environment.version}</option> } }).collect_view()}</select></label>
                    <label>"Strategy"<select prop:value=move || strategy.get().as_str() disabled=move || !can_request.get() || submitting.get()
                        on:change=move |event| { if let Some(next) = DeploymentStrategy::from_wire(&event_target_value(&event)) { strategy.set(next); } }>
                        {[("REPLACE", "Replace"), ("ROLLING", "Rolling"), ("CANARY", "Canary"), ("BLUE_GREEN", "Blue/green")].into_iter().map(|(wire, label)| view! {
                            <option value=wire selected=move || strategy.get().as_str() == wire>{label}</option> }).collect_view()}</select></label>
                    {move || match preview.get() {
                        Some(value) => view! { <Preview preview=value /> }.into_any(),
                        None => can_request.get().then(|| view! { <p role="status">"Computing server-owned frozen inputs…"</p> }).into_any(),
                    }}
                    <button type="submit" disabled=move || preview.with(Option::is_none) || environment_id.with(String::is_empty) || !can_request.get() || submitting.get()>
                        {move || if submitting.get() { "Requesting deployment…" } else { "Request deployment" }}</button>
                </form>
                <p><a href=format!("/projects/{project}/agents/{agent}/versions/{id}")>"Back to immutable version"</a></p>
            } })}
        </main>
    }
}

#[component]
fn Preview(preview: DeploymentPreviewFields) -> impl IntoView {
    let environment = &preview.environment_definition_version;
    let target = preview.current_target.as_ref();
    view! {
        <section class="deployment-preview" aria-labelledby="deployment-preview-title"><h2 id="deployment-preview-title">"Frozen request preview"</h2><dl>
            <dt>"Environment definition"</dt><dd>{format!("{} · {}@{}", environment.display_name, environment.stable_definition_id, environment.version)}</dd>
            <dt>"Logical policy class"</dt><dd>{environment.logical_environment_class.as_str()}</dd>
            <dt>"Current alias target"</dt><dd>{target.map_or("No current alias target is recorded in the local M13 projection.".to_string(), |target| format!("Version {} · {}", target.agent_version_number, target.target_digest))}</dd>
            <dt>"Last successful deployment"</dt><dd>{target.map_or("No successful local deployment is recorded.".to_string(), |target| display_time(Some(&target.requested_at)))}</dd>
            <dt>"Requirement expiry"</dt><dd>{display_time(preview.requirement_expires_at.as_deref())}</dd>
            <dt>"Risk"</dt><dd>{preview.risk.as_str()}</dd>
            <dt>"Policy revision"</dt><dd>{preview.policy_revision}" "<code>{preview.policy_digest.clone()}</code></dd>
            <dt>"Required evidence"</dt><dd>{joined_or(&preview.required_evidence, "None")}</dd>
            <dt>"Distinct approvers"</dt><dd>{preview.required_approvers}</dd>
            <dt>"Compatibility"</dt><dd>{preview.compatibility.clone()}</dd>
            <dt>"Warnings"</dt><dd>{if preview.warnings.is_empty() { "No local compatibility warning is recorded.".to_string() } else { preview.warnings.join(" ") }}</dd>
            <dt>"Version content"</dt><dd><code>{preview.agent_content_digest.clone()}</code></dd>
            <dt>"Target digest"</dt><dd><code>{preview.target_digest.clone()}</code></dd>
            <dt>"Plan digest"</dt><dd><code>{preview.plan_digest.clone()}</code></dd>
            <dt>"Package digest"</dt><dd><code>{preview.package_digest.clone()}</code></dd>
            <dt>"Binding digest"</dt><dd><code>{preview.binding_digest.clone()}</code></dd></dl>
            <p>"Submission freezes these server-derived facts. Later policy changes do not rewrite this request."</p></section>
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListKind {
    Loading,
    Ready,
    Error,
    Unavailable,
}

struct ListCopy {
    title_id: &'static str,
    main_class: &'static str,
    loading: &'static str,
    empty: &'static str,
    error: &'static str,
}

/// The project-wide list and the one-agent list differ in their filter, heading, and wording.
fn deployment_list(agent_scoped: bool) -> impl IntoView {
    let copy = if agent_scoped {
        ListCopy {
            title_id: "agent-deployments-title",
            main_class: "directory agent-scoped-page",
            loading: "Loading this agent's deployments…",
            empty: "No deployments have been requested for this agent.",
            error: "We could not refresh this agent's deployments.",
        }
    } else {
        ListCopy { title_id: "deployments-title", main_class: "deployment-page", loading: "Loading deployments…", empty: "No deployments have been requested for this project.", error: "We could not refresh deployments. Displayed rows remain from the last successful request." }
    };
    let revision = use_console().revision;
    let params = use_params_map();
    let route = Memo::new(move |_| {
        (
            params.read().get("project_id").unwrap_or_default(),
            params.read().get("agent_id").filter(|_| agent_scoped),
        )
    });
    let kind = RwSignal::new(ListKind::Loading);
    let rows = RwSignal::new(None::<Vec<DeploymentListItem>>);
    let sequence = StoredValue::new(0_u32);
    let load = move || {
        let (project, agent) = route.get_untracked();
        let this = sequence.get_value() + 1;
        sequence.set_value(this);
        kind.set(ListKind::Loading);
        if agent_scoped {
            rows.set(None);
        }
        spawn_local(async move {
            let result = request_deployments(&project, agent.as_deref()).await;
            if sequence.try_get_value() != Some(this) {
                return;
            }
            match result {
                Ok(Some(list)) => {
                    rows.set(Some(list));
                    kind.set(ListKind::Ready);
                }
                Ok(_) | Err(GraphqlError::SessionExpired) => {
                    rows.set(None);
                    kind.set(ListKind::Unavailable);
                }
                Err(GraphqlError::Transport(_)) => kind.set(ListKind::Error),
            }
        });
    };
    Effect::new(move |_| {
        let _ = route.get();
        let _ = revision.get();
        rows.set(None);
        load();
    });
    view! {
        <main class=copy.main_class aria-labelledby=copy.title_id>
            {move || route.get().1.map(|agent| view! { <AgentTabs project_id=route.get_untracked().0 agent_id=agent active="deployments" /> })}
            <PageHeader title_id=copy.title_id title="Deployments".to_string()><button type="button" on:click=move |_| load()>"Refresh deployments"</button></PageHeader>
            {move || (kind.get() == ListKind::Unavailable).then(|| view! { <p role="status">"Deployments are unavailable."</p> })}
            {move || (kind.get() == ListKind::Error).then(|| view! { <p role="alert">{copy.error}</p> })}
            {move || (kind.get() == ListKind::Loading && rows.with(Option::is_none)).then(|| view! { <p role="status">{copy.loading}</p> })}
            {move || rows.with(|list| list.as_ref().is_some_and(Vec::is_empty)).then(|| view! { <p role="status">{copy.empty}</p> })}
            {move || rows.get().filter(|list| !list.is_empty()).map(|list| { let project = route.get_untracked().0; view! {
                <ul class="deployment-list">{list.into_iter().map(|deployment| { let status = deployment.lifecycle_status.clone();
                    let environment = deployment.environment_definition_versions.as_ref().map(|value| value.display_name.clone()).unwrap_or_default(); view! {
                    <li><div><h2><a href=format!("/projects/{project}/deployments/{}", deployment.id)>
                        {if agent_scoped { format!("v{}", deployment.agent_version_number()) } else { format!("{} · v{}", deployment.agent_display_name(), deployment.agent_version_number()) }}</a></h2>
                        <p>{environment}" · "{deployment.strategy.clone()}" · requested "{display_time(Some(&deployment.requested_at))}</p></div>
                        <span class=format!("deployment-status deployment-status-{}", status.to_lowercase())>{status.clone()}</span></li> } }).collect_view()}</ul> } })}
        </main>
    }
}

#[component]
pub fn DeploymentsPage() -> impl IntoView {
    deployment_list(false)
}

#[component]
pub fn AgentDeploymentsPage() -> impl IntoView {
    deployment_list(true)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailState {
    Loading,
    Ready,
    Error,
    Unavailable,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Recovery {
    Retry,
    Promote,
    Rollback,
}

#[component]
pub fn DeploymentDetailPage() -> impl IntoView {
    let console = use_console();
    let params = use_params_map();
    let navigate = use_navigate();
    let route = Memo::new(move |_| {
        (
            params.read().get("project_id").unwrap_or_default(),
            params.read().get("deployment_id").unwrap_or_default(),
        )
    });
    let deployment = RwSignal::new(None::<Deployment>);
    let timeline = RwSignal::new(Vec::<DeploymentTimelineEvent>::new());
    let state = RwSignal::new(DetailState::Loading);
    let action_message = RwSignal::new(String::new());
    let dialog = RwSignal::new(None::<Recovery>);
    let (rollback_reason, production_confirmation) =
        (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let sequence = StoredValue::new(0_u32);
    let backoff = StoredValue::new(3_000_u32);
    let timer = StoredValue::new(None::<TimeoutHandle>);
    let clear_timer = move || {
        if let Some(handle) = timer.try_update_value(Option::take).flatten() {
            handle.clear();
        }
    };
    let can = move |code: &'static str| {
        console
            .context
            .with(|context| has_capability(context, code, "PROJECT", &route.get().0))
    };

    // A projection older than the one displayed is ignored, so a slow poll never rewinds the page.
    let load = Callback::new(move |done: Callback<()>| {
        let (project, id) = route.get_untracked();
        let this = sequence.get_value() + 1;
        sequence.set_value(this);
        spawn_local(async move {
            let result = request_deployment(&id).await;
            let latest = sequence.try_get_value() == Some(this);
            if route.try_get_untracked() != Some((project.clone(), id.clone())) {
                return;
            }
            match result {
                Err(GraphqlError::Transport(_)) => {
                    if latest {
                        backoff.update_value(|value| *value = (*value * 2).min(30_000));
                        state.set(DetailState::Error);
                    }
                }
                Ok(Some((next, events))) if next.id == id && next.project_id == project => {
                    let newer = deployment.with_untracked(|previous| {
                        previous.as_ref().is_none_or(|previous| {
                            next.revision > previous.revision
                                || (next.revision == previous.revision
                                    && next.projection() > previous.projection())
                        })
                    });
                    if newer || (latest && deployment.with_untracked(Option::is_none)) {
                        deployment.set(Some(next));
                        timeline.set(events);
                        state.set(DetailState::Ready);
                        backoff.set_value(3_000);
                    } else if latest {
                        state.set(DetailState::Ready);
                    }
                }
                Ok(_) | Err(GraphqlError::SessionExpired) => {
                    if latest {
                        state.set(DetailState::Unavailable);
                    }
                }
            }
            done.run(());
        });
    });
    let nothing = Callback::new(|()| ());

    Effect::new(move |_| {
        let _ = route.get();
        let _ = console.revision.get();
        deployment.set(None);
        timeline.set(Vec::new());
        action_message.set(String::new());
        dialog.set(None);
        rollback_reason.set(String::new());
        production_confirmation.set(String::new());
        state.set(DetailState::Loading);
        backoff.set_value(3_000);
        load.run(nothing);
    });

    // While execution is active the visible page polls, with jitter and failure backoff.
    let schedule = StoredValue::new(None::<Callback<()>>);
    let active = Memo::new(move |_| {
        deployment.with(|current| {
            current
                .as_ref()
                .is_some_and(|current| current.status().is_none_or(|status| !is_terminal(status)))
        })
    });
    let visible_online = || {
        document().visibility_state() == web_sys::VisibilityState::Visible
            && window().navigator().on_line()
    };
    schedule.set_value(Some(Callback::new(move |()| {
        clear_timer();
        if !active.try_get_untracked().unwrap_or(false) || !visible_online() {
            return;
        }
        let delay =
            backoff.get_value().max(3_000) + (js_sys::Math::random() * 2_001.0).floor() as u32;
        let again = schedule.get_value().unwrap_or(nothing);
        timer.set_value(
            set_timeout_with_handle(
                move || load.run(again),
                Duration::from_millis(u64::from(delay)),
            )
            .ok(),
        );
    })));
    let reschedule = move || {
        if let Some(callback) = schedule.try_get_value().flatten() {
            callback.run(());
        }
    };
    Effect::new(move |_| {
        let _ = active.get();
        reschedule();
    });
    let refresh_now = move || {
        if active.get_untracked() && visible_online() {
            load.run(schedule.get_value().unwrap_or(nothing));
        }
    };
    let on_focus = window_event_listener(ev::focus, move |_| refresh_now());
    let on_online = window_event_listener(ev::online, move |_| refresh_now());
    crate::dom::on_document_event("visibilitychange", reschedule);
    on_cleanup(move || {
        clear_timer();
        on_focus.remove();
        on_online.remove();
    });

    // Applies a recovery mutation: a refusal shows its message; success either stays or moves to the new cycle.
    let settle = move |result: Result<DeploymentMutationPayload, GraphqlError>,
                       moves: bool,
                       navigate: &dyn Fn(&str, leptos_router::NavigateOptions)| {
        let (project, id) = route.get_untracked();
        match result {
            Err(GraphqlError::SessionExpired) => state.set(DetailState::Unavailable),
            Err(GraphqlError::Transport(_)) => action_message.set(
                "We could not record the cancellation. Refresh the deployment before trying again."
                    .to_string(),
            ),
            Ok(DeploymentMutationPayload { problems, .. }) if !problems.is_empty() => {
                action_message.set(problems[0].message.clone());
                if !moves {
                    load.run(nothing);
                }
            }
            Ok(DeploymentMutationPayload {
                deployment: Some(next),
                ..
            }) if next.project_id == project => {
                dialog.set(None);
                if moves {
                    navigate(
                        &format!("/projects/{project}/deployments/{}", next.id),
                        Default::default(),
                    );
                } else if next.id == id {
                    timeline.set(next.timeline.clone());
                    deployment.set(Some(next));
                    state.set(DetailState::Ready);
                    load.run(nothing);
                }
            }
            Ok(_) => {}
        }
    };
    let run = move |kind: Option<Recovery>| {
        let Some(current) = deployment.get_untracked() else {
            return;
        };
        action_message.set(String::new());
        let navigate = navigate.clone();
        let (id, revision) = (
            cynic::Id::new(current.id.clone()),
            i64::from(current.revision),
        );
        spawn_local(async move {
            let (result, moves) = match kind {
                None => (
                    cancel_deployment(CancelDeploymentInput {
                        deployment_id: id,
                        expected_revision: revision,
                        reason: Some("Canceled from the local deployment detail.".to_string()),
                    })
                    .await,
                    false,
                ),
                Some(Recovery::Retry) => (
                    retry_deployment(RetryDeploymentInput {
                        deployment_id: id,
                        expected_revision: revision,
                        idempotency_key: random_uuid(),
                    })
                    .await,
                    true,
                ),
                Some(Recovery::Promote) => (
                    promote_deployment(PromoteDeploymentInput {
                        deployment_id: id,
                        expected_revision: revision,
                        idempotency_key: random_uuid(),
                    })
                    .await,
                    false,
                ),
                Some(Recovery::Rollback) => (
                    rollback_deployment(RollbackDeploymentInput {
                        deployment_id: id,
                        target_agent_version_id: current
                            .rollback_target
                            .as_ref()
                            .map(|target| cynic::Id::new(target.agent_version_id.clone())),
                        expected_revision: revision,
                        reason: rollback_reason.get_untracked(),
                        production_confirmation: Some(production_confirmation.get_untracked())
                            .filter(|text| !text.is_empty()),
                        idempotency_key: random_uuid(),
                    })
                    .await,
                    true,
                ),
            };
            settle(result, moves, &navigate);
        });
    };

    // The header, the facts, and the dialog render separately. A poll replaces the facts only, so a
    // header button or a dialog field keeps its focus while newer projections arrive.
    let run = StoredValue::new_local(run);
    let go = move |kind: Option<Recovery>| run.with_value(|run| run(kind));
    let status = Memo::new(move |_| {
        deployment.with(|current| current.as_ref().and_then(Deployment::status))
    });
    let title = Memo::new(move |_| {
        deployment.with(|current| {
            current.as_ref().map(|current| {
                format!(
                    "{} · v{}",
                    current.agent_display_name(),
                    current.agent_version_number()
                )
            })
        })
    });
    let can_cancel = Memo::new(move |_| {
        status.get().is_some_and(|status| !is_terminal(status)) && can("DEPLOYMENT.CANCEL")
    });
    let can_retry = Memo::new(move |_| {
        status.get() == Some(DeploymentLifecycleStatus::Failed) && can("DEPLOYMENT.RETRY")
    });
    let can_promote = Memo::new(move |_| {
        status.get() == Some(DeploymentLifecycleStatus::Active)
            && deployment.with(|current| {
                current.as_ref().is_some_and(|current| {
                    current
                        .deployment_runtime_health
                        .as_ref()
                        .and_then(|health| health.health())
                        == Some(DeploymentRuntimeHealthStatus::Healthy)
                })
            })
            && can("DEPLOYMENT.PROMOTE")
    });
    let can_rollback = Memo::new(move |_| {
        matches!(
            status.get(),
            Some(DeploymentLifecycleStatus::Failed | DeploymentLifecycleStatus::Active)
        ) && deployment.with(|current| {
            current
                .as_ref()
                .is_some_and(|current| current.rollback_target.is_some())
        }) && can("DEPLOYMENT.ROLLBACK")
    });

    // A Memo, because setting `state` to the value it already holds still notifies, and the page must not remount on every reload.
    let unavailable = Memo::new(move |_| state.get() == DetailState::Unavailable);
    move || {
        if unavailable.get() {
            return view! { <main class="deployment-page"><p role="status">"This deployment is unavailable."</p></main> }.into_any();
        }
        view! {
            <main class="deployment-page" aria-labelledby="deployment-detail-title">
                {move || deployment.with(Option::is_none).then(|| view! { <p role="status">"Loading deployment…"</p> })}
                {move || (state.get() == DetailState::Error).then(|| view! { <p role="alert">"We could not refresh this deployment. Displayed facts remain from the last successful request."</p> })}
                {move || { let text = action_message.get(); (!text.is_empty()).then(|| view! { <p role="alert">{text}</p> }) }}
                {move || title.with(Option::is_some).then(|| view! {
                    <PageHeader title_id="deployment-detail-title" title=Signal::derive(move || title.get().unwrap_or_default())>
                        <button type="button" on:click=move |_| load.run(nothing)>"Refresh deployment"</button>
                        {move || can_cancel.get().then(|| view! { <button type="button" on:click=move |_| go(None)>"Cancel deployment"</button> })}
                        {move || can_retry.get().then(|| view! { <button type="button" on:click=move |_| dialog.set(Some(Recovery::Retry))>"Retry deployment"</button> })}
                        {move || can_promote.get().then(|| view! { <button type="button" on:click=move |_| dialog.set(Some(Recovery::Promote))>"Promote deployment"</button> })}
                        {move || can_rollback.get().then(|| view! { <button type="button" on:click=move |_| dialog.set(Some(Recovery::Rollback))>"Review rollback"</button> })}
                    </PageHeader> })}
                {move || deployment.get().map(|current| {
                    let project = route.get_untracked().0;
                    let status = current.lifecycle_status.clone();
                    let terminal = current.status().is_some_and(is_terminal);
                    let needs_evaluation = current.required_evidence().iter().any(|kind| kind == ApprovalEvidenceKind::EvaluationPassed.as_str());
                    let environment = current.environment_definition_versions.clone().map(|value| format!("{}@{}", value.stable_definition_id, value.version)).unwrap_or_default();
                    let attempt = current.current_attempt.clone();
                    let plan = current.plan.clone();
                    let policy = current.deployment_policy_snapshots.clone();
                    let health = current.deployment_runtime_health.clone();
                    let evidence = current.evidence();
                    let (id, lifecycle) = (current.id.clone(), status.clone());
                    view! {
                        <p class="page-header-meta">"Lifecycle: "<strong>{status}</strong></p>
                        <p class="deployment-polling" role="status">{if terminal { "This deployment reached a terminal state." } else { "This visible page polls the deployment projection while execution remains active." }}</p>
                        <section class="deployment-projections" aria-label="Separate deployment projections">
                            <article><h2>"Lifecycle"</h2><p>{lifecycle}</p><small>"Request state · revision "{current.revision}</small></article>
                            <article><h2>"Current attempt"</h2><p>{attempt.as_ref().map_or("Not started".to_string(), |attempt| attempt.status.clone())}</p>
                                <small>{attempt.as_ref().map_or("No execution attempt exists yet.".to_string(), |attempt| format!("Attempt {}", attempt.attempt_number))}</small></article>
                            <article><h2>"Runtime health"</h2><p>{health.as_ref().map_or("NOT_OBSERVED".to_string(), |health| health.status.clone())}</p>
                                <small>{health.as_ref().map_or(String::new(), |health| health.summary.clone())}</small></article>
                        </section>
                        {attempt.as_ref().and_then(|attempt| attempt.failure_summary.clone().map(|summary| (summary, attempt.failure_code.clone()))).map(|(summary, code)| view! {
                            <section class="deployment-failure" aria-labelledby="deployment-failure-title"><h2 id="deployment-failure-title">"Failure investigation"</h2>
                                <p>{summary}</p><p>"Failure code: "<code>{code}</code></p><p>"Package digest: "<code>{plan.as_ref().map(|plan| plan.package_digest.clone())}</code></p></section> })}
                        <section class="deployment-facts"><h2>"Frozen plan and policy facts"</h2><dl>
                            <dt>"Environment definition"</dt><dd>{environment}</dd>
                            <dt>"Version"</dt><dd>{plan.as_ref().map(|plan| plan.agent_version_id.clone())}" "<code>{plan.as_ref().and_then(|plan| plan.agent_content_digest.clone())}</code></dd>
                            <dt>"Target"</dt><dd><code>{plan.as_ref().and_then(|plan| plan.target_digest.clone())}</code></dd>
                            <dt>"Plan"</dt><dd><code>{plan.as_ref().map(|plan| plan.plan_digest.clone())}</code></dd>
                            <dt>"Package"</dt><dd><code>{plan.as_ref().map(|plan| plan.package_digest.clone())}</code></dd>
                            <dt>"Catalog release"</dt><dd>{plan.as_ref().map(|plan| plan.catalog_release_id.clone())}" "<code>{plan.as_ref().and_then(|plan| plan.catalog_release_digest.clone())}</code></dd>
                            <dt>"Policy"</dt><dd>"revision "{policy.as_ref().map(|policy| policy.policy_revision)}" · "<code>{policy.as_ref().map(|policy| policy.policy_digest.clone())}</code></dd>
                            <dt>"Risk"</dt><dd>{policy.as_ref().map(|policy| policy.risk.clone())}</dd>
                            <dt>"Evidence"</dt><dd>{joined_or(&evidence.iter().map(|item| item.evidence_kind.clone()).collect::<Vec<_>>(), "None")}</dd></dl></section>
                        <section class="deployment-timeline"><h2>"Deployment timeline"</h2>
                            {move || { let events = timeline.get(); if events.is_empty() { view! { <p role="status">"No deployment history has been recorded."</p> }.into_any() } else { view! {
                                <ol>{events.into_iter().map(|event| view! { <li><strong>{event.stage}" · "{event.status}</strong><span>{event.message}</span><small>{event.source}</small>
                                    <time datetime=event.occurred_at.clone()>{display_time(Some(&event.occurred_at))}</time></li> }).collect_view()}</ol> }.into_any() } }}</section>
                        {needs_evaluation.then(|| view! { <p><a href=format!("/projects/{project}/evaluations?deploymentId={}", encode(&id))>"Review this deployment’s required evaluation"</a></p> })}
                        <p><a href=format!("/projects/{project}/audit?resourceType=DEPLOYMENT&resourceId={}", encode(&id))>"Review deployment audit history"</a></p>
                        <p><a href=format!("/projects/{project}/deployments")>"Back to deployments"</a></p>
                    }
                })}
                {move || dialog.get().zip(deployment.get_untracked()).map(|(kind, detail)| {
                    let needs_evaluation = detail.required_evidence().iter().any(|kind| kind == ApprovalEvidenceKind::EvaluationPassed.as_str());
                    let environment = detail.environment_definition_versions.clone();
                    let production = environment.as_ref().and_then(EnvironmentDefinitionVersion::environment_class) == Some(LogicalEnvironmentClass::Production);
                    let stable_id = environment.as_ref().map(|value| value.stable_definition_id.clone()).unwrap_or_default();
                    match kind {
                            Recovery::Retry => view! {
                                <ConfirmationDialog title="Retry failed deployment" on_close=Callback::new(move |()| dialog.set(None))>
                                    <p>"The service creates a new immutable deployment cycle from the failed deployment facts. The failed attempt remains in the timeline."</p>
                                    <button type="button" on:click=move |_| go(Some(Recovery::Retry))>"Retry deployment"</button><button type="button" on:click=move |_| dialog.set(None)>"Cancel"</button>
                                </ConfirmationDialog> }.into_any(),
                            Recovery::Promote => view! {
                                <ConfirmationDialog title="Promote healthy deployment" on_close=Callback::new(move |()| dialog.set(None))>
                                    <p>{format!("Alias: local:{stable_id}. Current version: v{0}. Target version: v{0}. Deployment: {1}. Strategy: {2}.", detail.agent_version_number(), detail.id, detail.strategy)}</p>
                                    <p>"Impact: the service records this healthy immutable target locally. It does not contact a provider or shift live traffic. Rollback remains available when a prior active target exists."</p>
                                    <button type="button" on:click=move |_| go(Some(Recovery::Promote))>"Promote deployment"</button><button type="button" on:click=move |_| dialog.set(None)>"Cancel"</button>
                                </ConfirmationDialog> }.into_any(),
                            Recovery::Rollback => { let expected = stable_id.clone(); view! {
                                <ConfirmationDialog title="Review rollback" on_close=Callback::new(move |()| dialog.set(None))>
                                    <p>{format!("Current version: v{}. Current health: {}. Target version: v{}. Target health: {}. Environment: {stable_id}.", detail.agent_version_number(), detail.deployment_runtime_health.as_ref().map_or(String::new(), |health| health.status.clone()),
                                        detail.rollback_target.as_ref().map_or(String::new(), |target| target.agent_version_number().to_string()),
                                        detail.rollback_target.as_ref().and_then(|target| target.deployment_runtime_health.as_ref()).map_or(String::new(), |health| health.status.clone()))}</p>
                                    <p>{if needs_evaluation { "Evaluation context: a new deployment cycle requires immutable evaluation evidence that matches its pending approval facts." } else { "Evaluation context: this immutable policy requires no evaluation evidence." }}
                                        " Impact: the service creates a new immutable local deployment cycle and does not contact a provider or shift live traffic."</p>
                                    <label>"Reason"<input aria-label="Rollback reason" prop:value=move || rollback_reason.get() on:input=move |event| rollback_reason.set(event_target_value(&event)) /></label>
                                    {production.then(|| view! { <label>"Type production environment ID "{stable_id.clone()}" exactly"<input aria-label="Production environment confirmation" prop:value=move || production_confirmation.get() on:input=move |event| production_confirmation.set(event_target_value(&event)) /></label> })}
                                    <button type="button" disabled=move || rollback_reason.with(|text| text.trim().is_empty()) || (production && production_confirmation.with(|text| text.trim() != expected))
                                        on:click=move |_| go(Some(Recovery::Rollback))>"Rollback deployment"</button><button type="button" on:click=move |_| dialog.set(None)>"Cancel"</button>
                                </ConfirmationDialog> }.into_any() }
                    }
                })}
            </main>
        }.into_any()
    }
}
