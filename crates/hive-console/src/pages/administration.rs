//! Ports `OrganizationAdministration.tsx`, `ProjectAdministration.tsx`, and `MembershipControls.tsx`.

use crate::api::administration::{
    add_membership, archive_scope, end_membership, replace_membership_roles,
    request_organization_administration, request_project_administration, restore_scope,
    update_approval_policy, update_budget_policy, update_project_general, AdministrationMembership,
    AdministrationMembershipInput, AdministrationMutationPayload, AdministrationPrincipal,
    ApprovalPolicyCellInput, ApprovalPolicyRule, EndAdministrationMembershipInput,
    FixedApprovalPolicyMatrixInput, LifecycleAdministrationInput, OrganizationAdministrationFields,
    ProjectAdministrationFields, ReplaceAdministrationMembershipInput,
    UpdateProjectApprovalPolicyInput, UpdateProjectBudgetPolicyInput, UpdateProjectGeneralInput,
    NINE_CELLS,
};
use crate::confirmation_dialog::ConfirmationDialog;
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_params_map;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

type Mutation = Pin<Box<dyn Future<Output = Result<AdministrationMutationPayload, GraphqlError>>>>;
type Fetch<T> = fn(String) -> Pin<Box<dyn Future<Output = Result<Option<T>, GraphqlError>>>>;

/// `ORGANIZATION_ADMIN` reads as `organization admin`.
fn role_label(role: &str) -> String {
    role.replace('_', " ").to_lowercase()
}

fn encode(value: &str) -> String {
    String::from(js_sys::encode_uri_component(value))
}

#[component]
fn RoleSelector(
    legend: String,
    roles: Vec<String>,
    #[prop(into)] selected: Signal<Vec<String>>,
    change: Callback<Vec<String>>,
) -> impl IntoView {
    view! {
        <fieldset class="role-selector"><legend>{legend}</legend>
            {roles.into_iter().map(|role| { let (checked, toggled, label) = (role.clone(), role.clone(), role_label(&role)); view! {
                <label><input type="checkbox" prop:checked=move || selected.with(|all| all.contains(&checked))
                    on:change=move |event| { let mut next = selected.get_untracked(); next.retain(|entry| *entry != toggled); if event_target_checked(&event) { next.push(toggled.clone()); next.sort(); } change.run(next); } />
                    <span>{label}</span></label> } }).collect_view()}
        </fieldset>
    }
}

#[component]
fn AddMemberForm(
    label: &'static str,
    principals: Vec<AdministrationPrincipal>,
    roles: Vec<String>,
    principal_id: RwSignal<String>,
    selected_roles: RwSignal<Vec<String>>,
    submit: Callback<()>,
) -> impl IntoView {
    let none_eligible = principals.is_empty();
    view! {
        <form class="member-form" on:submit=move |event: ev::SubmitEvent| { event.prevent_default(); submit.run(()); }>
            <h3>{label}</h3>
            <label>"Known principal"<select required prop:value=move || principal_id.get() on:change=move |event| principal_id.set(event_target_value(&event))>
                <option value="" disabled selected=move || principal_id.with(String::is_empty)>"Select a principal"</option>
                {principals.into_iter().map(|principal| { let id = principal.id.inner().to_string(); let chosen = id.clone(); view! {
                    <option value=id selected=move || principal_id.get() == chosen>{principal.display_name}" · "{principal.email}</option> } }).collect_view()}
            </select></label>
            {none_eligible.then(|| view! { <p role="status">"No eligible known principals are available in this organization."</p> })}
            <RoleSelector legend="Assigned roles".to_string() roles=roles selected=selected_roles change=Callback::new(move |next| selected_roles.set(next)) />
            <p><strong>"Selected:"</strong>" "{move || selected_roles.with(|all| if all.is_empty() { "No roles".to_string() } else { all.iter().map(|role| role_label(role)).collect::<Vec<_>>().join(", ") })}</p>
            <button type="submit" disabled=move || principal_id.with(String::is_empty) || selected_roles.with(Vec::is_empty)>"Add member"</button>
        </form>
    }
}

