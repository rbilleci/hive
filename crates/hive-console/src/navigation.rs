//! Ports `ConsoleNavigation.tsx`: the capability-filtered resource tree and the organization switcher.

use crate::api::console::{
    has_any_capability, has_capability, selected_context, ConsoleContext, ConsoleProject,
    ORGANIZATION_SETTINGS_CAPABILITIES, PROJECT_SETTINGS_CAPABILITIES,
};
use crate::api::directory::{request_navigation_agents, DirectoryAgent};
use crate::dom::{elements, move_menu_focus};
use crate::shell::use_console;
use leptos::ev;
use leptos::html::{Button, Div, Ul};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_location, use_navigate};
use std::collections::BTreeSet;
use wasm_bindgen::JsCast;

#[derive(Clone)]
enum AgentState {
    Loading,
    Unavailable,
    Ready(Vec<DirectoryAgent>, bool),
}

/// `aria-current` for a tree link: the exact path, or any path beneath it for a `family` link.
fn route_current(pathname: &str, target: &str, family: bool) -> Option<&'static str> {
    (pathname == target || (family && pathname.starts_with(&format!("{target}/"))))
        .then_some("page")
}

#[component]
fn TreeLink(
    to: String,
    label: String,
    icon: &'static str,
    #[prop(optional)] family: bool,
    on_navigate: Callback<()>,
) -> impl IntoView {
    let pathname = use_location().pathname;
    let target = to.clone();
    let data_label = label.clone();
    view! {
        <a class="navigation-tree-link" data-label=data_label href=to on:click=move |_| on_navigate.run(())
            aria-current=move || route_current(&pathname.get(), &target, family)>
            <span class="navigation-tree-icon" aria-hidden="true">{icon}</span>
            <span class="navigation-tree-label">{label}</span>
        </a>
    }
}

/// The first page of a project's active agents, requested once when the project branch opens.
#[component]
fn AgentBranch(project_id: String, on_navigate: Callback<()>) -> impl IntoView {
    let state = RwSignal::new(AgentState::Loading);
    let requested = project_id.clone();
    spawn_local(async move {
        let next = match request_navigation_agents(&requested).await {
            Ok(Some(page)) => {
                let has_more = page.has_next_page();
                AgentState::Ready(page.rows, has_more)
            }
            Ok(None) | Err(_) => AgentState::Unavailable,
        };
        let _ = state.try_set(next);
    });
    move || match state.get() {
        AgentState::Loading => {
            view! { <li class="navigation-tree-state" role="status">"Loading agents…"</li> }
                .into_any()
        }
        AgentState::Unavailable => {
            view! { <li class="navigation-tree-state" role="status">"Agents unavailable."</li> }
                .into_any()
        }
        AgentState::Ready(agents, _) if agents.is_empty() => {
            view! { <li class="navigation-tree-state" role="status">"No agents."</li> }.into_any()
        }
        AgentState::Ready(agents, has_next_page) => {
            let base = format!("/projects/{project_id}/agents");
            let all = base.clone();
            view! {
                {agents.into_iter().map(|agent| view! {
                    <li class="navigation-tree-agent"><TreeLink to=format!("{base}/{}", agent.id) label=agent.display_name icon="●" family=true on_navigate=on_navigate /></li>
                }).collect_view()}
                {has_next_page.then(|| view! { <li><TreeLink to=all label="View all agents".to_string() icon="…" on_navigate=on_navigate /></li> })}
            }.into_any()
        }
    }
}

