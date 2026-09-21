//! The shared shell rechecks server-owned access on navigation, focus,
//! reconnect, and a fixed interval, then frames every page with the sidebar and the drawer.

use crate::api::console::{
    has_capability, has_view_capability, request_console, requested_scope,
    save_display_preferences, ConsoleAccess, ConsoleContext, DisplayPreferences,
};
use crate::dom::{contain_focus, local_storage, session_storage};
use crate::navigation::SidebarContents;
use leptos::ev;
use leptos::html::{Button, Div};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{Outlet, Redirect, A};
use leptos_router::hooks::use_location;
use wasm_bindgen::prelude::*;

const SIDEBAR_WIDTH_KEY: &str = "hive.console-sidebar-width";
const DEFAULT_SIDEBAR_WIDTH: i32 = 304;
const MINIMUM_SIDEBAR_WIDTH: i32 = 240;
const MAXIMUM_SIDEBAR_WIDTH: i32 = 420;

fn selection_key(principal_id: &str) -> String {
    format!("hive.console-context.{principal_id}")
}

/// What every page under the shell can read. `context` and `preferences` hold their last verified
/// values; `revision` changes when the verified access changes, and pages reload on it.
#[derive(Clone, Copy)]
pub struct ConsoleStore {
    pub context: Memo<ConsoleContext>,
    pub preferences: RwSignal<DisplayPreferences>,
    pub revision: Memo<String>,
    /// Rechecks console access now; the administration pages call it after a mutation changes it.
    pub refresh: Callback<()>,
    pub drawer_open: RwSignal<bool>,
    pub drawer_trigger: NodeRef<Button>,
}

pub fn use_console() -> ConsoleStore {
    expect_context::<ConsoleStore>()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Loading,
    Ready,
    SessionError,
    Error,
}

/// Remembers the last organization and project this principal opened.
fn save_selection(context: &ConsoleContext, pathname: &str) {
    let Some(scope) = requested_scope(pathname) else {
        return;
    };
    let (organization, project) = if scope.scope_type == "ORGANIZATION" {
        (Some(scope.id.to_string()), None)
    } else {
        let owner = context
            .organizations
            .iter()
            .find(|organization| {
                organization
                    .projects
                    .iter()
                    .any(|project| project.id.inner() == scope.id)
            })
            .map(|organization| organization.id.inner().to_string());
        (owner, Some(scope.id.to_string()))
    };
    let (Some(organization), Some(storage)) = (organization, local_storage()) else {
        return;
    };
    let value =
        serde_json::json!({ "selectedOrganization": organization, "selectedProject": project });
    let _ = storage.set_item(
        &selection_key(context.principal.id.inner()),
        &value.to_string(),
    );
}

/// The stylesheet reads these three attributes on `<html>`.
fn apply_visual_preferences(preferences: &DisplayPreferences) {
    let Some(root) = document().document_element() else {
        return;
    };
    let theme = if preferences.color_scheme == "SYSTEM" {
        let dark = window()
            .match_media("(prefers-color-scheme: dark)")
            .ok()
            .flatten()
            .is_some_and(|media| media.matches());
        if dark {
            "dark".to_string()
        } else {
            "light".to_string()
        }
    } else {
        preferences.color_scheme.to_lowercase()
    };
    let _ = root.set_attribute("data-theme", &theme);
    let _ = root.set_attribute("data-density", &preferences.density.to_lowercase());
    let _ = root.set_attribute(
        "data-sidebar-state",
        &preferences.sidebar_state.to_lowercase(),
    );
}