/// The state both administration pages share: one loaded record, the last refusal, per-member role
/// edits, and a `mutate` that applies a refusal, reloads, and rechecks console access.
struct Administration<T: Clone + Send + Sync + 'static> {
    data: RwSignal<Option<T>>,
    loaded: RwSignal<bool>,
    problem: RwSignal<String>,
    roles: RwSignal<BTreeMap<String, Vec<String>>>,
    reload: Callback<bool>,
    /// Runs a mutation; the callback receives whether the server accepted it.
    mutate: Callback<(Mutation, Callback<bool>)>,
}

impl<T: Clone + Send + Sync + 'static> Clone for Administration<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Clone + Send + Sync + 'static> Copy for Administration<T> {}

fn use_administration<T: Clone + Send + Sync + 'static>(
    key: Memo<String>,
    unavailable: &'static str,
    fetch: Fetch<T>,
    on_loaded: Callback<T>,
) -> Administration<T> {
    let refresh = use_console().refresh;
    let data = RwSignal::new(None::<T>);
    let loaded = RwSignal::new(false);
    let problem = RwSignal::new(String::new());
    let roles = RwSignal::new(BTreeMap::new());
    let after = StoredValue::new(None::<(bool, Callback<bool>)>);
    let reload = Callback::new(move |preserve_problem: bool| {
        loaded.set(false);
        let id = key.get_untracked();
        spawn_local(async move {
            let result = fetch(id).await.ok().flatten();
            if result.is_none() || !preserve_problem {
                problem.set(if result.is_some() {
                    String::new()
                } else {
                    unavailable.to_string()
                });
            }
            if let Some(value) = &result {
                on_loaded.run(value.clone());
            }
            data.set(result);
            loaded.set(true);
            if let Some((accepted, done)) = after.try_update_value(Option::take).flatten() {
                refresh.run(());
                done.run(accepted);
            }
        });
    });
    Effect::new(move |_| {
        key.track();
        let _ = key.get();
        reload.run(false);
    });
    let mutate = Callback::new(move |(request, done): (Mutation, Callback<bool>)| {
        spawn_local(async move {
            match request.await {
                Ok(payload) => {
                    let refusal = payload
                        .problems
                        .iter()
                        .map(|entry| entry.message.clone())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let accepted = refusal.is_empty();
                    problem.set(refusal);
                    after.set_value(Some((accepted, done)));
                    reload.run(!accepted);
                }
                Err(_) => {
                    problem.set(unavailable.to_string());
                    data.set(None);
                    loaded.set(true);
                    done.run(false);
                }
            }
        });
    });
    Administration {
        data,
        loaded,
        problem,
        roles,
        reload,
        mutate,
    }
}

const IGNORE: fn(bool) = |_| ();

/// The member list both pages render; `show_project_access` adds the organization page's summary line.
#[allow(clippy::too_many_arguments)]
fn member_list(
    memberships: Vec<AdministrationMembership>,
    assignable: Vec<String>,
    scope: &'static str,
    scope_id: String,
    can_change: bool,
    can_end: bool,
    show_project_access: bool,
    roles: RwSignal<BTreeMap<String, Vec<String>>>,
    mutate: Callback<(Mutation, Callback<bool>)>,
    open_end: Callback<(String, i32)>,
) -> impl IntoView {
    view! {
        <ul class="member-list">{memberships.into_iter().map(|membership| {
            let id = membership.id.inner().to_string();
            let active = membership.ended_at.is_none();
            let current = { let (id, fallback) = (id.clone(), membership.role_codes.clone()); Signal::derive(move || roles.with(|all| all.get(&id).cloned().unwrap_or_else(|| fallback.clone()))) };
            let (edit_id, end_id, scope_id, revision) = (id.clone(), id.clone(), scope_id.clone(), membership.revision);
            view! {
                <li><div class="member-summary"><strong>{membership.display_name.clone()}</strong><span>{membership.email.clone()}</span>
                    <span>{if active { "Active" } else { "Ended" }}" · revision "{membership.revision}</span>
                    <span>"Roles: "{membership.role_codes.join(", ")}</span>
                    {show_project_access.then(|| view! { <span>"Project access: "{if membership.project_access_summary.is_empty() { "No direct project roles".to_string() } else { membership.project_access_summary.join("; ") }}</span> })}
                    <span>"Started "{membership.started_at.clone()}" · Last seen "{membership.last_seen_at.clone().unwrap_or_else(|| "Unknown".to_string())}</span></div>
                    {(active && can_change).then(|| { let (store_id, membership_id) = (edit_id.clone(), edit_id.clone()); view! {
                        <div class="member-role-editor">
                            <RoleSelector legend=format!("{} roles", membership.display_name) roles=assignable.clone() selected=current
                                change=Callback::new(move |next: Vec<String>| roles.update(|all| { all.insert(store_id.clone(), next); })) />
                            <p><strong>"Selected:"</strong>" "{move || current.get().join(", ")}</p>
                            <button class="primary-action" type="button" disabled=move || current.with(Vec::is_empty)
                                on:click=move |_| mutate.run((Box::pin(replace_membership_roles(ReplaceAdministrationMembershipInput { scope: scope.to_string(), scope_id: scope_id.as_str().into(),
                                    membership_id: membership_id.as_str().into(), role_codes: current.get_untracked(), expected_revision: revision })), Callback::new(IGNORE)))>"Replace roles"</button>
                        </div> } })}
                    {(active && can_end).then(|| view! { <button class="danger-action" type="button" on:click=move |_| open_end.run((end_id.clone(), revision))>"End membership"</button> })}
                </li>
            }
        }).collect_view()}</ul>
    }
}

