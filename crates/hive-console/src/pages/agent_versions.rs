//! Agent creation and the read-only immutable version pages.

use crate::agent_tabs::AgentTabs;
use crate::api::agent_draft::{
    create_agent_draft, request_agent_version, request_agent_version_comparison,
    request_agent_versions, AgentVersionFields, ComparedVersion,
};
use crate::graphql::GraphqlError;
use crate::json_viewer::JsonViewer;
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};

fn encode(value: &str) -> String {
    String::from(js_sys::encode_uri_component(value))
}

fn local_time(value: &str) -> String {
    String::from(
        js_sys::Date::new(&value.into())
            .to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED),
    )
}

/// A message that reports a failure is an alert; anything else is a status.
fn message_role(message: &str) -> &'static str {
    if message.contains("could") {
        "alert"
    } else {
        "status"
    }
}

#[component]
pub fn CreateAgentDraftPage() -> impl IntoView {
    let params = use_params_map();
    let navigate = use_navigate();
    let (display_name, agent_id) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let message = RwSignal::new(None::<String>);
    let saving = RwSignal::new(false);
    let create = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let project = params
            .read_untracked()
            .get("project_id")
            .unwrap_or_default();
        if project.is_empty() || saving.get_untracked() {
            return;
        }
        saving.set(true);
        message.set(None);
        let navigate = navigate.clone();
        spawn_local(async move {
            match create_agent_draft(
                &project,
                &display_name.get_untracked(),
                &agent_id.get_untracked(),
            )
            .await
            {
                Err(GraphqlError::SessionExpired) => message.set(Some(
                    "Your session has expired. Sign in again to create an agent.".to_string(),
                )),
                Err(GraphqlError::Transport(_)) => message.set(Some(
                    "We could not create this agent. No agent was created.".to_string(),
                )),
                Ok(result) => {
                    if let Some(first) = result.problems.into_iter().next() {
                        message.set(Some(first.message));
                    } else if let Some(draft) = result.agent_draft {
                        navigate(
                            &format!("/projects/{project}/agents/{}/edit", draft.agent_id),
                            Default::default(),
                        );
                    }
                }
            }
            let _ = saving.try_set(false);
        });
    };
    view! {
        <main class="agent-authoring-route" aria-labelledby="create-agent-title">
            <PageHeader title_id="create-agent-title" title="Create agent".to_string() description="Creation opens a revision-protected local draft; it does not deploy anything." />
            {move || message.get().map(|text| view! { <p role="alert">{text}</p> })}
            <form on:submit=create>
                <label>"Agent ID (optional)"<input prop:value=move || agent_id.get() on:input=move |event| agent_id.set(event_target_value(&event)) aria-describedby="agent-id-help" /></label>
                <p id="agent-id-help">"Lowercase letters, digits, and hyphens only; a stable ID is generated when omitted."</p>
                <label>"Display name"<input required prop:value=move || display_name.get() on:input=move |event| display_name.set(event_target_value(&event)) /></label>
                <button type="submit" disabled=move || saving.get()>{move || if saving.get() { "Creating agent…" } else { "Create agent draft" }}</button>
            </form>
        </main>
    }
}

#[component]
fn VersionFacts(version: AgentVersionFields) -> impl IntoView {
    let dependencies = version.dependencies();
    let dependencies = if dependencies.is_empty() {
        "None".to_string()
    } else {
        dependencies.join(", ")
    };
    view! {
        <dl class="agent-version-facts">
            <dt>"Immutable version"</dt><dd>"v"{version.version_number}</dd>
            <dt>"Digest"</dt><dd><code>{version.content_digest}</code></dd>
            <dt>"Catalog release"</dt><dd>{version.catalog_release_id}" "<code>{version.catalog_release_digest}</code></dd>
            <dt>"Dependencies"</dt><dd>{dependencies}</dd>
            <dt>"Published"</dt><dd>{local_time(&version.published_at)}</dd>
        </dl>
    }
}

