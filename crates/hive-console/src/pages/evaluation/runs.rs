//! One evaluation run: its target, its case, metric, artifact and audit fact lists, and the
//! cancel and rerun commands.

use super::{alert, route_param, state_lines, Kind};
use crate::api::evaluation::*;
use crate::confirmation_dialog::ConfirmationDialog;
use crate::format::{display_time, encode, random_uuid};
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use std::time::Duration;

#[component]
pub fn EvaluationRunPage() -> impl IntoView {
    let (project, run_id) = (route_param("project_id"), route_param("run_id"));
    let navigate = use_navigate();
    let run = RwSignal::new(None::<EvaluationRunDetail>);
    let (state, message, cancel_dialog) = (
        RwSignal::new(Kind::Loading),
        RwSignal::new(String::new()),
        RwSignal::new(false),
    );
    // Which run actions are offered is the server's answer, taken against the run's lifecycle and
    // this principal's capabilities; the page only renders it.
    let offered = move |available: fn(&EvaluationRunSummary) -> bool| {
        move || run.with(|run| run.as_ref().is_some_and(|run| available(&run.summary)))
    };
    let can_cancel = offered(|run| run.can_cancel);
    let can_rerun = offered(|run| run.can_rerun);
    let generation = StoredValue::new(0_u32);
    let load = Callback::new(move |done: Callback<()>| {
        let (project_id, id) = (project.get_untracked(), run_id.get_untracked());
        let current = generation.get_value() + 1;
        generation.set_value(current);
        spawn_local(async move {
            let result = request_evaluation_run(&id).await;
            if generation.try_get_value() == Some(current) {
                match result {
                    Err(GraphqlError::Transport(_)) => state.set(Kind::Error),
                    Ok(Some(value)) if value.summary.project_id == project_id => {
                        run.set(Some(value));
                        state.set(Kind::Ready);
                    }
                    _ => state.set(Kind::Unavailable),
                }
            }
            done.run(());
        });
    });
    let nothing = Callback::new(|()| ());
    Effect::new(move |_| {
        let _ = (project.get(), run_id.get());
        load.run(nothing);
    });

    // While the run is active the visible, online page polls every three seconds.
    let timer = StoredValue::new(None::<TimeoutHandle>);
    let clear_timer = move || {
        if let Some(handle) = timer.try_update_value(Option::take).flatten() {
            handle.clear();
        }
    };
    let active =
        Memo::new(move |_| run.with(|run| run.as_ref().is_some_and(|run| !run.summary.terminal)));
    let visible_online = || {
        document().visibility_state() == web_sys::VisibilityState::Visible
            && window().navigator().on_line()
    };
    let schedule = StoredValue::new(None::<Callback<()>>);
    schedule.set_value(Some(Callback::new(move |()| {
        clear_timer();
        if !active.try_get_untracked().unwrap_or(false) || !visible_online() {
            return;
        }
        let again = schedule.get_value().unwrap_or(nothing);
        timer.set_value(
            set_timeout_with_handle(move || load.run(again), Duration::from_secs(3)).ok(),
        );
    })));
    let again = move || schedule.try_get_value().flatten().unwrap_or(nothing);
    Effect::new(move |_| {
        let _ = active.get();
        again().run(());
    });
    let refresh = move || {
        if active.get_untracked() && visible_online() {
            load.run(again());
        }
    };
    let on_focus = window_event_listener(ev::focus, move |_| refresh());
    let on_online = window_event_listener(ev::online, move |_| refresh());
    crate::dom::on_document_event("visibilitychange", refresh);
    on_cleanup(move || {
        clear_timer();
        on_focus.remove();
        on_online.remove();
    });

    let cancel = move |_| {
        let Some(current) = run.get_untracked().filter(|_| can_cancel()) else {
            return;
        };
        message.set(String::new());
        spawn_local(async move {
            match cancel_evaluation(CancelEvaluationInput {
                run_id: current.summary.id.as_str().into(),
                expected_generation: current.summary.generation.into(),
                idempotency_key: random_uuid(),
                reason: Some("Canceled from the local evaluation detail.".to_string()),
            })
            .await
            {
                Err(_) => message.set("Evaluation runs are unavailable.".to_string()),
                Ok(value) => match value.problem() {
                    Some(problem) => message.set(problem),
                    None => {
                        if value.run.is_some() {
                            cancel_dialog.set(false);
                            load.run(nothing);
                        }
                    }
                },
            }
        });
    };
    let rerun = move |_| {
        let Some(current) = run.get_untracked().filter(|_| can_rerun()) else {
            return;
        };
        message.set(String::new());
        let (navigate, project_id) = (navigate.clone(), project.get_untracked());
        spawn_local(async move {
            match rerun_evaluation(RerunEvaluationInput {
                run_id: current.summary.id.as_str().into(),
                idempotency_key: random_uuid(),
            })
            .await
            {
                Err(_) => message.set("Evaluation runs are unavailable.".to_string()),
                Ok(value) => match (value.problem(), value.run) {
                    (Some(problem), _) => message.set(problem),
                    (None, Some(next)) if next.project_id == project_id => navigate(
                        &format!("/projects/{project_id}/evaluations/runs/{}", next.id),
                        Default::default(),
                    ),
                    _ => {}
                },
            }
        });
    };
    /// Loads the next page of one fact list and appends it to the displayed run.
    macro_rules! more {
        ($field:ident, $request:ident, $unavailable:literal) => {
            move |_| {
                let Some((id, number)) = run.with_untracked(|run| {
                    run.as_ref().and_then(|run| {
                        run.$field
                            .next_page()
                            .map(|number| (run.summary.id.clone(), number))
                    })
                }) else {
                    return;
                };
                spawn_local(async move {
                    match $request(&id, number).await {
                        Ok(Some(next)) => run.update(|run| {
                            if let Some(run) = run {
                                run.$field.extend(next);
                            }
                        }),
                        _ => message.set($unavailable.to_string()),
                    }
                });
            }
        };
    }
    let more_cases = more!(
        cases,
        request_evaluation_run_cases,
        "Case facts are unavailable."
    );
    let more_metrics = more!(
        metrics,
        request_evaluation_run_metrics,
        "Metric facts are unavailable."
    );
    let more_artifacts = more!(
        artifacts,
        request_evaluation_run_artifacts,
        "Artifact facts are unavailable."
    );
    let more_audit = more!(
        audit,
        request_evaluation_run_audit,
        "Audit facts are unavailable."
    );

    // A Memo, because setting `state` to the value it already holds still notifies, and the page must not remount on every reload.
    let unavailable = Memo::new(move |_| state.get() == Kind::Unavailable);
    move || {
        if unavailable.get() {
            return view! { <main class="evaluation-page"><p role="status">"This evaluation run is unavailable."</p></main> }.into_any();
        }
        let rerun = rerun.clone();
        view! {
            <main class="evaluation-page" aria-labelledby="evaluation-run-detail-title">
                {state_lines(state, "Loading evaluation run…", "", "We could not refresh this evaluation run.")}
                {alert(message)}
                {move || run.with(Option::is_some).then(|| { let rerun = rerun.clone(); let project_id = project.get_untracked(); view! {
                    <PageHeader title_id="evaluation-run-detail-title" title="Evaluation run".to_string()
                        meta=Signal::derive(move || run.with(|run| run.as_ref().map_or(String::new(), |run| format!("{}{}", run.summary.lifecycle_status, run.summary.outcome_category.clone().map_or(String::new(), |outcome| format!(" · {outcome}"))))))>
                        <button type="button" on:click=move |_| load.run(nothing)>"Refresh run"</button>
                        {move || can_cancel().then(|| view! { <button type="button" on:click=move |_| cancel_dialog.set(true)>"Cancel evaluation"</button> })}
                        {move || { let rerun = rerun.clone(); can_rerun().then(|| view! { <button type="button" on:click=rerun>"Rerun immutable target"</button> }) }}
                    </PageHeader>
                    <p role="status">{move || if active.get() { "This visible page polls the bounded run projection while local execution remains active." } else { "This evaluation run reached a terminal state." }}</p>
                    {move || run.get().map(|run| { let summary = run.summary;
                        let (more_cases_page, more_metrics_page, more_artifacts_page, more_audit_page) =
                            (run.cases.next_page(), run.metrics.next_page(), run.artifacts.next_page(), run.audit.next_page());
                        let audit_href = format!("/projects/{project_id}/audit?resourceType=EVALUATION_RUN&resourceId={}", encode(&summary.id)); view! {
                        <section class="evaluation-facts" aria-labelledby="evaluation-run-facts-title"><h2 id="evaluation-run-facts-title">"Frozen target and environment"</h2><dl>
                            <dt>"Target kind"</dt><dd>{summary.target_kind}</dd><dt>"Target ID"</dt><dd><code>{summary.target_id}</code></dd>
                            <dt>"Definition version"</dt><dd><code>{summary.definition_version_id}</code></dd><dt>"Environment version"</dt><dd><code>{summary.environment_definition_version_id}</code></dd>
                            <dt>"Duration"</dt><dd>{summary.duration_millis.map_or("Not recorded".to_string(), |millis| format!("{millis} milliseconds"))}</dd>
                            <dt>"Failure summary"</dt><dd>{summary.failure_summary.unwrap_or_else(|| "Not recorded".to_string())}</dd>
                            <dt>"Deployment evidence"</dt><dd>{summary.deployment_evidence_disposition}</dd>
                            {summary.target.map(|target| view! { <dt>"Agent content"</dt><dd><code>{target.agent_content_digest}</code></dd><dt>"Environment content"</dt><dd><code>{target.environment_content_digest}</code></dd>
                                <dt>"Catalog release"</dt><dd><code>{target.catalog_release_digest}</code></dd> })}</dl></section>
                        <section aria-labelledby="evaluation-case-title"><h2 id="evaluation-case-title">"Case projection"</h2>
                            <ul>{run.cases.rows.into_iter().map(|item| view! { <li><strong>{item.case_key}</strong>" · "{item.lifecycle_status}" · "{match item.passed { None => "Not completed", Some(true) => "Passed", Some(false) => "Failed" }}
                                {item.failure_code.filter(|code| !code.is_empty()).map(|code| format!(" · {code}"))}</li> }).collect_view()}</ul>
                            {more_cases_page.is_some().then(|| view! { <button type="button" on:click=more_cases>"Load more cases"</button> })}</section>
                        <section aria-labelledby="evaluation-metric-title"><h2 id="evaluation-metric-title">"Metric projection"</h2>
                            <ul>{run.metrics.rows.into_iter().map(|item| view! { <li><strong>{item.metric_code}</strong>" · value "{item.value}" · threshold "{item.threshold}" · "{if item.passed { "Passed" } else { "Failed" }}</li> }).collect_view()}</ul>
                            {more_metrics_page.is_some().then(|| view! { <button type="button" on:click=more_metrics>"Load more metrics"</button> })}</section>
                        <section aria-labelledby="evaluation-artifact-title"><h2 id="evaluation-artifact-title">"Artifact metadata"</h2>
                            <ul>{run.artifacts.rows.into_iter().map(|item| view! { <li>{item.artifact_kind}" · "<code>{item.content_digest}</code>" · "{item.media_type}" · "{item.byte_length}" bytes"</li> }).collect_view()}</ul>
                            {more_artifacts_page.is_some().then(|| view! { <button type="button" on:click=more_artifacts>"Load more artifacts"</button> })}</section>
                        <section aria-labelledby="evaluation-audit-title"><h2 id="evaluation-audit-title">"Audit projection"</h2>
                            <ol>{run.audit.rows.into_iter().map(|item| view! { <li>{display_time(Some(&item.occurred_at))}" · "{item.action}" · "{item.summary}</li> }).collect_view()}</ol>
                            {more_audit_page.is_some().then(|| view! { <button type="button" on:click=more_audit>"Load more audit facts"</button> })}</section>
                        <p><a href=audit_href>"Review evaluation audit history"</a></p> } })}
                    <p><a href=format!("/projects/{}/evaluations", project.get_untracked())>"Back to evaluations"</a></p> } })}
                {move || cancel_dialog.get().then(|| view! { <ConfirmationDialog title="Cancel evaluation" on_close=Callback::new(move |()| cancel_dialog.set(false))>
                    <p>"The service records an immutable cancellation outcome when the active run still owns this generation."</p>
                    <button type="button" on:click=cancel>"Cancel evaluation"</button><button type="button" on:click=move |_| cancel_dialog.set(false)>"Keep evaluation running"</button></ConfirmationDialog> })}
            </main>
        }.into_any()
    }
}