fn eligible(
    principals: &[AdministrationPrincipal],
    memberships: &[AdministrationMembership],
) -> Vec<AdministrationPrincipal> {
    principals
        .iter()
        .filter(|principal| {
            !memberships.iter().any(|membership| {
                membership.principal_id == principal.id && membership.ended_at.is_none()
            })
        })
        .cloned()
        .collect()
}

#[derive(Clone, PartialEq)]
enum OrganizationDialog {
    Archive,
    End(String, i32),
}

#[component]
pub fn OrganizationAdministrationPage() -> impl IntoView {
    let params = use_params_map();
    let organization_id =
        Memo::new(move |_| params.read().get("organization_id").unwrap_or_default());
    let page = use_administration::<OrganizationAdministrationFields>(
        organization_id,
        "This organization is unavailable.",
        |id| Box::pin(async move { request_organization_administration(&id).await }),
        Callback::new(|_| ()),
    );
    let dialog = RwSignal::new(None::<OrganizationDialog>);
    let (reason, confirmation) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let (member_principal, member_roles) = (
        RwSignal::new(String::new()),
        RwSignal::new(vec!["ORGANIZATION_MEMBER".to_string()]),
    );
    let open_dialog = move |next: OrganizationDialog| {
        page.problem.set(String::new());
        reason.set(String::new());
        confirmation.set(String::new());
        dialog.set(Some(next));
    };
    let close_when_accepted = Callback::new(move |accepted: bool| {
        if accepted {
            dialog.set(None);
        }
    });
    let _ = page.reload;

    move || {
        if !page.loaded.get() {
            return view! { <main class="administration"><p role="status">"Loading organization administration…"</p></main> }.into_any();
        }
        let Some(data) = page.data.get() else {
            return view! { <main class="administration"><p role="status">"This organization is unavailable."</p><p role="alert">{move || page.problem.get()}</p></main> }.into_any();
        };
        let can = |capability: &str| data.capabilities.iter().any(|code| code == capability);
        let id = data.id.inner().to_string();
        let (add_id, archive_id, restore_id, end_scope, slug, revision) = (
            id.clone(),
            id.clone(),
            id.clone(),
            id.clone(),
            data.slug.clone(),
            data.revision,
        );
        let confirm_slug = slug.clone();
        view! {
            <main class="administration" aria-labelledby="organization-administration-title">
                <PageHeader title_id="organization-administration-title" title=format!("{} administration", data.display_name) meta=format!("ID {} · {}", data.slug, data.lifecycle_status) />
                <a href=format!("/organizations/{id}")>"Return to overview"</a>
                <p><a href=format!("/organizations/{id}/audit?resourceType=ORGANIZATION&resourceId={}", encode(&id))>"Review organization audit history"</a></p>
                {move || { let text = page.problem.get(); (!text.is_empty() && dialog.with(Option::is_none)).then(|| view! { <p role="alert">{text}</p> }) }}
                <section><h2>"Members"</h2>
                    {member_list(data.memberships.clone(), data.assignable_roles.clone(), "ORGANIZATION", id.clone(), can("ORGANIZATION_MEMBERSHIP.CHANGE_ROLES"), can("ORGANIZATION_MEMBERSHIP.END"), true,
                        page.roles, page.mutate, Callback::new(move |(membership, revision): (String, i32)| open_dialog(OrganizationDialog::End(membership, revision))))}
                    {can("ORGANIZATION_MEMBERSHIP.ADD").then(|| view! { <AddMemberForm label="Add organization member" principals=eligible(&data.available_principals, &data.memberships) roles=data.assignable_roles.clone()
                        principal_id=member_principal selected_roles=member_roles submit=Callback::new(move |()| page.mutate.run((Box::pin(add_membership(AdministrationMembershipInput { scope: "ORGANIZATION".to_string(),
                            scope_id: add_id.as_str().into(), principal_id: member_principal.get_untracked().as_str().into(), role_codes: member_roles.get_untracked(), expected_scope_revision: revision })), Callback::new(IGNORE)))) /> })}
                </section>
                <section><h2>"Danger zone"</h2>
                    {(data.lifecycle_status == "ACTIVE" && can("ORGANIZATION.ARCHIVE")).then(|| view! { <button class="danger-action" type="button" on:click=move |_| open_dialog(OrganizationDialog::Archive)>"Archive organization"</button> })}
                    {(data.lifecycle_status == "ARCHIVED" && can("ORGANIZATION.RESTORE")).then(|| view! { <button type="button" on:click=move |_| page.mutate.run((Box::pin(restore_scope(LifecycleAdministrationInput {
                        scope: "ORGANIZATION".to_string(), scope_id: restore_id.as_str().into(), expected_revision: revision, reason: None, confirmation: None })), Callback::new(IGNORE)))>"Restore organization"</button> })}
                </section>
                {move || match dialog.get() {
                    None => None,
                    Some(OrganizationDialog::Archive) => { let (scope_id, slug, expected) = (archive_id.clone(), confirm_slug.clone(), confirm_slug.clone()); Some(view! {
                        <ConfirmationDialog title="Archive organization" on_close=Callback::new(move |()| dialog.set(None))>
                            <p>"Projects, memberships, policies, spend, and audit history are retained. Runtime work is not stopped."</p>
                            <label>"Reason"<input aria-describedby="organization-archive-error" aria-label="Archive reason" prop:value=move || reason.get() on:input=move |event| reason.set(event_target_value(&event)) /></label>
                            <label>"Type organization ID "{slug}" exactly"<input aria-describedby="organization-archive-error" prop:value=move || confirmation.get() on:input=move |event| confirmation.set(event_target_value(&event)) /></label>
                            {move || { let text = page.problem.get(); (!text.is_empty()).then(|| view! { <p id="organization-archive-error" role="alert">{text}</p> }) }}
                            <button class="danger-action" disabled={ let expected = expected.clone(); move || reason.with(|value| value.trim().is_empty()) || confirmation.with(|value| value.trim() != expected) }
                                on:click=move |_| page.mutate.run((Box::pin(archive_scope(LifecycleAdministrationInput { scope: "ORGANIZATION".to_string(), scope_id: scope_id.as_str().into(), expected_revision: revision,
                                    reason: Some(reason.get_untracked()), confirmation: Some(confirmation.get_untracked()) })), close_when_accepted))>"Archive organization"</button>
                            <button on:click=move |_| dialog.set(None)>"Cancel"</button>
                        </ConfirmationDialog> }.into_any()) }
                    Some(OrganizationDialog::End(membership, membership_revision)) => { let scope_id = end_scope.clone(); Some(view! {
                        <ConfirmationDialog title="End membership" on_close=Callback::new(move |()| dialog.set(None))>
                            <p>"Access ends immediately; history remains and pending approval ownership can be affected."</p>
                            <label>"Reason"<input aria-describedby="organization-end-error" prop:value=move || reason.get() on:input=move |event| reason.set(event_target_value(&event)) /></label>
                            {move || { let text = page.problem.get(); (!text.is_empty()).then(|| view! { <p id="organization-end-error" role="alert">{text}</p> }) }}
                            <button class="danger-action" disabled=move || reason.with(|value| value.trim().is_empty())
                                on:click=move |_| page.mutate.run((Box::pin(end_membership(EndAdministrationMembershipInput { scope: "ORGANIZATION".to_string(), scope_id: scope_id.as_str().into(),
                                    membership_id: membership.as_str().into(), expected_revision: membership_revision, reason: reason.get_untracked() })), close_when_accepted))>"End membership"</button>
                            <button on:click=move |_| dialog.set(None)>"Cancel"</button>
                        </ConfirmationDialog> }.into_any()) }
                }}
            </main>
        }.into_any()
    }
}