#[component]
pub fn ConsoleShell() -> impl IntoView {
    let location = use_location();
    let access = RwSignal::new(Access::Loading);
    let verified = RwSignal::new(None::<ConsoleContext>);
    let preferences = RwSignal::new(DisplayPreferences::light_defaults());
    let serial = StoredValue::new(0_u32);
    let drawer_open = RwSignal::new(false);
    let drawer_trigger = NodeRef::<Button>::new();

    let load = move || {
        let request = serial.get_value() + 1;
        serial.set_value(request);
        spawn_local(async move {
            let result = request_console().await;
            if serial.try_get_value() != Some(request) {
                return;
            }
            match result {
                Ok(ConsoleAccess::Ready(context, stored)) => {
                    // A preview made on the preferences page survives a background recheck.
                    if access.get_untracked() != Access::Ready {
                        preferences.set(stored);
                    }
                    verified.set(Some(context));
                    access.set(Access::Ready);
                }
                Ok(ConsoleAccess::SessionError) => access.set(Access::SessionError),
                Err(_) => access.set(Access::Error),
            }
        });
    };

    Effect::new(move |_| {
        let _ = location.pathname.get();
        load();
    });
    let visible = || document().visibility_state() == web_sys::VisibilityState::Visible;
    let on_focus = window_event_listener(ev::focus, move |_| {
        if visible() {
            load()
        }
    });
    let on_online = window_event_listener(ev::online, move |_| load());
    let on_visibility = window_event_listener(ev::visibilitychange, move |_| {
        if visible() {
            load()
        }
    });
    let interval = set_interval_with_handle(load, std::time::Duration::from_secs(15)).ok();
    on_cleanup(move || {
        on_focus.remove();
        on_online.remove();
        on_visibility.remove();
        if let Some(interval) = interval {
            interval.clear();
        }
    });

    // `SYSTEM` follows the operating system while the page is open.
    let scheme_watch =
        StoredValue::new_local(None::<(web_sys::MediaQueryList, Closure<dyn FnMut()>)>);
    if let Ok(Some(media)) = window().match_media("(prefers-color-scheme: dark)") {
        let callback = Closure::<dyn FnMut()>::new(move || {
            if let Some(current) = preferences.try_get_untracked() {
                apply_visual_preferences(&current);
            }
        });
        let _ = media.add_event_listener_with_callback("change", callback.as_ref().unchecked_ref());
        scheme_watch.set_value(Some((media, callback)));
    }
    on_cleanup(move || {
        if let Some((media, callback)) = scheme_watch.try_update_value(Option::take).flatten() {
            let _ = media
                .remove_event_listener_with_callback("change", callback.as_ref().unchecked_ref());
        }
    });
    Effect::new(move |_| {
        if access.get() == Access::Ready {
            apply_visual_preferences(&preferences.get());
        }
    });

    let context = Memo::new(move |_| {
        verified.get().unwrap_or_else(|| ConsoleContext {
            principal: crate::api::console::ConsolePrincipal {
                id: "".into(),
                display_name: String::new(),
            },
            organizations: Vec::new(),
            capabilities: Vec::new(),
            revision: String::new(),
        })
    });
    let revision = Memo::new(move |_| context.with(|value| value.revision.clone()));
    provide_context(ConsoleStore {
        context,
        preferences,
        revision,
        refresh: Callback::new(move |()| load()),
        drawer_open,
        drawer_trigger,
    });

    let permitted = Memo::new(move |_| {
        context.with(|value| has_view_capability(value, &location.pathname.get()))
    });
    Effect::new(move |_| {
        if access.get() == Access::Ready && permitted.get() {
            context.with(|value| save_selection(value, &location.pathname.get()));
        }
    });

    // A background recheck stores `Ready` again; only a change of state may rebuild the layout,
    // or every recheck would remount the page and discard its unsaved edits.
    let access = Memo::new(move |_| access.get());
    move || {
        match access.get() {
        Access::Loading => view! {
            <main class="console-loading" aria-label="Loading shared console"><p role="status">"Checking your console access…"</p></main>
        }.into_any(),
        Access::SessionError => view! { <Redirect path="/session-error" /> }.into_any(),
        Access::Error => view! {
            <main class="console-loading"><p role="alert">"We could not verify console access. Try again."</p>
                <button type="button" on:click=move |_| { let _ = window().location().reload(); }>"Retry"</button></main>
        }.into_any(),
        Access::Ready if !permitted.get() => view! { <Redirect path="/access-denied" /> }.into_any(),
        Access::Ready => view! { <ConsoleLayout /> }.into_any(),
    }
    }
}

