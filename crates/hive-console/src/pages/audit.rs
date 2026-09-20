//! Ports `AuditPages.tsx`: immutable audit history for one organization or one project. Every
//! filter lives in the URL, so a filtered view and an open event can be linked and restored.

use crate::api::audit::{
    request_audit_event, request_audit_events, AuditEventFields, AuditEventFilter,
};
use crate::dom;
use crate::graphql::TransportFailure;
use crate::page_header::PageHeader;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_location, use_navigate, use_params_map, use_query_map};
use std::time::Duration;
use wasm_bindgen::JsCast;

const UNAVAILABLE: &str = "Audit history is unavailable.";
const FOCUSABLE: &str = "button, [href], input, select, textarea, [tabindex]:not([tabindex='-1'])";
const ERROR_ID: &str = "audit-filters-error";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Organization,
    Project,
}

#[derive(Clone, PartialEq)]
struct PageState {
    events: Vec<AuditEventFields>,
    end_cursor: Option<String>,
    has_next_page: bool,
    last_retrieved: String,
    stale: bool,
}

fn locale(value: &str) -> String {
    String::from(
        js_sys::Date::new(&value.into())
            .to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED),
    )
}

/// The `YYYY-MM-DDTHH:MM` prefix a `datetime-local` control accepts.
fn minutes(value: &str) -> String {
    value.chars().take(16).collect()
}

fn iso(date: &js_sys::Date) -> String {
    String::from(date.to_iso_string())
}

fn denied(failure: &TransportFailure) -> bool {
    matches!(failure.status, 401 | 403) || failure.message == UNAVAILABLE
}

fn not_recorded(text: &'static str) -> AnyView {
    view! { <span class="audit-not-recorded">{text}</span> }.into_any()
}

