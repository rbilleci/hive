//! The nine evaluation pages, grouped by what they show: `lists` for the project's and an
//! agent's evaluation lists, `definitions` for the draft-authoring page, `versions` for the
//! immutable version surfaces, and `runs` for one run.
//!
//! Held here is what they share: the four-state load `Kind` every page reports through
//! `state_lines`, the route parameter and capability memos, and the run list item.

mod definitions;
mod lists;
mod runs;
mod versions;

pub use definitions::EvaluationDefinitionPage;
pub use lists::{AgentEvaluationsPage, EvaluationListPage};
pub use runs::EvaluationRunPage;
pub use versions::{
    EvaluationPublicationReviewPage, EvaluationVersionComparisonPage, EvaluationVersionDetailPage,
    EvaluationVersionHistoryPage, EvaluationVersionUsagePage,
};

use crate::api::console::has_capability;
use crate::api::evaluation::EvaluationRunSummary;
use crate::format::display_time;
use crate::shell::use_console;
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;

const NO_DOCUMENT: &str = "Definition content is unavailable for this authority.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Loading,
    Ready,
    Unavailable,
    Error,
}

/// A route parameter that follows the URL.
fn route_param(name: &'static str) -> Memo<String> {
    let params = use_params_map();
    Memo::new(move |_| params.read().get(name).unwrap_or_default())
}

fn can(code: &'static str, project: Memo<String>) -> Memo<bool> {
    let console = use_console();
    Memo::new(move |_| {
        console
            .context
            .with(|context| has_capability(context, code, "PROJECT", &project.get()))
    })
}

/// The three lines every evaluation page shows for its load state.
fn state_lines(
    state: RwSignal<Kind>,
    loading: &'static str,
    unavailable: &'static str,
    error: &'static str,
) -> impl IntoView {
    view! {
        {move || (state.get() == Kind::Loading).then(|| view! { <p role="status">{loading}</p> })}
        {move || (state.get() == Kind::Unavailable).then(|| view! { <p role="status">{unavailable}</p> })}
        {move || (state.get() == Kind::Error).then(|| view! { <p role="alert">{error}</p> })}
    }
}

fn alert(message: RwSignal<String>) -> impl IntoView {
    move || {
        let text = message.get();
        (!text.is_empty()).then(|| view! { <p role="alert">{text}</p> })
    }
}

fn run_rows(project: String, runs: Vec<EvaluationRunSummary>) -> impl IntoView {
    view! { <ul class="evaluation-list">{runs.into_iter().map(|run| { let href = format!("/projects/{project}/evaluations/runs/{}", run.id); view! {
    <li><div><h3><a href=href>"Run "{run.id.clone()}</a></h3>
        <p>{run.target_kind.clone()}" · created "{display_time(Some(&run.created_at))}</p></div><span>{run.lifecycle_status.clone()}</span></li> } }).collect_view()}</ul> }
}