#[component]
fn ConsoleLayout() -> impl IntoView {
    let console = use_console();
    let location = use_location();
    let (context, preferences, drawer_open) =
        (console.context, console.preferences, console.drawer_open);
    let preference_error = RwSignal::new(String::new());
    let drawer = NodeRef::<Div>::new();
    let saved_width = session_storage()
        .and_then(|storage| storage.get_item(SIDEBAR_WIDTH_KEY).ok().flatten())
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|width| (MINIMUM_SIDEBAR_WIDTH..=MAXIMUM_SIDEBAR_WIDTH).contains(width));
    let sidebar_width = RwSignal::new(saved_width.unwrap_or(DEFAULT_SIDEBAR_WIDTH));
    let collapsed =
        Memo::new(move |_| preferences.with(|value| value.sidebar_state == "COLLAPSED"));
    let resize = move |width: f64| {
        let next = (width.round() as i32).clamp(MINIMUM_SIDEBAR_WIDTH, MAXIMUM_SIDEBAR_WIDTH);
        sidebar_width.set(next);
        if let Some(storage) = session_storage() {
            let _ = storage.set_item(SIDEBAR_WIDTH_KEY, &next.to_string());
        }
    };

    let close_drawer = move || {
        drawer_open.set(false);
        request_animation_frame(move || {
            if let Some(trigger) = console.drawer_trigger.get_untracked() {
                let _ = trigger.focus();
            }
        });
    };
    Effect::new(move |_| {
        if !drawer_open.get() {
            return;
        }
        contain_focus(
            move || drawer.get_untracked().map(Into::into),
            "a[href], button:not([disabled]), [tabindex]:not([tabindex=\"-1\"])",
            close_drawer,
        );
    });
    Effect::new(move |previous: Option<String>| {
        let path = location.pathname.get();
        if previous.is_some_and(|previous| previous != path) && drawer_open.get_untracked() {
            close_drawer();
        }
        path
    });

    let toggle_collapsed = move || {
        let previous = preferences.get_untracked();
        let mut next = previous.clone();
        next.sidebar_state = if collapsed.get_untracked() {
            "EXPANDED"
        } else {
            "COLLAPSED"
        }
        .to_string();
        preference_error.set(String::new());
        preferences.set(next.clone());
        let may_save = context.with_untracked(|value| {
            has_capability(
                value,
                "PREFERENCES.UPDATE",
                "PRINCIPAL",
                value.principal.id.inner(),
            )
        });
        if !may_save {
            return;
        }
        spawn_local(async move {
            match save_display_preferences(&next).await {
                Ok(saved) => preferences.set(saved),
                Err(message) => {
                    preferences.set(previous);
                    preference_error.set(message);
                }
            }
        });
    };

    view! {
        <div class="console-app" style=move || format!("--sidebar-width: {}px", sidebar_width.get())>
            <aside id="console-sidebar" class="console-sidebar" aria-label="Primary navigation">
                <SidebarContents collapsed=collapsed on_collapse=Callback::new(move |()| toggle_collapsed()) on_navigate=Callback::new(|()| ()) allow_collapse=true />
            </aside>
            <Show when=move || !collapsed.get()><SidebarResizeHandle width=sidebar_width resize=Callback::new(resize) /></Show>
            <div class="console-workspace" aria-hidden=move || drawer_open.get().then_some("true") inert=move || drawer_open.get()>
                <Show when=move || !preference_error.with(String::is_empty)>
                    <p class="console-preference-error" role="alert">{move || preference_error.get()}" The sidebar was restored."</p>
                </Show>
                <div class="console-content"><Outlet /></div>
            </div>
            <Show when=move || drawer_open.get()>
                <button class="console-drawer-backdrop" type="button" aria-label="Close navigation" on:click=move |_| close_drawer()></button>
                <div node_ref=drawer class="console-drawer" role="dialog" aria-modal="true" aria-labelledby="console-drawer-title">
                    <div class="console-drawer-title"><h2 id="console-drawer-title">"Resource navigation"</h2>
                        <button type="button" on:click=move |_| close_drawer()>"Close"</button></div>
                    <SidebarContents collapsed=Signal::derive(|| false) on_collapse=Callback::new(|()| ()) on_navigate=Callback::new(move |()| close_drawer()) allow_collapse=false />
                </div>
            </Show>
            <span class="console-visual-state" aria-live="polite">
                {move || preferences.with(|value| format!("{} mode, {} density", value.color_scheme.to_lowercase(), value.density.to_lowercase()))}
            </span>
        </div>
    }
}