const EVIDENCE_OPTIONS: [&str; 3] = [
    "PLAN_VALIDATED",
    "CHANGE_SUMMARY_READY",
    "EVALUATION_PASSED",
];

fn money(amount: Option<i32>, currency: Option<&str>) -> String {
    match (amount, currency.filter(|code| !code.is_empty())) {
        (Some(amount), Some(currency)) => format!("{currency} {:.2}", f64::from(amount) / 100.0),
        _ => "Unknown".to_string(),
    }
}

fn budget_icon(state: &str) -> &'static str {
    match state {
        "NOT_CONFIGURED" => "○",
        "NORMAL" => "✓",
        "WARNING" => "!",
        "EXCEEDED" => "×",
        _ => "?",
    }
}

/// The editable matrix in `NINE_CELLS` order; a cell the policy does not name requires the plan check alone.
fn matrix_from(rules: &[ApprovalPolicyRule]) -> Vec<ApprovalPolicyCellInput> {
    NINE_CELLS
        .iter()
        .map(|cell| {
            rules.iter().find(|rule| rule.cell == *cell).map_or(
                ApprovalPolicyCellInput {
                    required_evidence: vec!["PLAN_VALIDATED".to_string()],
                    required_approvers: 0,
                },
                |rule| ApprovalPolicyCellInput {
                    required_evidence: rule.required_evidence.clone(),
                    required_approvers: rule.required_approvers,
                },
            )
        })
        .collect()
}