fn audit_page(scope: Scope) -> impl IntoView {
    let params = use_params_map();
    let query = use_query_map();
    let location = use_location();
    let navigate = use_navigate();
    let scope_id = Memo::new(move |_| {
        params
            .read()
            .get(if scope == Scope::Organization {
                "organization_id"
            } else {
                "project_id"
            })
            .unwrap_or_default()
    });
    let default_after = iso(&js_sys::Date::new(
        &(js_sys::Date::now() - 30.0 * 24.0 * 60.0 * 60.0 * 1000.0).into(),
    ));
    let param = move |name: &str| {
        query
            .read()
            .get(name)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let raw = move |name: &'static str| query.read().get(name).unwrap_or_default();
    let filter = Memo::new(move |_| {
        let id = Some(cynic::Id::new(scope_id.get()));
        AuditEventFilter {
            organization_id: id.clone().filter(|_| scope == Scope::Organization),
            project_id: id.filter(|_| scope == Scope::Project),
            event_id: param("eventFilter").map(cynic::Id::new),
            correlation_id: param("correlation").map(cynic::Id::new),
            actor_id: param("actor").map(cynic::Id::new),
            action: param("action"),
            outcome: param("outcome"),
            resource_type: param("resourceType"),
            resource_id: param("resourceId").map(cynic::Id::new),
            occurred_after: Some(param("afterTime").unwrap_or_else(|| default_after.clone())),
            occurred_before: param("beforeTime"),
        }
    });
    let selected = Memo::new(move |_| query.read().get("event").filter(|value| !value.is_empty()));

    let state = RwSignal::new(None::<PageState>);
    let (loading, loading_more, unavailable) = (
        RwSignal::new(true),
        RwSignal::new(false),
        RwSignal::new(false),
    );
    let (error, detail_error) = (
        RwSignal::new(None::<&'static str>),
        RwSignal::new(None::<&'static str>),
    );
    let detail = RwSignal::new(None::<AuditEventFields>);
    let refresh_epoch = RwSignal::new(0_u32);
    // Narrows rows already loaded: the server scopes a query by an organization or a project, never both.
    let project_filter = RwSignal::new(String::new());
    let copied = RwSignal::new(None::<&'static str>);
    let (generation, detail_generation) = (StoredValue::new(0_u32), StoredValue::new(0_u32));
    let opener = StoredValue::new_local(None::<web_sys::HtmlElement>);
    let drawer = NodeRef::<leptos::html::Aside>::new();
    let heading = NodeRef::<leptos::html::H2>::new();

    let request = move |after: Option<String>, preserve: bool| {
        let current = generation.get_value() + 1;
        generation.set_value(current);
        if !preserve {
            state.set(None);
            loading_more.set(false);
            unavailable.set(false);
            loading.set(true);
        }
        error.set(None);
        let appending = after.is_some();
        let sent = filter.get_untracked();
        spawn_local(async move {
            let result = request_audit_events(sent, after).await;
            if generation.try_get_value() != Some(current) {
                return;
            }
            match result {
                Ok(connection) => state.update(|previous| {
                    let mut events = previous
                        .take()
                        .filter(|_| appending)
                        .map(|previous| previous.events)
                        .unwrap_or_default();
                    for edge in connection.edges {
                        if !events.iter().any(|event| event.id == edge.node.id) {
                            events.push(edge.node);
                        }
                    }
                    *previous = Some(PageState {
                        events,
                        end_cursor: connection.page_info.end_cursor,
                        has_next_page: connection.page_info.has_next_page,
                        last_retrieved: String::from(
                            js_sys::Date::new_0()
                                .to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED),
                        ),
                        stale: false,
                    });
                }),
                Err(failure) if denied(&failure) => {
                    state.set(None);
                    detail.set(None);
                    detail_error.set(None);
                    unavailable.set(true);
                    error.set(Some(UNAVAILABLE));
                }
                Err(failure) => {
                    state.update(|previous| {
                        if let Some(previous) = previous {
                            previous.stale = true;
                        }
                    });
                    error.set(Some(if failure.status == 503 {
                        "Audit history is temporarily unavailable. Retry the request."
                    } else {
                        "Audit history could not be refreshed."
                    }));
                }
            }
            loading.set(false);
            loading_more.set(false);
        });
    };
    Effect::new(move |_| {
        let _ = filter.get();
        request(None, false);
    });
    let online = window_event_listener(ev::online, move |_| request(None, true));

    Effect::new(move |_| {
        let _ = refresh_epoch.get();
        let sent = filter.get();
        let current = detail_generation.get_value() + 1;
        detail_generation.set_value(current);
        let Some(id) = selected.get() else {
            detail.set(None);
            return;
        };
        if detail.with_untracked(|shown| shown.as_ref().is_none_or(|shown| shown.id.inner() != id))
        {
            detail.set(None);
        }
        detail_error.set(None);
        spawn_local(async move {
            let result = request_audit_event(sent, &id).await;
            if detail_generation.try_get_value() != Some(current) {
                return;
            }
            match result {
                Ok(Some(event)) => detail.set(Some(event)),
                Ok(None) => {
                    state.set(None);
                    detail.set(None);
                    unavailable.set(true);
                }
                Err(failure) if denied(&failure) => {
                    state.set(None);
                    detail.set(None);
                    unavailable.set(true);
                }
                Err(failure) => detail_error.set(Some(if failure.status == 503 {
                    "Audit event detail is temporarily unavailable. Retry the request."
                } else {
                    "Audit event detail is unavailable."
                })),
            }
        });
    });
    Effect::new(move |_| {
        if detail.with(Option::is_some) {
            if let Some(heading) = heading.get() {
                let _ = heading.focus();
            }
        }
    });

    // Rewrites the query string, keeping the path; each change is a history entry, as in the React console.
    let set_search = {
        let navigate = navigate.clone();
        move |edit: &dyn Fn(&web_sys::UrlSearchParams)| {
            let Ok(search) =
                web_sys::UrlSearchParams::new_with_str(&location.search.get_untracked())
            else {
                return;
            };
            edit(&search);
            let text = String::from(search.to_string());
            navigate(
                &format!(
                    "{}{}{}",
                    location.pathname.get_untracked(),
                    if text.is_empty() { "" } else { "?" },
                    text
                ),
                Default::default(),
            );
        }
    };
    let set_value = {
        let set_search = set_search.clone();
        move |name: &'static str, value: Option<String>| {
            set_search(
                &|search| match value.as_deref().filter(|value| !value.is_empty()) {
                    Some(value) => search.set(name, value),
                    None => search.delete(name),
                },
            )
        }
    };
    let reset = {
        let set_search = set_search.clone();
        move |_| {
            set_search(&|search| {
                let event = search.get("event");
                for key in js_sys::Array::from(&search.keys()).iter() {
                    if let Some(key) = key.as_string() {
                        search.delete(&key);
                    }
                }
                if let Some(event) = event.filter(|event| !event.is_empty()) {
                    search.set("event", &event);
                }
            })
        }
    };
    let close = {
        let set_value = set_value.clone();
        move || {
            set_value("event", None);
            if let Some(opener) = opener.get_value() {
                let _ = opener.focus();
            }
        }
    };
    let show_correlation = {
        let set_search = set_search.clone();
        move |correlation: String| {
            set_search(&|search| {
                search.set("correlation", &correlation);
                search.delete("event");
            })
        }
    };
    let refresh = move || {
        refresh_epoch.update(|epoch| *epoch += 1);
        request(None, true);
    };
    let copy = Callback::new(move |(name, value): (&'static str, String)| {
        spawn_local(async move {
            if wasm_bindgen_futures::JsFuture::from(
                window().navigator().clipboard().write_text(&value),
            )
            .await
            .is_ok()
            {
                copied.set(Some(name));
                set_timeout(
                    move || {
                        let _ = copied.try_update(|current| {
                            if *current == Some(name) {
                                *current = None;
                            }
                        });
                    },
                    Duration::from_secs(2),
                );
            }
        })
    });
    let escape = {
        let close = close.clone();
        window_event_listener(ev::keydown, move |event| {
            if event.key() == "Escape" && selected.get_untracked().is_some() {
                close();
            }
        })
    };
    on_cleanup(move || {
        online.remove();
        escape.remove();
    });
    let trap_focus = move |event: ev::KeyboardEvent| {
        if event.key() != "Tab" {
            return;
        }
        let Some(panel) = drawer.get_untracked() else {
            return;
        };
        let targets: Vec<_> = dom::elements(&panel, FOCUSABLE)
            .into_iter()
            .filter(|item| !item.has_attribute("disabled"))
            .collect();
        let (Some(first), Some(last)) = (targets.first(), targets.last()) else {
            event.prevent_default();
            return;
        };
        let active = dom::active_element();
        if event.shift_key() && active.as_ref() == Some(first.as_ref()) {
            event.prevent_default();
            let _ = last.focus();
        }
        if !event.shift_key() && active.as_ref() == Some(last.as_ref()) {
            event.prevent_default();
            let _ = first.focus();
        }
    };

    let described = move || error.get().map(|_| ERROR_ID);
    let text_filter = {
        let set_value = set_value.clone();
        move |id: &'static str, label: &'static str, name: &'static str| {
            let set_value = set_value.clone();
            view! {
            <label for=id>{label}<input id=id aria-describedby=described prop:value=move || raw(name) on:input=move |event| set_value(name, Some(event_target_value(&event))) /></label> }
        }
    };
    let time_filter = {
        let set_value = set_value.clone();
        move |id: &'static str, label: &'static str, name: &'static str| {
            let set_value = set_value.clone();
            view! {
            <label for=id>{label}<input id=id aria-describedby=described type="datetime-local" prop:value=move || minutes(&raw(name))
                on:change=move |event| { let value = event_target_value(&event); set_value(name, (!value.is_empty()).then(|| iso(&js_sys::Date::new(&value.into())))); } /></label> }
        }
    };
    let select_filter = {
        let set_value = set_value.clone();
        move |id: &'static str,
              label: &'static str,
              name: &'static str,
              options: &'static [(&'static str, &'static str)]| {
            let set_value = set_value.clone();
            view! {
            <label for=id>{label}<select id=id aria-describedby=described prop:value=move || raw(name) on:change=move |event| set_value(name, Some(event_target_value(&event)))>
                {options.iter().map(|(value, text)| view! { <option value=*value selected=move || raw(name) == *value>{*text}</option> }).collect_view()}</select></label> }
        }
    };
    let visible = Memo::new(move |_| {
        state.with(|state| {
            state.as_ref().map_or_else(Vec::new, |state| {
                let wanted = project_filter.get();
                let wanted = wanted.trim();
                if scope == Scope::Organization && !wanted.is_empty() {
                    state
                        .events
                        .iter()
                        .filter(|event| {
                            event
                                .project_id
                                .as_ref()
                                .is_some_and(|id| id.inner() == wanted)
                        })
                        .cloned()
                        .collect()
                } else {
                    state.events.clone()
                }
            })
        })
    });
    let open_detail = {
        let set_value = set_value.clone();
        move |clicked: ev::MouseEvent, id: String| {
            opener.set_value(
                clicked
                    .current_target()
                    .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok()),
            );
            set_value("event", Some(id));
        }
    };

    view! {
        <main class="audit-page" aria-labelledby="audit-title">
            <PageHeader title_id="audit-title" title="Audit history".to_string() description="Review immutable, scope-authorized activity. Sensitive request metadata is redacted unless the server grants access." />
            {move || if unavailable.get() { view! { <p role="status">{UNAVAILABLE}</p> }.into_any() } else {
                let (close_backdrop, close_button, show_correlation, open_detail, reset) = (close.clone(), close.clone(), show_correlation.clone(), open_detail.clone(), reset.clone());
                view! {
                <form class="audit-filters" aria-label="Audit filters" on:submit=move |event| { event.prevent_default(); request(None, false); }>
                    {select_filter("audit-filters-action", "Action", "action", &[("", "All actions"), ("AGENT_VERSION_PUBLISHED", "Agent version published"), ("DEPLOYMENT_REQUESTED", "Deployment requested"),
                        ("DEPLOYMENT_APPROVED", "Deployment approved"), ("EVALUATION_RUN_FAILED", "Evaluation failed"), ("ADMINISTRATION_CHANGED", "Settings changed")])}
                    {select_filter("audit-filters-outcome", "Outcome", "outcome", &[("", "All outcomes"), ("SUCCEEDED", "Succeeded"), ("FAILED", "Failed"), ("CANCELED", "Canceled")])}
                    {text_filter("audit-filters-actor", "Actor ID", "actor")}
                    {text_filter("audit-filters-resource-type", "Resource type", "resourceType")}
                    {text_filter("audit-filters-resource-id", "Resource ID", "resourceId")}
                    {text_filter("audit-filters-correlation", "Correlation ID", "correlation")}
                    {time_filter("audit-filters-after", "Occurred after", "afterTime")}
                    {time_filter("audit-filters-before", "Occurred before", "beforeTime")}
                    {(scope == Scope::Organization).then(|| view! { <label for="audit-filters-project">"Filter loaded rows by project ID"
                        <input id="audit-filters-project" prop:value=move || project_filter.get() on:input=move |event| project_filter.set(event_target_value(&event)) /></label> })}
                    <div class="audit-filter-actions"><button type="submit" class="primary-action">"Apply filters"</button><button type="button" on:click=reset>"Reset filters"</button><button type="button" on:click=move |_| refresh()>"Refresh"</button></div>
                </form>
                <p class="audit-retrieval" role="status">{move || state.with(|state| state.as_ref().map_or("No audit results loaded.".to_string(), |state|
                    format!("Last retrieved {}{}", state.last_retrieved, if state.stale { "; refresh failed; displayed results may be stale." } else { "." })))}</p>
                {move || (loading.get() && state.with(Option::is_none)).then(|| view! { <p role="status">"Loading audit history…"</p> })}
                {move || error.get().map(|text| view! { <p id=ERROR_ID role="alert">{text}" "<button type="button" on:click=move |_| refresh()>"Retry"</button></p> })}
                {move || (state.with(|state| state.as_ref().is_some_and(|state| state.events.is_empty())) && error.get().is_none()).then(|| view! { <p role="status">"No audit events match these filters."</p> })}
                {move || (state.with(|state| state.as_ref().is_some_and(|state| !state.events.is_empty())) && visible.with(Vec::is_empty)).then(|| view! {
                    <p role="status">"No loaded event belongs to project "{project_filter.get().trim().to_string()}". This filters rows already loaded, not the server query — load more events or clear the project filter to widen the search."</p> })}
                {move || { let events = visible.get(); let open_detail = open_detail.clone(); (!events.is_empty()).then(|| view! {
                    <div class="audit-table-scroll" role="region" aria-label="Audit history table" tabindex="0">
                        <table><caption>"Immutable audit events in the selected scope"</caption>
                            <thead><tr><th scope="col">"Occurred"</th><th scope="col">"Actor"</th><th scope="col">"Project"</th><th scope="col">"Action"</th><th scope="col">"Resource"</th><th scope="col">"Outcome"</th><th scope="col">"Correlation"</th><th scope="col">"Detail"</th></tr></thead>
                            <tbody>{events.into_iter().map(|event| { let (open_detail, id) = (open_detail.clone(), event.id.inner().to_string()); view! {
                                <tr><th scope="row">{locale(&event.occurred_at)}</th>
                                    <td>{event.actor_id.map_or("System".to_string(), |id| id.into_inner())}</td><td>{event.project_id.map_or("Organization".to_string(), |id| id.into_inner())}</td>
                                    <td>{event.action.replace('_', " ")}</td><td>{event.resource.map_or("Not recorded".to_string(), |resource| resource.label())}</td><td>{event.outcome}</td>
                                    <td>{event.correlation_id.map_or("Not recorded".to_string(), |id| id.into_inner())}</td>
                                    <td><button type="button" aria-label=format!("View audit event {}", event.action) on:click=move |clicked| open_detail(clicked, id.clone())>"View detail"</button></td></tr> } }).collect_view()}</tbody>
                        </table></div> }) }}
                {move || state.with(|state| state.as_ref().filter(|state| state.has_next_page).map(|state| state.end_cursor.clone())).map(|cursor| view! {
                    <button type="button" disabled=move || loading_more.get() on:click=move |_| { if let Some(cursor) = cursor.clone() { loading_more.set(true); request(Some(cursor), true); } }>
                        {move || if loading_more.get() { "Loading more events…" } else { "Load more events" }}</button> })}
                {move || selected.get().map(|_| { let (close_backdrop, close_button, show_correlation) = (close_backdrop.clone(), close_button.clone(), show_correlation.clone()); view! {
                    <div class="audit-drawer-backdrop" role="presentation" on:mousedown=move |_| close_backdrop()>
                        <aside node_ref=drawer class="audit-drawer" role="dialog" aria-modal="true" aria-labelledby="audit-detail-title" on:keydown=trap_focus on:mousedown=|event| event.stop_propagation()>
                            <button type="button" aria-label="Close audit detail" on:click=move |_| close_button()>"Close"</button><h2 id="audit-detail-title" tabindex="-1" node_ref=heading>"Audit event detail"</h2>
                            {move || detail_error.get().map(|text| view! { <p role="alert">{text}" "<button type="button" on:click=move |_| refresh()>"Retry"</button></p> })}
                            {move || (detail.with(Option::is_none) && detail_error.get().is_none()).then(|| view! { <p role="status">"Loading audit event detail…"</p> })}
                            {move || detail.get().map(|event| { let show_correlation = show_correlation.clone(); let redacted = if event.sensitive_fields_redacted { "Redacted" } else { "Not recorded" };
                                let copyable = move |name: &'static str, value: Option<String>| value.map_or_else(|| not_recorded("Not recorded"), |value| { let text = value.clone(); view! {
                                    <span class="audit-copyable"><code>{value}</code><button type="button" on:click=move |_| copy.run((name, text.clone()))>{move || if copied.get() == Some(name) { "Copied" } else { "Copy" }}</button></span> }.into_any() });
                                view! {
                                <section class="audit-detail-group" aria-label="What happened"><dl>
                                    <dt>"Action"</dt><dd>{event.action.replace('_', " ")}</dd>
                                    <dt>"Outcome"</dt><dd><span class=format!("audit-outcome audit-outcome-{}", event.outcome.to_lowercase())>{event.outcome.clone()}</span></dd>
                                    <dt>"Occurred"</dt><dd>{locale(&event.occurred_at)}</dd>
                                    <dt>"Actor"</dt><dd>{event.actor_id.clone().map_or("System".to_string(), |id| id.into_inner())}</dd>
                                    <dt>"Project"</dt><dd>{event.project_id.clone().map_or("Organization".to_string(), |id| id.into_inner())}</dd>
                                    <dt>"Resource"</dt><dd>{event.resource.as_ref().map_or_else(|| not_recorded("Not recorded"), |resource| resource.label().into_any())}</dd>
                                    <dt>"Correlation ID"</dt><dd>{event.correlation_id.clone().map_or_else(|| not_recorded("Not recorded"), |id| view! {
                                        <button type="button" on:click=move |_| show_correlation(id.inner().to_string())>"Show this correlation"</button> }.into_any())}</dd>
                                    {event.references.iter().map(|reference| view! { <div><dt>"Related resource or evidence"</dt><dd>{reference.label()}</dd></div> }).collect_view()}
                                </dl></section>
                                <details class="audit-detail-group audit-detail-technical"><summary>"Technical detail"</summary><dl>
                                    <dt>"Event ID"</dt><dd>{copyable("Event ID", Some(event.id.inner().to_string()))}</dd>
                                    <dt>"Request ID"</dt><dd>{copyable("Request ID", event.request_id.clone().map(cynic::Id::into_inner))}</dd>
                                    <dt>"Operation"</dt><dd>{event.graphql_operation.clone().map_or_else(|| not_recorded("Not recorded"), IntoAny::into_any)}</dd>
                                    <dt>"Before digest"</dt><dd>{copyable("Before digest", event.before_digest.clone())}</dd>
                                    <dt>"After digest"</dt><dd>{copyable("After digest", event.after_digest.clone())}</dd>
                                    <dt>"Changed fields"</dt><dd>{if event.changed_fields.is_empty() { not_recorded("Not recorded") } else { event.changed_fields.join(", ").into_any() }}</dd>
                                    <dt>"Source IP"</dt><dd>{event.source_ip.clone().map_or_else(|| not_recorded(redacted), IntoAny::into_any)}</dd>
                                    <dt>"User agent"</dt><dd>{event.user_agent.clone().map_or_else(|| not_recorded(redacted), IntoAny::into_any)}</dd>
                                </dl></details> } })}
                        </aside></div> } })}
                }.into_any() }}
        </main>
    }
}

#[component]
pub fn OrganizationAuditPage() -> impl IntoView {
    audit_page(Scope::Organization)
}

#[component]
pub fn ProjectAuditPage() -> impl IntoView {
    audit_page(Scope::Project)
}
