//! A read-only dashboard over the shared refresh state machine.

use super::request::{freshness_text, unloaded_view, use_request, Policy, UnloadedCopy, SLOW_POLL};
use crate::api::directory::{request_dashboard, ProjectDashboardFields};
use crate::format::{iso, iso_millis};
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;

static COPY: UnloadedCopy = UnloadedCopy {
    main_class: "directory project-dashboard",
    css: "project-dashboard",
    loading_label: "Loading project dashboard",
    skeleton_label: "Loading dashboard summary",
    offline: "You are offline. Reconnect to load this project dashboard.",
    session: crate::session_expired!("Sign in again to view this project."),
    unavailable: "This project is unavailable.",
    error_label: "Project dashboard error",
    error: "We could not load this project dashboard. Try again.",
};

/// The cost card's value and detail line.
fn cost_card(dashboard: &ProjectDashboardFields) -> (String, String) {
    if let ("AVAILABLE", Some(start), Some(end), Some(currency), Some(cents), Some(as_of)) = (
        dashboard.cost_availability.as_str(),
        &dashboard.cost_period_start,
        &dashboard.cost_period_end,
        &dashboard.cost_currency,
        dashboard.current_period_cost_cents,
        &dashboard.cost_data_as_of,
    ) {
        return (
            format!("{currency} {:.2}", f64::from(cents) / 100.0),
            format!(
                "Period {} to {}; reported {}.",
                iso(start),
                iso(end),
                iso(as_of)
            ),
        );
    }
    if dashboard.cost_availability == "UNAVAILABLE" {
        return (
            "Unavailable".to_string(),
            "The governed current-period cost source reported unavailable.".to_string(),
        );
    }
    (
        "Unknown".to_string(),
        "No governed current-period cost source is available.".to_string(),
    )
}

#[component]
pub fn ProjectDashboardPage() -> impl IntoView {
    let console = use_console();
    let params = use_params_map();
    let project_id = Memo::new(move |_| params.read().get("project_id").unwrap_or_default());
    let live = use_request(
        project_id,
        |id: String| Box::pin(async move { request_dashboard(&id).await }),
        Policy::polling(SLOW_POLL),
    );
    let snapshot = live.snapshot;

    move || {
        // `get`, not `track`: a lazy memo subscribes to its sources only once it has been read.
        let _ = live.shape.get();
        if let Some(view) =
            unloaded_view(&live.state.get_untracked(), &COPY, live.online, live.retry)
        {
            return view;
        }
        let refreshed_at =
            move || snapshot.with(|value| value.as_ref().map_or(0.0, |value| value.refreshed_at));
        let field = move |read: fn(&ProjectDashboardFields) -> String| {
            Signal::derive(move || {
                snapshot.with(|value| {
                    value
                        .as_ref()
                        .map(|value| read(&value.value))
                        .unwrap_or_default()
                })
            })
        };
        let status = field(|dashboard| dashboard.lifecycle_status.clone());
        let cards = move || {
            let Some(dashboard) = snapshot.get().map(|value| value.value) else {
                return ().into_any();
            };
            let project = dashboard.project_id.clone();
            let can_view_agents = console.context.with(|context| {
                context.capabilities.iter().any(|capability| {
                    capability.code == "AGENT.VIEW" && capability.scope_id.inner() == project
                })
            });
            // These three counts come from the project summary table, a separate source from the
            // list pages, so a reviewer may see 0 rows there for a nonzero count here.
            let summary = Some(
                "From the project summary source, independent of the corresponding list page."
                    .to_string(),
            );
            let (cost_value, cost_detail) = cost_card(&dashboard);
            let all_zero = [
                dashboard.active_agents,
                dashboard.active_deployments,
                dashboard.failed_deployments,
                dashboard.pending_approvals,
                dashboard.unhealthy_resources,
            ]
            .iter()
            .all(|count| *count == 0);
            let entries = [
                (
                    "Active agents",
                    dashboard.active_agents.to_string(),
                    None,
                    can_view_agents.then(|| format!("/projects/{project}/agents")),
                ),
                (
                    "Active deployments",
                    dashboard.active_deployments.to_string(),
                    summary.clone(),
                    None,
                ),
                (
                    "Failed deployments",
                    dashboard.failed_deployments.to_string(),
                    summary.clone(),
                    None,
                ),
                (
                    "Pending approvals",
                    dashboard.pending_approvals.to_string(),
                    summary,
                    None,
                ),
                (
                    "Unhealthy resources",
                    dashboard.unhealthy_resources.to_string(),
                    None,
                    None,
                ),
                ("Current-period cost", cost_value, Some(cost_detail), None),
            ];
            view! {
                <section class="project-dashboard-cards" aria-label="Project dashboard summary cards">
                    {entries.into_iter().map(|(label, value, detail, to)| {
                        let card = view! {
                            <article class="project-dashboard-card"><h2>{label}</h2><p>{value}</p>
                                {detail.map(|text| view! { <small class="project-dashboard-card-detail">{text}</small> })}</article>
                        };
                        match to {
                            Some(target) => view! { <a class="dashboard-card-link" href=target>{card}</a> }.into_any(),
                            None => card.into_any(),
                        }
                    }).collect_view()}
                </section>
                {all_zero.then(|| view! {
                    <section class="project-dashboard-setup" aria-labelledby="project-setup-title"><h2 id="project-setup-title">"Project setup guidance"</h2>
                        <ul><li>"Create an agent"</li><li>"Connect a tool or integration"</li><li>"Configure evaluation"</li><li>"Invite project members"</li></ul></section>
                })}
            }.into_any()
        };
        view! {
            <main class="directory project-dashboard" aria-labelledby="project-dashboard-title">
                <PageHeader title_id="project-dashboard-title" title=field(|dashboard| dashboard.display_name.clone()) meta=field(|dashboard| format!("ID: {}", dashboard.slug))>
                    <span class="lifecycle-badge" data-status=move || status.get()>{move || status.get()}</span>
                </PageHeader>
                <p class="project-dashboard-freshness" role="status" aria-live="polite">
                    {move || if live.flags.get().0 { "Refreshing dashboard…".to_string() } else { freshness_text("dashboard", refreshed_at(), live.clock.get()) }}</p>
                <Show when=move || !live.online.get()><p class="project-dashboard-offline" role="status">"Offline. Dashboard data remains from "{move || iso_millis(refreshed_at())}"."</p></Show>
                <Show when=move || live.flags.get().1><p class="project-dashboard-error" role="alert">"We could not refresh this dashboard. Stale data from "{move || iso_millis(refreshed_at())}" remains visible."</p></Show>
                {cards}
            </main>
        }.into_any()
    }
}