#[derive(Clone, PartialEq)]
enum ProjectDialog {
    Archive,
    End(String, i32),
}

#[component]
pub fn ProjectAdministrationPage() -> impl IntoView {
    let console = use_console();
    let params = use_params_map();
    let project_id = Memo::new(move |_| params.read().get("project_id").unwrap_or_default());
    let (display_name, description) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let (currency, limit, warning, budget_reason) = (
        RwSignal::new("USD".to_string()),
        RwSignal::new("500000".to_string()),
        RwSignal::new("400000".to_string()),
        RwSignal::new("Local adjustment".to_string()),
    );
    let matrix = RwSignal::new(matrix_from(&[]));
    let adopt = Callback::new(move |project: ProjectAdministrationFields| {
        display_name.set(project.display_name.clone());
        description.set(project.description.clone());
        matrix.set(matrix_from(
            project
                .approval_policy
                .as_ref()
                .map_or(&[][..], |policy| &policy.matrix),
        ));
        if let Some(policy) = &project.budget_policy {
            currency.set(policy.currency.clone());
            limit.set(policy.monthly_limit_cents.to_string());
            warning.set(policy.warning_threshold_cents.to_string());
            budget_reason.set("Local adjustment".to_string());
        }
    });
    let page = use_administration::<ProjectAdministrationFields>(
        project_id,
        "This project is unavailable.",
        |id| Box::pin(async move { request_project_administration(&id).await }),
        adopt,
    );
    let dialog = RwSignal::new(None::<ProjectDialog>);
    let reason = RwSignal::new(String::new());
    let (member_principal, member_roles) = (
        RwSignal::new(String::new()),
        RwSignal::new(vec!["AGENT_DEVELOPER".to_string()]),
    );
    let open_dialog = move |next: ProjectDialog| {
        page.problem.set(String::new());
        reason.set(String::new());
        dialog.set(Some(next));
    };
    let close_when_accepted = Callback::new(move |accepted: bool| {
        if accepted {
            dialog.set(None);
        }
    });
    let _ = page.reload;

    move || {
        if !page.loaded.get() {
            return view! { <main class="administration"><p role="status">"Loading project settings…"</p></main> }.into_any();
        }
        let Some(data) = page.data.get() else {
            return view! { <main class="administration"><p role="status">"This project is unavailable."</p><p role="alert">{move || page.problem.get()}</p></main> }.into_any();
        };
        let can = |capability: &str| data.capabilities.iter().any(|code| code == capability);
        let active = data.lifecycle_status == "ACTIVE";
        let id = data.id.inner().to_string();
        let status = data.budget_status.clone();
        let can_view_mcp = console.context.with_untracked(|context| {
            context.capabilities.iter().any(|capability| {
                capability.code == "TOOL_CONNECTION.VIEW" && capability.scope_id.inner() == id
            })
        });
        let revision = data.revision;
        let (general_id, add_id, budget_id, policy_id, archive_id, restore_id, end_scope) = (
            id.clone(),
            id.clone(),
            id.clone(),
            id.clone(),
            id.clone(),
            id.clone(),
            id.clone(),
        );
        let budget_revision = data
            .budget_policy
            .as_ref()
            .map_or(0, |policy| policy.revision);
        let policy_currency = data
            .budget_policy
            .as_ref()
            .map(|policy| policy.currency.clone());
        view! {
            <main class="administration" aria-labelledby="project-administration-title">
                <PageHeader title_id="project-administration-title" title=format!("{} settings", data.display_name) meta=format!("ID {} · {} · Revision {}", data.slug, data.lifecycle_status, data.revision) />
                <a href=format!("/projects/{id}")>"Return to dashboard"</a>
                <p><a href=format!("/projects/{id}/audit?resourceType=PROJECT&resourceId={}", encode(&id))>"Review project settings audit history"</a></p>
                {move || { let text = page.problem.get(); (!text.is_empty() && dialog.with(Option::is_none)).then(|| view! { <p role="alert">{text}</p> }) }}

                <section><h2>"General"</h2>
                    <p>"Project ID: "{data.slug.clone()}" (read-only after creation)"</p><p>"Lifecycle: "{data.lifecycle_status.clone()}</p>
                    {if active && can("PROJECT.UPDATE") { view! {
                        <form on:submit=move |event: ev::SubmitEvent| { event.prevent_default(); page.mutate.run((Box::pin(update_project_general(UpdateProjectGeneralInput { project_id: general_id.as_str().into(),
                            expected_revision: revision, display_name: display_name.get_untracked(), description: description.get_untracked() })), Callback::new(IGNORE))); }>
                            <label>"Name"<input required prop:value=move || display_name.get() on:input=move |event| display_name.set(event_target_value(&event)) /></label>
                            <label>"Description"<textarea prop:value=move || description.get() on:input=move |event| description.set(event_target_value(&event)) /></label>
                            <button class="primary-action" type="submit">"Save general settings"</button></form>
                    }.into_any() } else { view! { <p>"Name: "{data.display_name.clone()}</p><p>"Description: "{if data.description.is_empty() { "No description".to_string() } else { data.description.clone() }}</p> }.into_any() }}
                </section>

                <section><h2>"Members"</h2>
                    {member_list(data.memberships.clone(), data.assignable_roles.clone(), "PROJECT", id.clone(), active && can("PROJECT_MEMBERSHIP.CHANGE_ROLES"), can("PROJECT_MEMBERSHIP.END"), false,
                        page.roles, page.mutate, Callback::new(move |(membership, revision): (String, i32)| open_dialog(ProjectDialog::End(membership, revision))))}
                    {(active && can("PROJECT_MEMBERSHIP.ADD")).then(|| view! { <AddMemberForm label="Add project member" principals=eligible(&data.available_principals, &data.memberships) roles=data.assignable_roles.clone()
                        principal_id=member_principal selected_roles=member_roles submit=Callback::new(move |()| page.mutate.run((Box::pin(add_membership(AdministrationMembershipInput { scope: "PROJECT".to_string(),
                            scope_id: add_id.as_str().into(), principal_id: member_principal.get_untracked().as_str().into(), role_codes: member_roles.get_untracked(), expected_scope_revision: revision })), Callback::new(IGNORE)))) /> })}
                </section>

                <section><h2>"Budgets"</h2>
                    <p role="status"><span role="img" aria-label=format!("Budget status {}", status.state)>{budget_icon(&status.state)}</span>" "{status.state.clone()}": "{money(status.amount_cents, status.currency.as_deref())}
                        {status.includes_estimates.then_some(" (includes estimates)")}{status.reason.clone().map(|reason| format!(" — {reason}"))}</p>
                    <p>"UTC month: "{status.period_start.clone().unwrap_or_else(|| "Unknown".to_string())}" to "{status.period_end.clone().unwrap_or_else(|| "Unknown".to_string())}"."</p>
                    {status.data_as_of.clone().map(|at| view! { <p>"Data as of: "{at}"."</p> })}
                    {status.last_successful_import_at.clone().map(|at| view! { <p>"Last successful import: "{at}"."</p> })}
                    {data.budget_policy.clone().map(|policy| view! { <p>"Policy revision "{policy.revision}": warning "{money(Some(policy.warning_threshold_cents), Some(&policy.currency))}"; limit "{money(Some(policy.monthly_limit_cents), Some(&policy.currency))}"."</p> })}
                    {(active && can("PROJECT_BUDGET.UPDATE")).then(|| view! {
                        <form on:submit=move |event: ev::SubmitEvent| { event.prevent_default(); page.mutate.run((Box::pin(update_budget_policy(UpdateProjectBudgetPolicyInput { project_id: budget_id.as_str().into(),
                            expected_revision: budget_revision, currency: currency.get_untracked(), monthly_limit_cents: limit.get_untracked().parse().unwrap_or(0), warning_threshold_cents: warning.get_untracked().parse().unwrap_or(0),
                            reason: budget_reason.get_untracked() })), Callback::new(IGNORE))); }>
                            <label>"Currency"<input required prop:value=move || currency.get() on:input=move |event| currency.set(event_target_value(&event).to_uppercase()) /></label>
                            {move || policy_currency.clone().filter(|saved| *saved != currency.get()).map(|_| view! { <p role="status">"Currency change causes UNKNOWN CURRENCY_MISMATCH until a complete local import uses "{currency.get()}"."</p> })}
                            <label>"Monthly limit"<input required type="number" min="1" prop:value=move || limit.get() on:input=move |event| limit.set(event_target_value(&event)) /></label>
                            <label>"Warning threshold"<input required type="number" min="1" prop:value=move || warning.get() on:input=move |event| warning.set(event_target_value(&event)) /></label>
                            <label>"Reason"<input required prop:value=move || budget_reason.get() on:input=move |event| budget_reason.set(event_target_value(&event)) /></label>
                            <button class="primary-action" type="submit">"Save budget policy"</button></form> })}
                    {(!data.budget_history.is_empty()).then(|| view! { <p>"Policy history: "{data.budget_history.iter().map(|entry| format!("r{} ({})", entry.revision, entry.change_reason)).collect::<Vec<_>>().join("; ")}</p> })}
                </section>

                <section><h2>"Approval policies"</h2>
                    {data.approval_policy.clone().map(|policy| { let (policy_revision, policy_id) = (policy.revision, policy_id.clone()); view! {
                        <p>"Fixed local P-05 policy revision "{policy.revision}"; digest "{policy.digest.clone()}"."</p>
                        {(active && can("PROJECT_APPROVAL_POLICY.UPDATE")).then(|| view! {
                            <form on:submit=move |event: ev::SubmitEvent| { event.prevent_default();
                                let Ok(cells) = <[ApprovalPolicyCellInput; 9]>::try_from(matrix.get_untracked()) else { return };
                                page.mutate.run((Box::pin(update_approval_policy(UpdateProjectApprovalPolicyInput { project_id: policy_id.as_str().into(), expected_revision: policy_revision,
                                    matrix: FixedApprovalPolicyMatrixInput::from_cells(cells), reason: reason.get_untracked() })), Callback::new(IGNORE))); }>
                                {NINE_CELLS.iter().enumerate().map(|(index, cell)| view! {
                                    <fieldset class="approval-policy-cell"><legend>{*cell}</legend>
                                        <label>"Required approvers"<input aria-label=format!("{cell} approvers") type="number" min="0" max="2" prop:value=move || matrix.with(|all| all[index].required_approvers.to_string())
                                            on:input=move |event| matrix.update(|all| all[index].required_approvers = event_target_value(&event).parse().unwrap_or(0)) /></label>
                                        <fieldset class="role-selector"><legend>"Required evidence"</legend>
                                            {EVIDENCE_OPTIONS.iter().map(|evidence| view! {
                                                <label><input type="checkbox" prop:checked=move || matrix.with(|all| all[index].required_evidence.iter().any(|entry| entry == evidence))
                                                    on:change=move |event| matrix.update(|all| { let list = &mut all[index].required_evidence; list.retain(|entry| entry != evidence); if event_target_checked(&event) { list.push(evidence.to_string()); list.sort(); } }) />
                                                    <span>{role_label(evidence)}</span></label> }).collect_view()}
                                        </fieldset></fieldset> }).collect_view()}
                                <label>"Change reason"<input required prop:value=move || reason.get() on:input=move |event| reason.set(event_target_value(&event)) /></label>
                                <button class="primary-action" type="submit" disabled=move || reason.with(|value| value.trim().is_empty())>"Save stronger policy"</button></form> })}
                        <p>"Policy history: "{policy.history.iter().map(|entry| format!("r{} {} ({})", entry.revision, entry.digest, entry.change_reason)).collect::<Vec<_>>().join("; ")}</p>
                    } })}
                </section>

                <section><h2>"MCP servers"</h2>
                    <p>"MCP server configuration has one project boundary with explicit create and edit modes. Secrets and external execution are not available."</p>
                    {if can_view_mcp { view! { <a href=format!("/projects/{id}/tools")>"Open MCP servers"</a> }.into_any() } else { view! { <p role="status">"MCP server configuration is unavailable with your current capabilities."</p> }.into_any() }}
                </section>

                <section><h2>"Danger zone"</h2>
                    {(active && can("PROJECT.ARCHIVE")).then(|| view! { <button class="danger-action" type="button" on:click=move |_| open_dialog(ProjectDialog::Archive)>"Archive project"</button> })}
                    {(!active && can("PROJECT.RESTORE")).then(|| view! { <button type="button" on:click=move |_| page.mutate.run((Box::pin(restore_scope(LifecycleAdministrationInput { scope: "PROJECT".to_string(),
                        scope_id: restore_id.as_str().into(), expected_revision: revision, reason: None, confirmation: None })), Callback::new(IGNORE)))>"Restore project"</button> })}
                </section>

                {move || match dialog.get() {
                    None => None,
                    Some(ProjectDialog::Archive) => { let scope_id = archive_id.clone(); Some(view! {
                        <ConfirmationDialog title="Archive project" on_close=Callback::new(move |()| dialog.set(None))>
                            <p>"Data and history remain; project writes become read-only except later safety-reducing actions. Runtime work is not stopped."</p>
                            <label>"Reason"<input aria-describedby="project-archive-error" prop:value=move || reason.get() on:input=move |event| reason.set(event_target_value(&event)) /></label>
                            {move || { let text = page.problem.get(); (!text.is_empty()).then(|| view! { <p id="project-archive-error" role="alert">{text}</p> }) }}
                            <button class="danger-action" disabled=move || reason.with(|value| value.trim().is_empty())
                                on:click=move |_| page.mutate.run((Box::pin(archive_scope(LifecycleAdministrationInput { scope: "PROJECT".to_string(), scope_id: scope_id.as_str().into(), expected_revision: revision,
                                    reason: Some(reason.get_untracked()), confirmation: None })), close_when_accepted))>"Archive project"</button>
                            <button on:click=move |_| dialog.set(None)>"Cancel"</button>
                        </ConfirmationDialog> }.into_any()) }
                    Some(ProjectDialog::End(membership, membership_revision)) => { let scope_id = end_scope.clone(); Some(view! {
                        <ConfirmationDialog title="End project membership" on_close=Callback::new(move |()| dialog.set(None))>
                            <p>"Access ends immediately; audit history and possible approval impact remain."</p>
                            <label>"Reason"<input aria-describedby="project-end-error" prop:value=move || reason.get() on:input=move |event| reason.set(event_target_value(&event)) /></label>
                            {move || { let text = page.problem.get(); (!text.is_empty()).then(|| view! { <p id="project-end-error" role="alert">{text}</p> }) }}
                            <button class="danger-action" disabled=move || reason.with(|value| value.trim().is_empty())
                                on:click=move |_| page.mutate.run((Box::pin(end_membership(EndAdministrationMembershipInput { scope: "PROJECT".to_string(), scope_id: scope_id.as_str().into(),
                                    membership_id: membership.as_str().into(), expected_revision: membership_revision, reason: reason.get_untracked() })), close_when_accepted))>"End membership"</button>
                            <button on:click=move |_| dialog.set(None)>"Cancel"</button>
                        </ConfirmationDialog> }.into_any()) }
                }}
            </main>
        }.into_any()
    }
}
