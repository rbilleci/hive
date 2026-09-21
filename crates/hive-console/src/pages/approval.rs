//! The approval inbox (all projects, or one organization) and one requirement.

use crate::api::console::has_capability;
use crate::api::deployment::{
    decide_deployment_approval, request_approval_inbox, request_approval_requirement,
    ApprovalDecisionValue, ApprovalInboxItem, DecideDeploymentApprovalInput,
};
use crate::api::page::Page;
use crate::format::{display_time, encode, joined_or, random_uuid};
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_params_map;

const UNAVAILABLE_DECISION: &str = "A decision is unavailable for this requirement.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Loading,
    Ready,
    Unavailable,
    Error,
}

fn approval_inbox(organization: bool) -> impl IntoView {
    let console = use_console();
    let params = use_params_map();
    let organization_id = Memo::new(move |_| {
        organization.then(|| params.read().get("organization_id").unwrap_or_default())
    });
    let has_view = Memo::new(move |_| {
        console.context.with(|context| {
            context.capabilities.iter().any(|capability| {
                capability.code == "DEPLOYMENT_APPROVAL.VIEW"
                    && capability.scope_type == "PROJECT"
                    && organization_id.get().is_none_or(|id| {
                        context
                            .organizations
                            .iter()
                            .find(|candidate| candidate.id.inner() == id)
                            .is_some_and(|candidate| {
                                candidate
                                    .projects
                                    .iter()
                                    .any(|project| project.id == capability.scope_id)
                            })
                    })
            })
        })
    });
    let route_key = Memo::new(move |_| {
        (
            console
                .context
                .with(|context| context.principal.id.inner().to_string()),
            console.revision.get(),
            organization_id.get(),
        )
    });
    let kind = RwSignal::new(Kind::Loading);
    let page = RwSignal::new(None::<Page<ApprovalInboxItem>>);
    let serial = StoredValue::new(0_u32);
    let load = move |number: i32, append: bool| {
        if !has_view.get_untracked() {
            kind.set(Kind::Unavailable);
            return;
        }
        let this = serial.get_value() + 1;
        serial.set_value(this);
        kind.set(Kind::Loading);
        let scope = organization_id.get_untracked();
        spawn_local(async move {
            let result = request_approval_inbox(scope.as_deref(), number).await;
            if serial.try_get_value() != Some(this) {
                return;
            }
            match result {
                Ok(Some(next)) => {
                    page.update(|prior| match prior {
                        Some(prior) if append => prior.extend(next),
                        _ => *prior = Some(next),
                    });
                    kind.set(Kind::Ready);
                }
                Ok(None) | Err(GraphqlError::SessionExpired) => {
                    page.set(None);
                    kind.set(Kind::Unavailable);
                }
                Err(GraphqlError::Transport(_)) => kind.set(Kind::Error),
            }
        });
    };
    Effect::new(move |_| {
        let _ = route_key.get();
        let _ = has_view.get();
        page.set(None);
        load(0, false);
    });
    // A Memo, because setting `kind` to the value it already holds still notifies, and the page must not remount on every reload.
    let unavailable = Memo::new(move |_| kind.get() == Kind::Unavailable);
    move || {
        if !has_view.get() || unavailable.get() {
            return view! { <main class="approval-page"><p role="status">"Approvals are unavailable."</p></main> }.into_any();
        }
        view! {
            <main class="approval-page" aria-labelledby="approval-inbox-title">
                <PageHeader title_id="approval-inbox-title" title="Approval inbox".to_string()><button type="button" on:click=move |_| load(0, false)>"Refresh approvals"</button></PageHeader>
                {move || (kind.get() == Kind::Error).then(|| view! { <p role="alert">{if page.with(Option::is_some) { "We could not refresh approvals. Displayed rows remain from the last successful request." } else { "We could not load approvals. Retry the request." }}</p> })}
                {move || (kind.get() == Kind::Loading && page.with(Option::is_none)).then(|| view! { <p role="status">"Loading approval requirements…"</p> })}
                {move || page.with(|page| page.as_ref().is_some_and(|page| page.rows.is_empty())).then(|| view! { <p role="status">"No approval requirements are available."</p> })}
                {move || page.get().filter(|page| !page.rows.is_empty()).map(|shown| { let more = shown.next_page(); view! {
                    <ul class="approval-list">{shown.rows.into_iter().map(|item| { let status = item.status.clone(); let decision_available = item.decision_available; let (qualifying, required) = (item.qualifying_approval_count, item.required_approvers); let risk = item.approval_snapshot.risk.clone(); let expires_at = item.expires_at.clone(); let id = item.id.clone(); let deployment = item.deployments; view! {
                        <li><a href=format!("/projects/{}/deployments/{}/approvals/{}", deployment.as_ref().map(|value| value.project_id.clone()).unwrap_or_default(), deployment.as_ref().map(|value| value.id.clone()).unwrap_or_default(), id)>
                            <div><h2>{deployment.as_ref().map(|value| value.agent_display_name()).unwrap_or_default()}" · v"{deployment.as_ref().map_or(0, |value| value.agent_version_number())}</h2>
                                <p>{deployment.as_ref().and_then(|value| value.environment_definition_versions.as_ref()).map(|value| value.display_name.clone()).unwrap_or_default()}" · "{status.clone()}" · "{qualifying}"/"{required}" qualifying approvals"</p>
                                <p>"Risk "{risk}" · expires "{display_time(Some(&expires_at))}</p></div>
                            <span class=format!("deployment-status deployment-status-{}", status.to_lowercase())>{if decision_available { "Review" } else { "Read-only" }}</span></a></li> } }).collect_view()}</ul>
                    {more.map(|number| view! { <button type="button" on:click=move |_| load(number, true)>"Load more approvals"</button> })} } })}
            </main>
        }.into_any()
    }
}

