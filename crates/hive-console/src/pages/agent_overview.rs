//! A read-only agent overview over the shared refresh state machine.

use super::refresh::{freshness_text, iso, iso_millis, unloaded_view, use_refreshing, RefreshCopy};
use crate::agent_tabs::AgentTabs;
use crate::api::console::has_capability;
use crate::api::directory::{request_agent_overview, AgentOperationalViewFields};
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;

static COPY: RefreshCopy = RefreshCopy {
    main_class: "directory agent-overview",
    css: "agent-overview",
    loading_label: "Loading agent overview",
    skeleton_label: "Loading agent operational summaries",
    offline: "You are offline. Reconnect to load this agent overview.",
    session: "Your session has expired. Sign in again to view this agent.",
    unavailable: "This agent is unavailable.",
    error_label: "Agent overview error",
    error: "We could not load this agent overview. Try again.",
};

/// `NOT_VALIDATED` becomes `Not Validated`.
fn title(value: &str) -> String {
    value
        .to_lowercase()
        .split('_')
        .map(|word| {
            let mut letters = word.chars();
            letters
                .next()
                .map(|first| first.to_uppercase().chain(letters).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn counted(count: i32, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

struct Card {
    label: &'static str,
    value: String,
    detail: String,
    to: Option<String>,
    stale: bool,
}

fn cards(overview: &AgentOperationalViewFields, base: &str, can_edit: bool) -> Vec<Card> {
    let status = |value: &Option<String>| title(value.as_deref().unwrap_or("UNKNOWN"));
    let draft_detail = match overview.draft_validation_status.as_deref() {
        None | Some("NOT_VALIDATED") => "No draft validation summary is available.".to_string(),
        Some(_) => format!(
            "{} and {}{}",
            counted(overview.draft_error_count.unwrap_or(0), "error"),
            counted(overview.draft_warning_count.unwrap_or(0), "warning"),
            overview
                .draft_validated_at
                .as_deref()
                .map_or(".".to_string(), |at| format!("; validated {}.", iso(at)))
        ),
    };
    let published_detail = if overview.has_published_version() {
        overview
            .published_at
            .as_deref()
            .map_or("Publication time is unavailable.".to_string(), |at| {
                format!("Published {}.", iso(at))
            })
    } else {
        "No published version summary is available.".to_string()
    };
    let (alias_targets, active_alias_targets) = (
        overview.alias_target_count.unwrap_or(0),
        overview.active_alias_target_count.unwrap_or(0),
    );
    let freshness = overview.runtime_freshness.as_deref().unwrap_or("UNKNOWN");
    vec![
        Card {
            label: "Draft validation",
            value: title(
                overview
                    .draft_validation_status
                    .as_deref()
                    .unwrap_or("NOT_VALIDATED"),
            ),
            detail: draft_detail,
            to: can_edit.then(|| format!("{base}/edit")),
            stale: false,
        },
        Card {
            label: "Latest published version",
            value: overview
                .published_version
                .clone()
                .unwrap_or_else(|| "No published version".to_string()),
            detail: published_detail,
            to: Some(format!("{base}/versions")),
            stale: false,
        },
        Card {
            label: "Alias targets",
            value: if alias_targets == 0 {
                "No alias targets".to_string()
            } else {
                format!("{active_alias_targets} of {alias_targets} active")
            },
            detail: "Compact target counts only.".to_string(),
            to: None,
            stale: false,
        },
        Card {
            label: "Active deployment",
            value: title(
                overview
                    .deployment_status
                    .as_deref()
                    .unwrap_or("NOT_DEPLOYED"),
            ),
            detail: overview.deployment_observed_at.as_deref().map_or(
                "No deployment observation is available.".to_string(),
                |at| format!("Observed {}.", iso(at)),
            ),
            to: Some(format!("{base}/deployments")),
            stale: false,
        },
        Card {
            label: "Recent evaluation",
            value: title(
                overview
                    .evaluation_outcome
                    .as_deref()
                    .unwrap_or("NO_EVALUATION"),
            ),
            detail: overview
                .evaluation_completed_at
                .as_deref()
                .map_or("No evaluation summary is available.".to_string(), |at| {
                    format!("Completed {}.", iso(at))
                }),
            to: Some(format!("{base}/evaluations")),
            stale: false,
        },
        // Current-period cost is omitted: the view has no cost column at agent scope.
        Card {
            label: "Runtime health",
            value: status(&overview.runtime_health),
            detail: overview
                .runtime_observed_at
                .as_deref()
                .map_or("No runtime observation is available.".to_string(), |at| {
                    format!("{}; observed {}.", title(freshness), iso(at))
                }),
            to: None,
            stale: freshness.eq_ignore_ascii_case("STALE"),
        },
    ]
}

#[component]
pub fn AgentOperationalOverview() -> impl IntoView {
    let console = use_console();
    let params = use_params_map();
    let key = Memo::new(move |_| {
        (
            params.read().get("project_id").unwrap_or_default(),
            params.read().get("agent_id").unwrap_or_default(),
        )
    });
    let live = use_refreshing(key, |(project, agent): (String, String)| {
        Box::pin(async move { request_agent_overview(&project, &agent).await })
    });
    let snapshot = live.snapshot;

    move || {
        let _ = live.shape.get();
        if let Some(view) =
            unloaded_view(&live.state.get_untracked(), &COPY, live.online, live.retry)
        {
            return view;
        }
        let (project, agent) = key.get_untracked();
        let base = format!("/projects/{project}/agents/{agent}");
        let refreshed_at =
            move || snapshot.with(|value| value.as_ref().map_or(0.0, |value| value.refreshed_at));
        let field = move |read: fn(&AgentOperationalViewFields) -> String| {
            Signal::derive(move || {
                snapshot.with(|value| {
                    value
                        .as_ref()
                        .map(|value| read(&value.value))
                        .unwrap_or_default()
                })
            })
        };
        let status = field(|overview| overview.lifecycle_status.clone());
        let body = {
            let (base, project) = (base.clone(), project.clone());
            move || {
                let Some(overview) = snapshot.get().map(|value| value.value) else {
                    return ().into_any();
                };
                let (can_edit, can_deploy, can_run) = console.context.with(|context| {
                    (
                        context.capabilities.iter().any(|capability| {
                            capability.code == "AGENT_DRAFT.UPDATE"
                                && capability.scope_id.inner() == project
                        }),
                        has_capability(context, "DEPLOYMENT.REQUEST", "PROJECT", &project),
                        has_capability(context, "EVALUATION_RUN.RUN", "PROJECT", &project),
                    )
                });
                let published = overview.has_published_version();
                let edit = format!("{base}/edit");
                view! {
                <nav class="agent-overview-actions" aria-label="Agent actions">
                    {can_edit.then(|| view! { <a href=edit.clone()>"Edit draft"</a><a href=edit.clone()>"Validate"</a><a href=edit.clone()>"Publish"</a> })}
                    {can_deploy.then(|| if published { view! { <a href=format!("{base}/versions")>"Deploy"</a> }.into_any() }
                        else { view! { <span class="agent-action-disabled" title="Publish a version before deploying it.">"Deploy"</span> }.into_any() })}
                    {can_run.then(|| view! { <a href=format!("/projects/{project}/evaluations")>"Run evaluation"</a> })}
                </nav>
                <p class="agent-overview-freshness" role="status" aria-live="polite">
                    {move || if live.flags.get().0 { "Refreshing agent overview…".to_string() } else { freshness_text("agent overview", refreshed_at(), live.clock.get()) }}</p>
                <Show when=move || !live.online.get()><p class="agent-overview-offline" role="status">"Offline. Agent data remains from "{move || iso_millis(refreshed_at())}"."</p></Show>
                <Show when=move || live.flags.get().1><p class="agent-overview-error" role="alert">"We could not refresh this agent overview. Stale data from "{move || iso_millis(refreshed_at())}" remains visible."</p></Show>
                <section class="agent-overview-cards" aria-label="Agent operational summaries">
                    {cards(&overview, &base, can_edit).into_iter().map(|card| {
                        let class = if card.stale { "agent-overview-card agent-overview-card-stale" } else { "agent-overview-card" };
                        let article = view! {
                            <article class=class><h2>{card.label}</h2>
                                <p>{card.stale.then(|| view! { <span class="stale-marker" aria-hidden="true">"⚠ "</span> })}{card.value}</p>
                                <small>{card.detail}</small></article>
                        };
                        match card.to {
                            Some(target) => view! { <a class="dashboard-card-link" href=target>{article}</a> }.into_any(),
                            None => article.into_any(),
                        }
                    }).collect_view()}
                </section>
            }.into_any()
            }
        };
        view! {
            <main class="directory agent-overview" aria-labelledby="agent-overview-title">
                <AgentTabs project_id=project.clone() agent_id=agent.clone() active="overview" />
                <PageHeader title_id="agent-overview-title" title=field(|overview| overview.display_name.clone()) meta=field(|overview| format!("ID: {}", overview.slug))>
                    <span class="lifecycle-badge" data-status=move || status.get()>{move || status.get()}</span>
                </PageHeader>
                {body}
            </main>
        }.into_any()
    }
}