#[component]
fn SidebarResizeHandle(width: RwSignal<i32>, resize: Callback<f64>) -> impl IntoView {
    let drag = StoredValue::new_local(
        None::<(
            leptos::prelude::WindowListenerHandle,
            leptos::prelude::WindowListenerHandle,
        )>,
    );
    let finish = move || {
        if let Some((moving, ending)) = drag.try_update_value(Option::take).flatten() {
            moving.remove();
            ending.remove();
        }
    };
    on_cleanup(finish);
    view! {
        <div class="console-sidebar-resize-hitbox">
            <div class="console-sidebar-resize" role="separator" aria-label="Resize sidebar" aria-controls="console-sidebar"
                aria-orientation="vertical" aria-valuemin=MINIMUM_SIDEBAR_WIDTH aria-valuemax=MAXIMUM_SIDEBAR_WIDTH
                aria-valuenow=move || width.get() tabindex="0"
                on:keydown=move |event| {
                    let current = f64::from(width.get_untracked());
                    let next = match event.key().as_str() {
                        "ArrowLeft" => current - 16.0,
                        "ArrowRight" => current + 16.0,
                        "Home" => f64::from(MINIMUM_SIDEBAR_WIDTH),
                        "End" => f64::from(MAXIMUM_SIDEBAR_WIDTH),
                        _ => return,
                    };
                    event.prevent_default();
                    resize.run(next);
                }
                on:pointerdown=move |event| {
                    if event.button() != 0 { return; }
                    event.prevent_default();
                    finish();
                    let (origin, initial) = (f64::from(event.client_x()), f64::from(width.get_untracked()));
                    let moving = window_event_listener(ev::pointermove, move |pointer| resize.run(initial + f64::from(pointer.client_x()) - origin));
                    let ending = window_event_listener(ev::pointerup, move |_| finish());
                    drag.set_value(Some((moving, ending)));
                }></div>
        </div>
    }
}

/// The root restores an accessible local selection and never trusts a stale localStorage value.
#[component]
pub fn ConsoleHome() -> impl IntoView {
    let context = use_console().context;
    let target = context.with_untracked(|value| {
        let selection: serde_json::Value = local_storage()
            .and_then(|storage| {
                storage
                    .get_item(&selection_key(value.principal.id.inner()))
                    .ok()
                    .flatten()
            })
            .and_then(|stored| serde_json::from_str(&stored).ok())
            .unwrap_or_default();
        let chosen = |key: &str| {
            selection
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let (organization, project) = (chosen("selectedOrganization"), chosen("selectedProject"));
        let known_project = value
            .organizations
            .iter()
            .flat_map(|entry| entry.projects.iter())
            .any(|entry| {
                entry.id.inner() == project
                    && has_capability(value, "PROJECT.VIEW", "PROJECT", &project)
            });
        let known_organization = value.organizations.iter().any(|entry| {
            entry.id.inner() == organization
                && has_capability(value, "ORGANIZATION.VIEW", "ORGANIZATION", &organization)
        });
        if known_project {
            format!("/projects/{project}")
        } else if known_organization {
            format!("/organizations/{organization}/projects")
        } else {
            "/organizations".to_string()
        }
    });
    view! { <Redirect path=target /> }
}

#[component]
pub fn AccessDeniedPage() -> impl IntoView {
    view! {
        <main class="console-terminal" aria-labelledby="access-denied-title"><h1 id="access-denied-title">"Access denied"</h1>
            <p role="status">"This resource is unavailable in your current console context."</p><A href="/">"Return to your console"</A></main>
    }
}

#[component]
pub fn SessionErrorPage() -> impl IntoView {
    view! {
        <main class="console-terminal" aria-labelledby="session-error-title"><h1 id="session-error-title">"Session error"</h1>
            <p role="alert">{crate::session_expired!("Sign in again to continue.")}</p></main>
    }
}

#[component]
pub fn NotFoundPage() -> impl IntoView {
    view! {
        <main class="console-terminal" aria-labelledby="not-found-title">
            <crate::page_header::PageHeader title_id="not-found-title" title="Page not found".to_string() />
            <p>"The requested console route does not exist."</p><A href="/organizations">"Open organizations"</A></main>
    }
}