#[component]
pub fn ApprovalInboxPage() -> impl IntoView {
    approval_inbox(false)
}

#[component]
pub fn OrganizationApprovalInboxPage() -> impl IntoView {
    approval_inbox(true)
}

/// A decision whose outcome is unknown. Resubmitting the same choice reuses its key and revision,
/// so the server returns the immutable result instead of recording a second decision.
#[derive(Clone, PartialEq)]
struct PendingDecision {
    key: String,
    decision: ApprovalDecisionValue,
    comment: String,
    reason: String,
    revision: i64,
}

#[component]
pub fn ApprovalDetailPage() -> impl IntoView {
    let console = use_console();
    let params = use_params_map();
    let route = Memo::new(move |_| {
        let read = params.read();
        (
            read.get("project_id").unwrap_or_default(),
            read.get("deployment_id").unwrap_or_default(),
            read.get("approval_requirement_id").unwrap_or_default(),
        )
    });
    let has_view = Memo::new(move |_| {
        console.context.with(|context| {
            has_capability(
                context,
                "DEPLOYMENT_APPROVAL.VIEW",
                "PROJECT",
                &route.get().0,
            )
        })
    });
    let route_key = Memo::new(move |_| {
        (
            console
                .context
                .with(|context| context.principal.id.inner().to_string()),
            console.revision.get(),
            route.get(),
        )
    });
    let item = RwSignal::new(None::<ApprovalInboxItem>);
    let state = RwSignal::new(Kind::Loading);
    let decision = RwSignal::new(ApprovalDecisionValue::Approve);
    let (comment, reason, message) = (
        RwSignal::new(String::new()),
        RwSignal::new("CHANGE_SCOPE_NOT_APPROVED".to_string()),
        RwSignal::new(String::new()),
    );
    let submitting = RwSignal::new(false);
    let pending = StoredValue::new(None::<PendingDecision>);
    let same_route = move |key: &(String, String, (String, String, String))| {
        route_key.try_get_untracked().as_ref() == Some(key)
    };
    let abandon = move |next: Kind| {
        pending.set_value(None);
        item.set(None);
        message.set(String::new());
        state.set(next);
    };

    let load = move || async move {
        let key = route_key.get_untracked();
        let (project, deployment, id) = key.2.clone();
        if id.is_empty() || !has_view.get_untracked() {
            abandon(Kind::Unavailable);
            submitting.set(false);
            return None;
        }
        if state.get_untracked() == Kind::Ready {
            state.set(Kind::Loading);
        }
        let result = request_approval_requirement(&id).await;
        if !same_route(&key) {
            return None;
        }
        match result {
            Ok(Some(found))
                if found
                    .deployments
                    .as_ref()
                    .is_some_and(|row| row.id == deployment && row.project_id == project) =>
            {
                item.set(Some(found.clone()));
                state.set(Kind::Ready);
                Some(found)
            }
            Ok(_) | Err(GraphqlError::SessionExpired) => {
                abandon(Kind::Unavailable);
                None
            }
            Err(GraphqlError::Transport(_)) => {
                abandon(Kind::Error);
                None
            }
        }
    };
    Effect::new(move |_| {
        let _ = route_key.get();
        let _ = has_view.get();
        pending.set_value(None);
        spawn_local(async move {
            load().await;
        });
    });

    let submit = move |event: ev::SubmitEvent| {
        event.prevent_default();
        message.set(String::new());
        let key = route_key.get_untracked();
        spawn_local(async move {
            let prior = pending.get_value();
            let current = if prior.is_some() {
                item.get_untracked()
            } else {
                load().await
            };
            let Some(current) = current.filter(|current| current.decision_available) else {
                message.set(UNAVAILABLE_DECISION.to_string());
                return;
            };
            let (chosen, note, why) = (
                decision.get_untracked(),
                comment.get_untracked(),
                reason.get_untracked(),
            );
            if chosen == ApprovalDecisionValue::Reject && why.trim().is_empty() {
                message.set("Enter a rejection reason before recording that decision.".to_string());
                return;
            }
            let request = prior
                .filter(|prior| {
                    prior.decision == chosen && prior.comment == note && prior.reason == why
                })
                .unwrap_or_else(|| PendingDecision {
                    key: random_uuid(),
                    decision: chosen,
                    comment: note,
                    reason: why,
                    revision: i64::from(current.revision),
                });
            pending.set_value(Some(request.clone()));
            submitting.set(true);
            let approve = request.decision == ApprovalDecisionValue::Approve;
            let result = decide_deployment_approval(DecideDeploymentApprovalInput {
                approval_requirement_id: current.id.as_str().into(),
                expected_revision: request.revision,
                decision: request.decision,
                idempotency_key: request.key.as_str().into(),
                comment: Some(request.comment).filter(|text| approve && !text.is_empty()),
                rejection_reason: Some(request.reason).filter(|text| !approve && !text.is_empty()),
            })
            .await;
            if !same_route(&key) {
                return;
            }
            match result {
                Err(GraphqlError::SessionExpired) | Ok(None) => abandon(Kind::Unavailable),
                Err(GraphqlError::Transport(_)) => message.set("We could not confirm the decision. Submit the same decision again to recover its immutable result.".to_string()),
                Ok(Some(payload)) => {
                    pending.set_value(None);
                    message.set(payload.problems.first().map_or_else(|| if chosen == ApprovalDecisionValue::Approve { "The immutable approval was recorded." } else { "The immutable rejection was recorded." }.to_string(), |problem| problem.message.clone()));
                    // `load` clears the message only when the requirement vanishes.
                    let shown = message.get_untracked();
                    if load().await.is_some() { message.set(shown); }
                }
            }
            if same_route(&key) {
                submitting.set(false);
            }
        });
    };

    let displayed = Memo::new(move |_| {
        let (project, deployment, id) = route.get();
        item.get().filter(|item| {
            item.id == id
                && item
                    .deployments
                    .as_ref()
                    .is_some_and(|row| row.id == deployment && row.project_id == project)
        })
    });
    let enabled = move || {
        displayed.with(|item| item.as_ref().is_some_and(|item| item.decision_available))
            && !submitting.get()
    };
    let retry = move |_| {
        spawn_local(async move {
            load().await;
        })
    };

    // A Memo, because setting `state` to the value it already holds still notifies, and the page must not remount on every reload.
    let unavailable = Memo::new(move |_| state.get() == Kind::Unavailable);
    move || {
        if !has_view.get() || unavailable.get() {
            return view! { <main class="approval-page"><p role="status">"This approval requirement is unavailable."</p></main> }.into_any();
        }
        let Some(shown) = displayed.get() else {
            return if state.get() == Kind::Error {
                view! { <main class="approval-page"><p role="alert">"We could not load this approval requirement."</p><button type="button" on:click=retry>"Retry approval requirement"</button></main> }.into_any()
            } else {
                view! { <main class="approval-page"><p role="status">"Loading approval requirement…"</p></main> }.into_any()
            };
        };
        let decisions = shown.decisions();
        let requester_id = shown.requester_id();
        // `displayed` already matched the route against this row's own deployment, so it is
        // present; a requirement whose deployment is invisible is "unavailable", never a panic.
        let Some(deployment) = shown.deployments.clone() else {
            return view! { <main class="approval-page"><p role="status">"This approval requirement is unavailable."</p></main> }.into_any();
        };
        let requirement = shown;
        let (snapshot, review) = (
            requirement.approval_snapshot.clone(),
            deployment.plan.as_ref().map(|plan| plan.review.clone()),
        );
        let (project, deployment_id, requirement_id) = (
            deployment.project_id.clone(),
            deployment.id.clone(),
            requirement.id.clone(),
        );
        let decision_available = requirement.decision_available;
        view! {
            <main class="approval-page" aria-labelledby="approval-detail-title">
                <PageHeader title_id="approval-detail-title" title=format!("{} · v{}", deployment.agent_display_name(), deployment.agent_version_number())
                    meta=format!("{} · expires {}", requirement.status, display_time(Some(&requirement.expires_at)))><button type="button" on:click=retry>"Refresh requirement"</button></PageHeader>
                {move || (state.get() == Kind::Error).then(|| view! { <p role="alert">"We could not load this approval requirement."</p> })}
                {move || { let text = message.get(); (!text.is_empty()).then(|| view! { <p role=if text.contains("could") { "alert" } else { "status" }>{text.clone()}</p> }) }}
                <section class="approval-facts"><h2>"Review context"</h2><dl>
                    <dt>"Project"</dt><dd><code>{project.clone()}</code></dd>
                    <dt>"Requester"</dt><dd><code>{requester_id}</code>" · "{display_time(Some(&deployment.requested_at))}</dd>
                    <dt>"Requested agent version"</dt><dd>"v"{deployment.agent_version_number()}</dd>
                    <dt>"Active version at request"</dt><dd>{review.as_ref().and_then(|review| review.active_agent_version_number).map_or("No active version".to_string(), |number| format!("v{number}"))}</dd>
                    <dt>"Deployment strategy"</dt><dd>{deployment.strategy.clone()}</dd>
                    <dt>"Change summary"</dt><dd>{review.as_ref().map_or_else(|| "The immutable local plan does not include a semantic change summary.".to_string(), |review| review.change_summary.clone())}</dd>
                    <dt>"Added dependencies"</dt><dd>{joined_or(&review.as_ref().map(|review| review.added_dependency_versions.clone()).unwrap_or_default(), "None")}</dd>
                    <dt>"Removed dependencies"</dt><dd>{joined_or(&review.as_ref().map(|review| review.removed_dependency_versions.clone()).unwrap_or_default(), "None")}</dd>
                    <dt>"Cost impact"</dt><dd>"Unavailable in the local MVP."</dd></dl></section>
                <section class="approval-facts"><h2>"Frozen policy and target facts"</h2><dl>
                    <dt>"Environment"</dt><dd>{deployment.environment_definition_versions.as_ref().map(|value| value.display_name.clone()).unwrap_or_default()}" · "{snapshot.environment_class.clone()}</dd>
                    <dt>"Risk"</dt><dd>{snapshot.risk.clone()}</dd>
                    <dt>"Required evidence"</dt><dd>{joined_or(&snapshot.rule.required_evidence, "None")}</dd>
                    <dt>"Distinct approvers"</dt><dd>{requirement.qualifying_approval_count}"/"{requirement.required_approvers}</dd>
                    <dt>"Policy"</dt><dd>"revision "{snapshot.policy_revision}" · "<code>{snapshot.policy_digest}</code></dd>
                    <dt>"Version digest"</dt><dd><code>{snapshot.target.agent_version_digest}</code></dd>
                    <dt>"Target digest"</dt><dd><code>{snapshot.target.target_digest}</code></dd>
                    <dt>"Plan digest"</dt><dd><code>{snapshot.target.deployment_plan_digest}</code></dd>
                    <dt>"Artifact digest"</dt><dd><code>{snapshot.target.artifact_digest}</code></dd>
                    <dt>"Evidence"</dt><dd>{snapshot.evidence.iter().map(|evidence| format!("{}: {} · {} · {}", evidence.kind, evidence.state, evidence.digest.as_deref().unwrap_or("null"), display_time(evidence.expires_at.as_deref()))).collect::<Vec<_>>().join("; ")}</dd></dl></section>
                <section class="approval-decisions"><h2>"Recorded decisions"</h2>
                    {if decisions.is_empty() { view! { <p role="status">"No decision has been recorded."</p> }.into_any() } else { view! {
                        <ol>{decisions.into_iter().map(|entry| view! { <li><strong>{entry.decision.clone()}</strong><span>{display_time(Some(&entry.decided_at))}</span>
                            {entry.comment.filter(|text| !text.is_empty()).map(|text| view! { <p>{text}</p> })}{entry.rejection_reason.filter(|text| !text.is_empty()).map(|text| view! { <p>{text}</p> })}</li> }).collect_view()}</ol> }.into_any() }}</section>
                <section class="approval-form"><h2>"Record a decision"</h2>
                    {(!decision_available).then(|| view! { <p role="status">{UNAVAILABLE_DECISION}</p> })}
                    <form on:submit=submit>
                        <label>"Decision"<select prop:value=move || decision.get().as_str() disabled=move || !enabled() on:change=move |event| {
                            let next = if event_target_value(&event) == "REJECT" { ApprovalDecisionValue::Reject } else { ApprovalDecisionValue::Approve };
                            decision.set(next); if next == ApprovalDecisionValue::Reject { comment.set(String::new()); } }>
                            <option value="APPROVE" selected=move || decision.get() == ApprovalDecisionValue::Approve>"Approve"</option>
                            <option value="REJECT" selected=move || decision.get() == ApprovalDecisionValue::Reject>"Reject"</option></select></label>
                        <p role="note">"Select a standard review code. The server stores no arbitrary review text in immutable audit history."</p>
                        {move || if decision.get() == ApprovalDecisionValue::Approve { view! {
                            <label>"Approval comment (optional)"<select prop:value=move || comment.get() disabled=move || !enabled() on:change=move |event| comment.set(event_target_value(&event))>
                                {[("", "No comment"), ("REVIEWED_CHANGE_SCOPE", "Reviewed change scope"), ("AUTHORIZATION_GRANTED", "Authorization granted")].into_iter().map(|(value, label)| view! {
                                    <option value=value selected=move || comment.get() == value>{label}</option> }).collect_view()}</select></label> }.into_any() } else { view! {
                            <label>"Rejection reason"<select prop:value=move || reason.get() disabled=move || !enabled() required on:change=move |event| reason.set(event_target_value(&event))>
                                {[("CHANGE_SCOPE_NOT_APPROVED", "Change scope not approved"), ("UNACCEPTABLE_CHANGE_SCOPE", "Unacceptable change scope")].into_iter().map(|(value, label)| view! {
                                    <option value=value selected=move || reason.get() == value>{label}</option> }).collect_view()}</select></label> }.into_any() }}
                        <button type="submit" disabled=move || !enabled()>{move || if submitting.get() { "Recording decision…" } else if decision.get() == ApprovalDecisionValue::Approve { "Approve deployment" } else { "Reject deployment" }}</button>
                    </form></section>
                <p><a href=format!("/projects/{project}/deployments/{deployment_id}")>"Open immutable deployment audit correlation"</a></p>
                <p><a href=format!("/projects/{project}/audit?resourceType=APPROVAL_REQUIREMENT&resourceId={}", encode(&requirement_id))>"Review approval audit history"</a></p>
            </main>
        }.into_any()
    }
}