#[component]
pub fn AgentVersionsPage() -> impl IntoView {
    let revision = use_console().revision;
    let params = use_params_map();
    let navigate = use_navigate();
    let key = Memo::new(move |_| {
        (
            params.read().get("project_id").unwrap_or_default(),
            params.read().get("agent_id").unwrap_or_default(),
        )
    });
    let versions = RwSignal::new(None::<Vec<AgentVersionFields>>);
    let status = RwSignal::new("Loading immutable versions…");
    let (from_version, to_version) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    Effect::new(move |_| {
        let (project, agent) = key.get();
        let _ = revision.get();
        spawn_local(async move {
            match request_agent_versions(&project, &agent).await {
                Err(GraphqlError::SessionExpired) => status.set("Your session has expired."),
                Err(GraphqlError::Transport(_)) => {
                    status.set("We could not load immutable versions.")
                }
                Ok(None) => status.set("This agent is unavailable."),
                Ok(Some(list)) => {
                    let id = |index: usize| list.get(index).map(|version| version.id.clone());
                    from_version.set(id(1).or_else(|| id(0)).unwrap_or_default());
                    to_version.set(id(0).unwrap_or_default());
                    versions.set(Some(list));
                    status.set("");
                }
            }
        });
    });
    let base = move || {
        let (project, agent) = key.get();
        format!("/projects/{project}/agents/{agent}/versions")
    };
    let options = move |selected: RwSignal<String>| {
        versions.get().unwrap_or_default().into_iter().map(|version| {
        let id = version.id.clone();
        let chosen = { let id = id.clone(); move || selected.get() == id };
        view! { <option value=id selected=chosen>"v"{version.version_number}" · "{version.content_digest.chars().take(12).collect::<String>()}</option> }
    }).collect_view()
    };
    view! {
        <main class="agent-authoring-route" aria-labelledby="agent-version-list-title">
            {move || { let (project, agent) = key.get(); view! { <AgentTabs project_id=project agent_id=agent active="versions" /> } }}
            <PageHeader title_id="agent-version-list-title" title="Immutable versions".to_string() />
            {move || { let text = status.get(); (!text.is_empty()).then(|| view! { <p role=message_role(text)>{text}</p> }) }}
            {move || versions.with(|list| list.as_ref().is_some_and(Vec::is_empty)).then(|| view! { <p role="status">"No immutable versions have been published."</p> })}
            {move || versions.with(|list| list.as_ref().is_some_and(|list| list.len() > 1)).then(|| {
                let navigate = navigate.clone();
                view! {
                    <section class="agent-version-comparison-picker" aria-labelledby="agent-version-comparison-picker-title">
                        <h2 id="agent-version-comparison-picker-title">"Compare two immutable versions"</h2>
                        <p>"Choose exactly two publication facts. Comparison is read-only and never deploys either version."</p>
                        <label for="agent-version-compare-from">"From version"</label>
                        <select id="agent-version-compare-from" prop:value=move || from_version.get() on:change=move |event| from_version.set(event_target_value(&event))>{options(from_version)}</select>
                        <label for="agent-version-compare-to">"To version"</label>
                        <select id="agent-version-compare-to" prop:value=move || to_version.get() on:change=move |event| to_version.set(event_target_value(&event))>{options(to_version)}</select>
                        <button type="button" disabled=move || from_version.with(String::is_empty) || to_version.with(String::is_empty) || from_version.get() == to_version.get()
                            on:click=move |_| navigate(&format!("{}/compare?from={}&to={}", base(), encode(&from_version.get_untracked()), encode(&to_version.get_untracked())), Default::default())>"Compare selected versions"</button>
                    </section>
                }
            })}
            {move || versions.get().filter(|list| !list.is_empty()).map(|list| view! {
                <ul class="agent-version-list">{list.into_iter().map(|version| {
                    let href = format!("{}/{}", base(), version.id);
                    view! { <li><h2>"v"{version.version_number}</h2><VersionFacts version=version /><a href=href>"Open read-only version"</a></li> }
                }).collect_view()}</ul>
            })}
        </main>
    }
}

