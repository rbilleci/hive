//! These descriptors declare approved transports and allowlists; the console never connects to,
//! discovers, tests, or executes a server.

use crate::api::configuration::{
    create_mcp_server, request_known, request_mcp_servers, update_mcp_server,
    CreateProjectMcpServerInput, McpServerConfiguration, UpdateProjectMcpServerInput,
};
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_params_map;
use std::collections::BTreeSet;

#[derive(Clone, PartialEq)]
enum Mode {
    List,
    Create,
    Edit(Box<McpServerConfiguration>),
}

#[derive(Clone, PartialEq)]
struct Draft {
    server_id: String,
    name: String,
    definition: String,
    environment: String,
    enabled: bool,
    transport_type: String,
    command: String,
    arguments: String,
    remote_url: String,
    redacted_bindings: String,
    tools: String,
    resources: String,
    prompts: String,
    lifecycle_status: String,
}

impl Draft {
    fn blank() -> Self {
        Self {
            server_id: String::new(),
            name: String::new(),
            definition: String::new(),
            environment: "DEVELOPMENT".to_string(),
            enabled: true,
            transport_type: "STDIO".to_string(),
            command: String::new(),
            arguments: String::new(),
            remote_url: String::new(),
            redacted_bindings: String::new(),
            tools: String::new(),
            resources: String::new(),
            prompts: String::new(),
            lifecycle_status: "ACTIVE".to_string(),
        }
    }

    fn of(server: &McpServerConfiguration) -> Self {
        Self {
            server_id: server.server_id.clone().unwrap_or_default(),
            name: server.name.clone(),
            definition: format!(
                "tool:{}@{}",
                server.definition_identity, server.definition_version
            ),
            environment: server.environment.clone(),
            enabled: server.enabled.unwrap_or(true),
            transport_type: server
                .transport_type
                .clone()
                .unwrap_or_else(|| "STDIO".to_string()),
            command: server.stdio_command.clone().unwrap_or_default(),
            arguments: server
                .arguments
                .iter()
                .map(|value| safe_argument(value))
                .collect::<Vec<_>>()
                .join("\n"),
            remote_url: safe_remote_url(server.remote_url.as_deref()),
            redacted_bindings: server.redacted_bindings().join("\n"),
            tools: server.tools().join("\n"),
            resources: server.resources().join("\n"),
            prompts: server.prompts().join("\n"),
            lifecycle_status: server.lifecycle_status.clone(),
        }
    }
}

/// Unique, trimmed, non-empty lines in sorted order.
fn lines(value: &str) -> Vec<String> {
    value
        .split('\n')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

const SENSITIVE_NAMES: [&str; 12] = [
    "password",
    "passwd",
    "secret",
    "token",
    "api-key",
    "api_key",
    "apikey",
    "authorization",
    "credential",
    "bearer",
    "basic",
    "header",
];

fn sensitive_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    SENSITIVE_NAMES.iter().any(|word| lower.contains(word)) || lower.contains("env")
}

/// Never echoes an argument that names or carries a credential, even one stored before validation existed.
fn safe_argument(value: &str) -> String {
    let stripped = value.trim();
    let name = stripped
        .trim_start_matches('-')
        .split(['=', ':'])
        .next()
        .unwrap_or_default();
    let lower = stripped.to_lowercase();
    let scheme = ["bearer", "basic"].iter().any(|scheme| {
        lower
            .strip_prefix(scheme)
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    });
    if scheme || sensitive_name(name) {
        "[redacted]".to_string()
    } else {
        value.to_string()
    }
}

fn safe_remote_url(value: Option<&str>) -> String {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return String::new();
    };
    const UNSAFE: &str = "[redacted unsafe URL]";
    let Ok(url) = web_sys::Url::new(value) else {
        return UNSAFE.to_string();
    };
    let unsafe_query = js_sys::try_iter(&url.search_params().keys())
        .ok()
        .flatten()
        .is_some_and(|keys| {
            keys.filter_map(Result::ok)
                .filter_map(|key| key.as_string())
                .any(|name| sensitive_name(&name))
        });
    if url.protocol() == "https:"
        && url.username().is_empty()
        && url.password().is_empty()
        && !unsafe_query
        && url.hash().is_empty()
    {
        value.to_string()
    } else {
        UNSAFE.to_string()
    }
}

fn transport_summary(server: &McpServerConfiguration) -> String {
    match server.transport_type.as_deref() {
        Some("STDIO") => format!(
            "STDIO · {}{}",
            server.stdio_command.clone().unwrap_or_default(),
            if server.arguments.is_empty() {
                String::new()
            } else {
                format!(
                    " {}",
                    server
                        .arguments
                        .iter()
                        .map(|value| safe_argument(value))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            }
        ),
        Some("REMOTE") => format!("Remote · {}", safe_remote_url(server.remote_url.as_deref())),
        _ => "Not configured (legacy metadata)".to_string(),
    }
}

fn status_icon(status: &str) -> &'static str {
    match status {
        "NOT_CHECKED" => "○",
        "DISABLED" => "Ⅱ",
        "ARCHIVED" => "□",
        _ => "!",
    }
}