#[component]
fn ProjectBranch(
    context: ConsoleContext,
    project: ConsoleProject,
    open_projects: RwSignal<BTreeSet<String>>,
    on_navigate: Callback<()>,
) -> impl IntoView {
    let id = project.id.inner().to_string();
    let can = |code: &str| has_capability(&context, code, "PROJECT", &id);
    let can_view_configuration = can("CONFIGURATION.VIEW");
    let can_view_tools = can("TOOL_CONNECTION.VIEW");
    let can_view_agents = can("AGENT.VIEW");
    let can_view_deployments = can("DEPLOYMENT.VIEW");
    let can_view_evaluations = has_any_capability(
        &context,
        &["EVALUATION_DEFINITION.VIEW", "EVALUATION_RUN.VIEW"],
        "PROJECT",
        &id,
    );
    let can_settings = has_any_capability(&context, &PROJECT_SETTINGS_CAPABILITIES, "PROJECT", &id);
    // Organization-scoped AUDIT.VIEW already returns every descendant project's events, so the
    // project-level entry shows only for a principal whose only audit grant is project-scoped.
    let can_view_audit = !has_capability(
        &context,
        "AUDIT.VIEW",
        "ORGANIZATION",
        project.organization_id.inner(),
    ) && can("AUDIT.VIEW");
    let child_id = format!("project-{id}-children");
    let base = format!("/projects/{id}");
    let name = project.display_name.clone();
    let open = {
        let id = id.clone();
        Memo::new(move |_| open_projects.with(|all| all.contains(&id)))
    };
    let toggle = {
        let id = id.clone();
        move |_| {
            open_projects.update(|all| {
                if !all.remove(&id) {
                    all.insert(id.clone());
                }
            })
        }
    };
    let link = move |suffix: &str, label: &str, icon: &'static str, family: bool| {
        view! {
            <li><TreeLink to=format!("{base}{suffix}") label=label.to_string() icon=icon family=family on_navigate=on_navigate /></li>
        }
    };
    let children = {
        let (child_id, id, name, base) = (
            child_id.clone(),
            id.clone(),
            name.clone(),
            format!("/projects/{}", project.id.inner()),
        );
        move || {
            open.get().then(|| view! {
            <ul id=child_id.clone() class="navigation-tree-project-children">
                {can_view_agents.then(|| view! {
                    <li><TreeLink to=format!("{base}/agents") label="Agents".to_string() icon="◉" on_navigate=on_navigate />
                        <ul aria-label=format!("{name} agents")><AgentBranch project_id=id.clone() on_navigate=on_navigate /></ul></li>
                })}
                {can_view_deployments.then(|| link("/deployments", "Deployments", "⇧", true))}
                {can_view_evaluations.then(|| link("/evaluations", "Evaluations", "✓", true))}
                {can_view_audit.then(|| link("/audit", "Audit history", "◷", true))}
                {can_view_configuration.then(|| link("/prompts", "Prompts", "✎", true))}
                {can_view_configuration.then(|| link("/policies", "Policies", "≡", false))}
                {can_view_configuration.then(|| link("/model-profiles", "Model profiles", "◎", false))}
                {can_view_tools.then(|| link("/tools", "MCP servers", "⌁", false))}
                {can_settings.then(|| link("/settings", "Project settings", "⚙", false))}
            </ul>
        })
        }
    };
    let label = project.display_name.clone();
    view! {
        <li class="navigation-tree-project">
            <div class="navigation-tree-row">
                <button class="navigation-disclosure" type="button" aria-expanded=move || open.get().to_string() aria-controls=child_id
                    aria-label=move || format!("{} {label}", if open.get() { "Collapse" } else { "Expand" }) on:click=toggle>
                    <span aria-hidden="true">{move || if open.get() { "▾" } else { "▸" }}</span></button>
                <TreeLink to=format!("/projects/{}", project.id.inner()) label=project.display_name.clone() icon="◆" on_navigate=on_navigate />
            </div>
            {children}
        </li>
    }
}