#[component]
pub fn AgentVersionDetailPage() -> impl IntoView {
    let revision = use_console().revision;
    let params = use_params_map();
    let key = Memo::new(move |_| {
        let read = params.read();
        (
            read.get("project_id").unwrap_or_default(),
            read.get("agent_id").unwrap_or_default(),
            read.get("version_id").unwrap_or_default(),
        )
    });
    let version = RwSignal::new(None::<AgentVersionFields>);
    let message = RwSignal::new("Loading immutable version…");
    Effect::new(move |_| {
        let (project, agent, id) = key.get();
        let _ = revision.get();
        spawn_local(async move {
            match request_agent_version(&project, &agent, &id).await {
                Err(GraphqlError::SessionExpired) => message.set("Your session has expired."),
                Err(GraphqlError::Transport(_)) => {
                    message.set("We could not load this immutable version.")
                }
                Ok(None) => message.set("This immutable version is unavailable."),
                Ok(Some(value)) => {
                    version.set(Some(value));
                    message.set("");
                }
            }
        });
    });
    view! {
        <main class="agent-authoring-route" aria-labelledby="agent-version-title">
            {move || { let text = message.get(); (!text.is_empty()).then(|| view! { <p role=message_role(text)>{text}</p> }) }}
            {move || version.get().map(|value| {
                let (project, agent, _) = key.get_untracked();
                let history = format!("/projects/{project}/agents/{agent}/versions");
                let id = value.id.clone();
                view! {
                    <PageHeader title_id="agent-version-title" title=format!("{} · v{}", value.display_name(), value.version_number) />
                    <VersionFacts version=value.clone() />
                    <p>"Published versions are configuration facts, not deployments."</p>
                    <p><a href=format!("{history}/{id}/deploy")>"Request deployment from this immutable version"</a></p>
                    <p><a href=format!("/projects/{project}/audit?resourceType=AGENT_VERSION&resourceId={}", encode(&id))>"Review version audit history"</a></p>
                    <JsonViewer value=value.canonical_document.0 label="Canonical published configuration" />
                    <a href=history>"Back to version history"</a>
                }
            })}
        </main>
    }
}

#[component]
pub fn AgentVersionComparisonPage() -> impl IntoView {
    let revision = use_console().revision;
    let params = use_params_map();
    let query = use_query_map();
    let key = Memo::new(move |_| {
        (
            params.read().get("project_id").unwrap_or_default(),
            params.read().get("agent_id").unwrap_or_default(),
            query.read().get("from"),
            query.read().get("to"),
        )
    });
    let comparison = RwSignal::new(None::<ComparedVersion>);
    let message = RwSignal::new("");
    let history = move || {
        key.with(|(project, agent, _, _)| format!("/projects/{project}/agents/{agent}/versions"))
    };
    Effect::new(move |_| {
        let (project, agent, from, to) = key.get();
        let _ = revision.get();
        comparison.set(None);
        let (Some(from), Some(to)) = (from, to) else {
            message.set("Choose two immutable versions to compare.");
            return;
        };
        message.set("");
        spawn_local(async move {
            match request_agent_version_comparison(&project, &agent, &from, &to).await {
                Err(GraphqlError::SessionExpired) => message.set("Your session has expired."),
                Err(GraphqlError::Transport(_)) => {
                    message.set("We could not compare these immutable versions.")
                }
                Ok(None) => message.set("This comparison is unavailable."),
                Ok(Some(value)) => comparison.set(Some(value)),
            }
        });
    });
    view! {
        <main class="agent-authoring-route" aria-labelledby="agent-version-compare-title">
            <PageHeader title_id="agent-version-compare-title" title="Immutable version comparison".to_string() />
            <p><a href=history>"Choose versions from immutable history"</a></p>
            {move || { let text = message.get(); (!text.is_empty()).then(|| view! { <p role="status">{text}</p> }) }}
            {move || comparison.get().and_then(|to| to.comparison.clone().map(|compared| (compared, to))).map(|(compared, to)| {
                let base = history();
                let from = compared.from;
                let changed = if compared.changed_sections.is_empty() { "None".to_string() } else { compared.changed_sections.join(", ") };
                let both = serde_json::json!({ "from": from.canonical_document.0, "to": to.canonical_document.0 });
                view! {
                    <p><a href=format!("{base}/{}", from.id)>"v"{from.version_number}</a>" → "<a href=format!("{base}/{}", to.id)>"v"{to.version_number}</a></p>
                    <p>"Changed sections: "{changed}</p>
                    <JsonViewer value=both label="Read-only version comparison" />
                }
            })}
        </main>
    }
}