fn joined_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "None".to_string()
    } else {
        values.join(", ")
    }
}

#[component]
pub fn McpServersPage() -> impl IntoView {
    let console = use_console();
    let params = use_params_map();
    let project_id = Memo::new(move |_| params.read().get("project_id").unwrap_or_default());
    let can_write = Memo::new(move |_| {
        let project = project_id.get();
        console.context.with(|context| {
            context.capabilities.iter().any(|capability| {
                capability.code == "TOOL_CONNECTION.UPDATE"
                    && capability.scope_id.inner() == project
            })
        })
    });
    let servers = RwSignal::new(None::<Vec<McpServerConfiguration>>);
    let definitions = RwSignal::new(Vec::<(String, String)>::new());
    let mode = RwSignal::new(Mode::List);
    let draft = RwSignal::new(Draft::blank());
    let unavailable = RwSignal::new(false);
    let error = RwSignal::new(String::new());

    Effect::new(move |_| {
        let project = project_id.get();
        let organization = console.context.with_untracked(|context| {
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
        });
        mode.set(Mode::List);
        draft.set(Draft::blank());
        servers.set(None);
        unavailable.set(false);
        error.set(String::new());
        spawn_local(async move {
            let (listed, known) = (
                request_mcp_servers(&project).await,
                request_known(&project, &organization).await,
            );
            match (listed, known) {
                (Ok(None), Ok(_)) => {
                    servers.set(Some(Vec::new()));
                    unavailable.set(true);
                }
                (Ok(Some(list)), Ok(known)) => {
                    definitions.set(
                        known
                            .catalog
                            .map(|release| release.catalog_definitions.nodes)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|definition| definition.definition_kind == "tool")
                            .map(|definition| {
                                (
                                    format!("tool:{}@{}", definition.identity, definition.version),
                                    format!(
                                        "{} · {}@{}",
                                        definition.display_name,
                                        definition.identity,
                                        definition.version
                                    ),
                                )
                            })
                            .collect(),
                    );
                    servers.set(Some(list));
                }
                _ => {
                    servers.set(Some(Vec::new()));
                    error.set("MCP server metadata could not be loaded.".to_string());
                }
            }
        });
    });

    let cancel = move || {
        mode.set(Mode::List);
        draft.set(Draft::blank());
        error.set(String::new());
    };
    let begin_create = move || {
        mode.set(Mode::Create);
        draft.set(Draft {
            definition: definitions.with_untracked(|all| {
                all.first()
                    .map(|(value, _)| value.clone())
                    .unwrap_or_default()
            }),
            ..Draft::blank()
        });
        error.set(String::new());
    };
    let begin_edit = move |server: McpServerConfiguration| {
        draft.set(Draft::of(&server));
        mode.set(Mode::Edit(Box::new(server)));
        error.set(String::new());
    };

    let save = move |event: ev::SubmitEvent| {
        event.prevent_default();
        let (current, project) = (draft.get_untracked(), project_id.get_untracked());
        let stdio = current.transport_type == "STDIO";
        let (command, arguments, remote_url) = (
            stdio.then(|| current.command.clone()),
            if stdio {
                lines(&current.arguments)
            } else {
                Vec::new()
            },
            (!stdio).then(|| current.remote_url.clone()),
        );
        let editing = mode.get_untracked();
        spawn_local(async move {
            let result = match editing {
                Mode::Edit(server) => {
                    update_mcp_server(UpdateProjectMcpServerInput {
                        project_id: project.as_str().into(),
                        id: server.id.as_str().into(),
                        expected_revision: server.revision,
                        name: current.name,
                        definition: current.definition,
                        environment: current.environment,
                        enabled: current.enabled,
                        transport_type: current.transport_type,
                        command,
                        arguments,
                        remote_url,
                        redacted_bindings: lines(&current.redacted_bindings),
                        tools: lines(&current.tools),
                        resources: lines(&current.resources),
                        prompts: lines(&current.prompts),
                        lifecycle_status: current.lifecycle_status,
                    })
                    .await
                }
                _ => {
                    create_mcp_server(CreateProjectMcpServerInput {
                        project_id: project.as_str().into(),
                        server_id: current.server_id,
                        name: current.name,
                        definition: current.definition,
                        environment: current.environment,
                        enabled: current.enabled,
                        transport_type: current.transport_type,
                        command,
                        arguments,
                        remote_url,
                        redacted_bindings: lines(&current.redacted_bindings),
                        tools: lines(&current.tools),
                        resources: lines(&current.resources),
                        prompts: lines(&current.prompts),
                    })
                    .await
                }
            };
            match result {
                Err(_) => error.set("The MCP server was not saved.".to_string()),
                Ok(payload) => match (payload.problems.first(), payload.mcp_server) {
                    (Some(first), _) => error.set(first.message.clone()),
                    (None, None) => error.set("The MCP server was not saved.".to_string()),
                    (None, Some(saved)) => {
                        servers.update(|all| {
                            let list = all.get_or_insert_with(Vec::new);
                            list.retain(|entry| entry.id != saved.id);
                            list.push(saved);
                            list.sort_by(|left, right| {
                                left.name.to_lowercase().cmp(&right.name.to_lowercase())
                            });
                        });
                        cancel();
                    }
                },
            }
        });
    };

    let text_field = move |label: &'static str,
                           read: fn(&Draft) -> String,
                           write: fn(&mut Draft, String),
                           required: bool,
                           disabled: bool,
                           pattern: Option<&'static str>,
                           kind: &'static str| {
        view! {
            <label>{label}<input required=required disabled=disabled pattern=pattern type=kind prop:value=move || draft.with(read) on:input=move |event| draft.update(|next| write(next, event_target_value(&event))) /></label>
        }
    };
    let area = move |label: &'static str,
                     rows: &'static str,
                     placeholder: Option<&'static str>,
                     read: fn(&Draft) -> String,
                     write: fn(&mut Draft, String)| {
        view! {
            <label>{label}<textarea rows=rows placeholder=placeholder prop:value=move || draft.with(read) on:input=move |event| draft.update(|next| write(next, event_target_value(&event))) /></label>
        }
    };

    let form = move || {
        let editing = match mode.get() {
            Mode::List => return None,
            Mode::Create => None,
            Mode::Edit(server) => Some(server),
        };
        let creating = editing.is_none();
        Some(view! {
            <form class="configuration-form mcp-server-form" aria-label=if creating { "Add MCP server" } else { "Edit MCP server" } on:submit=save>
                <h2>{if creating { "Add MCP server" } else { "Edit MCP server" }}</h2>
                {match &editing { None => view! { <p>"Create a new inert server descriptor."</p> }.into_any(), Some(server) => view! { <p>"Editing "<strong>{server.name.clone()}</strong>" at revision "{server.revision}"."</p> }.into_any() }}
                {text_field("Server ID", |draft| draft.server_id.clone(), |draft, value| draft.server_id = value, true, !creating, Some("[a-z][a-z0-9-]{0,62}"), "text")}
                {text_field("Display name", |draft| draft.name.clone(), |draft, value| draft.name = value, true, false, None, "text")}
                <label>"Approved tool definition"<select required prop:value=move || draft.with(|draft| draft.definition.clone()) on:change=move |event| draft.update(|next| next.definition = event_target_value(&event))>
                    <option value="" disabled>"No approved definition selected"</option>
                    {move || definitions.get().into_iter().map(|(value, label)| { let chosen = value.clone(); view! { <option value=value selected=move || draft.with(|draft| draft.definition == chosen)>{label}</option> } }).collect_view()}</select></label>
                {move || definitions.with(Vec::is_empty).then(|| view! { <p role="status">"No approved tool definitions are available from the local catalog."</p> })}
                <label>"Environment"<select prop:value=move || draft.with(|draft| draft.environment.clone()) on:change=move |event| draft.update(|next| next.environment = event_target_value(&event))>
                    {["DEVELOPMENT", "STAGING", "PRODUCTION"].into_iter().map(|environment| view! { <option selected=move || draft.with(|draft| draft.environment == environment)>{environment}</option> }).collect_view()}</select></label>
                <label class="checkbox-row"><input type="checkbox" prop:checked=move || draft.with(|draft| draft.enabled) on:change=move |event| draft.update(|next| next.enabled = event_target_checked(&event)) />" Enabled"</label>
                <fieldset><legend>"Transport"</legend>
                    <label><input type="radio" name="transport" prop:checked=move || draft.with(|draft| draft.transport_type == "STDIO") on:change=move |_| draft.update(|next| next.transport_type = "STDIO".to_string()) />" Local STDIO"</label>
                    <label><input type="radio" name="transport" prop:checked=move || draft.with(|draft| draft.transport_type == "REMOTE") on:change=move |_| draft.update(|next| next.transport_type = "REMOTE".to_string()) />" Remote HTTPS"</label></fieldset>
                {move || if draft.with(|draft| draft.transport_type == "STDIO") { view! {
                    {text_field("Command", |draft| draft.command.clone(), |draft, value| draft.command = value, true, false, None, "text")}
                    {area("Arguments (one non-secret argument per line)", "4", None, |draft| draft.arguments.clone(), |draft, value| draft.arguments = value)}
                }.into_any() } else { text_field("Remote URL", |draft| draft.remote_url.clone(), |draft, value| draft.remote_url = value, true, false, Some("https://.*"), "url").into_any() }}
                {area("Redacted binding references (one reference per line)", "3", Some("redacted://local/service-token"), |draft| draft.redacted_bindings.clone(), |draft, value| draft.redacted_bindings = value)}
                <p>"Only redacted binding metadata is accepted; never enter a credential, raw header, or environment value."</p>
                {area("Allowed tools (one ID per line)", "3", None, |draft| draft.tools.clone(), |draft, value| draft.tools = value)}
                {area("Allowed resources (one ID per line)", "3", None, |draft| draft.resources.clone(), |draft, value| draft.resources = value)}
                {area("Allowed prompts (one ID per line)", "3", None, |draft| draft.prompts.clone(), |draft, value| draft.prompts = value)}
                {(!creating).then(|| view! { <label>"Lifecycle"<select prop:value=move || draft.with(|draft| draft.lifecycle_status.clone()) on:change=move |event| draft.update(|next| next.lifecycle_status = event_target_value(&event))>
                    <option value="ACTIVE" selected=move || draft.with(|draft| draft.lifecycle_status == "ACTIVE")>"Active"</option><option value="ARCHIVED" selected=move || draft.with(|draft| draft.lifecycle_status == "ARCHIVED")>"Archived"</option></select></label> })}
                <div class="configuration-actions"><button type="submit" disabled=move || draft.with(|draft| draft.definition.is_empty())>{if creating { "Create MCP server" } else { "Save MCP server" }}</button>
                    <button type="button" on:click=move |_| cancel()>"Cancel"</button></div>
            </form>
        })
    };

    view! {
        <main class="console-page-frame configuration-page" aria-labelledby="mcp-title">
            <PageHeader title_id="mcp-title" title="MCP servers".to_string()
                description="These project descriptors declare approved transports and allowlists. This console never connects, discovers, tests, refreshes, checks health, or executes a server.">
                {move || (mode.get() == Mode::List && can_write.get() && !unavailable.get()).then(|| view! { <button class="primary-action" type="button" on:click=move |_| begin_create()>"Add MCP server"</button> })}
            </PageHeader>
            {move || { let text = error.get(); (!text.is_empty()).then(|| view! { <p class="configuration-problem" role="alert">{text}</p> }) }}
            {move || (mode.get() == Mode::List).then(|| view! {
                {match servers.get() {
                    None => view! { <p role="status">"Loading MCP servers…"</p> }.into_any(),
                    Some(_) if unavailable.get() => view! { <p role="alert">"This route is unavailable or you no longer have access."</p> }.into_any(),
                    Some(list) if list.is_empty() => view! { <p role="status">"No MCP servers are configured for this project."</p> }.into_any(),
                    Some(list) => view! { <ul class="mcp-server-list">{list.into_iter().map(|server| { let editable = server.clone(); view! {
                        <li><div><h2>{server.name.clone()}</h2>
                            <p><strong>"ID:"</strong>" "<code>{server.server_id.clone().unwrap_or_default()}</code>" · "{format!("{}@{}", server.definition_identity, server.definition_version)}" · "{server.environment.clone()}</p>
                            <p class="status-with-text"><span aria-hidden="true">{status_icon(&server.status)}</span>{server.status.replacen('_', " ", 1)}</p>
                            <p><strong>"Transport:"</strong>" "{transport_summary(&server)}</p><p><strong>"Bindings:"</strong>" "{joined_or_none(&server.redacted_bindings())}</p>
                            <details><summary>"Declared capabilities"</summary><dl class="mcp-capabilities">
                                <dt>"Tools"</dt><dd>{joined_or_none(&server.tools())}</dd><dt>"Resources"</dt><dd>{joined_or_none(&server.resources())}</dd>
                                <dt>"Prompts"</dt><dd>{joined_or_none(&server.prompts())}</dd><dt>"Dependent resources"</dt><dd>{joined_or_none(&server.dependent_resources)}</dd></dl></details>
                        </div>{can_write.get().then(|| view! { <button type="button" aria-label=format!("Edit {}", server.name) on:click=move |_| begin_edit(editable.clone())>"Edit"</button> })}</li>
                    } }).collect_view()}</ul> }.into_any(),
                }}
                {(!can_write.get()).then(|| view! { <p role="status">"Read-only: your current capability does not allow MCP server changes."</p> })}
            })}
            {form}
        </main>
    }
}