#[component]
pub fn SidebarContents(
    #[prop(into)] collapsed: Signal<bool>,
    on_collapse: Callback<()>,
    on_navigate: Callback<()>,
    allow_collapse: bool,
) -> impl IntoView {
    let context = use_console().context;
    let pathname = use_location().pathname;
    let navigate = use_navigate();
    let selected =
        Memo::new(move |_| context.with(|value| selected_context(value, &pathname.get())));
    let open_projects = RwSignal::new(BTreeSet::<String>::new());
    Effect::new(move |_| {
        let (_, project) = selected.get();
        if !project.is_empty() {
            open_projects.update(|all| {
                all.insert(project);
            });
        }
    });

    let switcher_open = RwSignal::new(false);
    let switcher = NodeRef::<Div>::new();
    let switcher_trigger = NodeRef::<Button>::new();
    let switcher_menu = NodeRef::<Ul>::new();
    let menu_items = move || {
        switcher_menu
            .get_untracked()
            .map(|menu| elements(&menu, "[role=menuitemradio]"))
            .unwrap_or_default()
    };
    Effect::new(move |_| {
        if !switcher_open.get() {
            return;
        }
        request_animation_frame(move || {
            let items = menu_items();
            let checked = items
                .iter()
                .find(|item| item.get_attribute("aria-checked").as_deref() == Some("true"))
                .or(items.first());
            if let Some(target) = checked {
                let _ = target.focus();
            }
        });
        let outside = window_event_listener(ev::mousedown, move |event| {
            let inside = switcher
                .get_untracked()
                .zip(
                    event
                        .target()
                        .and_then(|target| target.dyn_into::<web_sys::Node>().ok()),
                )
                .is_some_and(|(root, node)| root.contains(Some(&node)));
            if !inside {
                switcher_open.set(false);
            }
        });
        let escape = window_event_listener(ev::keydown, move |event| {
            if event.key() == "Escape" {
                switcher_open.set(false);
                if let Some(trigger) = switcher_trigger.get_untracked() {
                    let _ = trigger.focus();
                }
            }
        });
        on_cleanup(move || {
            outside.remove();
            escape.remove();
        });
    });

    // Rebuilt only when the verified context or the selected organization changes; the open
    // branches and the switcher state live outside it and survive.
    let tree = move || {
        let value = context.get();
        let (selected_organization, _) = selected.get();
        let visible: Vec<_> = value
            .organizations
            .iter()
            .filter(|organization| {
                has_capability(
                    &value,
                    "ORGANIZATION.VIEW",
                    "ORGANIZATION",
                    organization.id.inner(),
                )
            })
            .cloned()
            .collect();
        let current = visible
            .iter()
            .find(|organization| organization.id.inner() == selected_organization)
            .or(visible.first())
            .cloned();
        let can_preferences = has_capability(
            &value,
            "PREFERENCES.UPDATE",
            "PRINCIPAL",
            value.principal.id.inner(),
        );
        let can_view_any_approvals = value.capabilities.iter().any(|capability| {
            capability.scope_type == "PROJECT" && capability.code == "DEPLOYMENT_APPROVAL.VIEW"
        });
        let initial = |name: &str| {
            name.chars()
                .next()
                .map_or("–".to_string(), |letter| letter.to_uppercase().collect())
        };
        let current_id = current
            .as_ref()
            .map(|organization| organization.id.inner().to_string());
        let many = visible.len() > 1;

        let options = {
            let (visible, current_id, navigate) =
                (visible.clone(), current_id.clone(), navigate.clone());
            move || {
                switcher_open.get().then(|| view! {
                <ul node_ref=switcher_menu id="organization-switcher-options" class="organization-switcher-options" role="menu" aria-label="Accessible organizations"
                    on:keydown=move |event| { if move_menu_focus(&menu_items(), &event.key()) { event.prevent_default(); } }>
                    {visible.iter().map(|organization| {
                        let checked = current_id.as_deref() == Some(organization.id.inner());
                        let (target, navigate) = (format!("/organizations/{}/projects", organization.id.inner()), navigate.clone());
                        view! {
                            <li role="none"><button type="button" role="menuitemradio" aria-checked=checked.to_string()
                                on:click=move |_| { switcher_open.set(false); on_navigate.run(()); navigate(&target, Default::default()); }>
                                <span class="organization-switcher-avatar" aria-hidden="true">{initial(&organization.display_name)}</span><span>{organization.display_name.clone()}</span>
                                {checked.then(|| view! { <span aria-hidden="true">"✓"</span> })}
                            </button></li>
                        }
                    }).collect_view()}
                </ul>
            })
            }
        };
        let switcher_view = {
            let current = current.clone();
            move || {
                (!collapsed.get()).then(|| {
                let name = current.as_ref().map_or("No organization available".to_string(), |organization| organization.display_name.clone());
                let avatar = current.as_ref().map_or("–".to_string(), |organization| initial(&organization.display_name));
                view! {
                    <div node_ref=switcher class="organization-switcher">
                        <button node_ref=switcher_trigger class="organization-switcher-trigger" type="button" aria-haspopup="menu"
                            aria-expanded=move || switcher_open.get().to_string() aria-controls="organization-switcher-options" disabled=!many
                            on:keydown=move |event| { if matches!(event.key().as_str(), "ArrowDown" | "ArrowUp") { event.prevent_default(); switcher_open.set(true); } }
                            on:click=move |_| switcher_open.update(|open| *open = !*open)>
                            <span class="organization-switcher-avatar" aria-hidden="true">{avatar}</span>
                            <span><small>"Organization"</small><strong>{name}</strong></span>
                            {many.then(|| view! { <span aria-hidden="true">"⌄"</span> })}
                        </button>
                        {options.clone()}
                    </div>
                }
            })
            }
        };

        let root = match current {
            None => view! { <li class="navigation-tree-state" role="status">"No organizations available."</li> }.into_any(),
            Some(organization) => {
                let base = format!("/organizations/{}", organization.id.inner());
                let projects: Vec<_> = organization.projects.iter().filter(|project| has_capability(&value, "PROJECT.VIEW", "PROJECT", project.id.inner())).cloned().collect();
                let can_approvals = organization.projects.iter().any(|project| has_capability(&value, "DEPLOYMENT_APPROVAL.VIEW", "PROJECT", project.id.inner()));
                let organization_can = |code: &str| has_capability(&value, code, "ORGANIZATION", organization.id.inner());
                let link = |suffix: &str, label: &str, icon: &'static str, family: bool| view! {
                    <li><TreeLink to=format!("{base}{suffix}") label=label.to_string() icon=icon family=family on_navigate=on_navigate /></li>
                };
                view! {
                    <li class="navigation-tree-projects-section">
                        <TreeLink to=format!("{base}/projects") label="Projects".to_string() icon="◆" family=true on_navigate=on_navigate />
                        <ul aria-label=format!("{} projects", organization.display_name)>
                            {projects.is_empty().then(|| view! { <li class="navigation-tree-state" role="status">"No projects available."</li> })}
                            {projects.into_iter().map(|project| view! { <ProjectBranch context=value.clone() project=project open_projects=open_projects on_navigate=on_navigate /> }).collect_view()}
                        </ul>
                    </li>
                    {organization_can("CATALOG.VIEW").then(|| view! { {link("/catalog", "Catalog", "▦", false)}{link("/environments", "Environments", "□", false)} })}
                    {can_approvals.then(|| link("/approvals", "Approvals", "✓", true))}
                    {organization_can("AUDIT.VIEW").then(|| link("/audit", "Audit history", "◷", true))}
                    {has_any_capability(&value, &ORGANIZATION_SETTINGS_CAPABILITIES, "ORGANIZATION", organization.id.inner())
                        .then(|| link("/settings", "Organization settings", "⚙", false))}
                }.into_any()
            }
        };

        view! {
            {switcher_view}
            <nav class="console-primary-nav" aria-label="Resource navigation">
                <ul class="navigation-tree navigation-tree-root">{root}</ul>
                {can_view_any_approvals.then(|| view! { <ul class="navigation-tree navigation-tree-account"><li><TreeLink to="/approvals".to_string() label="All approvals".to_string() icon="✓" family=true on_navigate=on_navigate /></li></ul> })}
                {can_preferences.then(|| view! { <ul class="navigation-tree navigation-tree-account"><li><TreeLink to="/preferences".to_string() label="Preferences".to_string() icon="◐" on_navigate=on_navigate /></li></ul> })}
            </nav>
            <p class="console-principal"><span aria-hidden="true">"●"</span><span class="navigation-tree-label">
                "Access verified"<br /><strong>{value.principal.display_name.clone()}</strong></span></p>
        }
    };

    let collapse_label = move || {
        if collapsed.get() {
            "Expand sidebar"
        } else {
            "Collapse sidebar"
        }
    };
    view! {
        <div class="console-sidebar-inner" data-collapsed=move || collapsed.get().then_some("true")>
            <div class="console-sidebar-brand">
                <a class="console-mark" href="/" on:click=move |_| on_navigate.run(()) aria-label="Hive home" data-label="Hive">
                    <span aria-hidden="true">"✦"</span><span class="navigation-tree-label">"Hive"</span></a>
                {allow_collapse.then(|| view! {
                    <button class="console-collapse" type="button" on:click=move |_| on_collapse.run(()) aria-label=collapse_label data-label=collapse_label>
                        <span aria-hidden="true">{move || if collapsed.get() { "»" } else { "«" }}</span></button>
                })}
            </div>
            {tree}
        </div>
    }
}
