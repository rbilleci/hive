//! Ports `AgentTabs.tsx`: the shared agent-scoped tab bar.

use leptos::prelude::*;

#[component]
pub fn AgentTabs(project_id: String, agent_id: String, active: &'static str) -> impl IntoView {
    let base = format!("/projects/{project_id}/agents/{agent_id}");
    let audit = format!(
        "/projects/{project_id}/audit?resourceType=AGENT&resourceId={}",
        js_sys::encode_uri_component(&agent_id)
    );
    let tabs = [
        ("overview", "Overview", base.clone()),
        ("draft", "Draft", format!("{base}/edit")),
        ("versions", "Versions", format!("{base}/versions")),
        ("deployments", "Deployments", format!("{base}/deployments")),
        ("evaluations", "Evaluations", format!("{base}/evaluations")),
        ("audit", "Audit", audit),
        ("settings", "Settings", format!("{base}/edit")),
    ];
    view! {
        <nav class="agent-tabs" aria-label="Agent sections"><ul>
            {tabs.into_iter().map(|(key, label, to)| view! { <li><a href=to aria-current=(key == active).then_some("page")>{label}</a></li> }).collect_view()}
        </ul></nav>
    }
}
