//! Ports `CreateProjectPage.tsx`.

use crate::api::administration::{
    create_project, request_organization_administration, CreateProjectInput,
    OrganizationAdministrationFields,
};
use crate::graphql::GraphqlError;
use crate::page_header::PageHeader;
use crate::shell::use_console;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_params_map;

#[component]
pub fn CreateProjectPage() -> impl IntoView {
    let revision = use_console().revision;
    let params = use_params_map();
    let organization_id =
        Memo::new(move |_| params.read().get("organization_id").unwrap_or_default());
    // `None` while loading; `Some(None)` when the organization is unavailable.
    let organization = RwSignal::new(None::<Option<OrganizationAdministrationFields>>);
    let (slug, display_name, description) = (
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
    );
    let message = RwSignal::new(None::<&'static str>);
    let problem = RwSignal::new(None::<String>);
    let saving = RwSignal::new(false);
    let generation = StoredValue::new(0_u32);

    Effect::new(move |_| {
        let id = organization_id.get();
        let _ = revision.get();
        let request = generation.get_value() + 1;
        generation.set_value(request);
        organization.set(None);
        spawn_local(async move {
            let result = request_organization_administration(&id).await;
            if generation.try_get_value() == Some(request) {
                organization.set(Some(result.ok().flatten()));
            }
        });
    });

    let create = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let Some(Some(current)) = organization.get_untracked() else {
            return;
        };
        if saving.get_untracked() {
            return;
        }
        saving.set(true);
        message.set(None);
        problem.set(None);
        let input = CreateProjectInput {
            organization_id: current.id.clone(),
            expected_revision: current.revision,
            slug: slug.get_untracked(),
            display_name: display_name.get_untracked(),
            description: Some(description.get_untracked()),
        };
        spawn_local(async move {
            match create_project(input).await {
                Ok(result) => {
                    if let Some(first) = result.problems.into_iter().next() {
                        problem.set(Some(first.message));
                    }
                    // A full navigation makes the shell fetch capabilities afresh; client-side routing
                    // would race its cached list, which does not know this project yet.
                    else if let Some(project) = result.project {
                        let _ = window()
                            .location()
                            .assign(&format!("/projects/{}", project.id.inner()));
                    }
                }
                Err(GraphqlError::SessionExpired) => message.set(Some(
                    "Your session has expired. Sign in again to create a project.",
                )),
                Err(GraphqlError::Transport(_)) => message.set(Some(
                    "We could not create this project. No project was created.",
                )),
            }
            saving.set(false);
        });
    };

    move || {
        match organization.get() {
        None => view! { <main class="administration"><p role="status">"Loading organization…"</p></main> }.into_any(),
        Some(None) => view! { <main class="administration"><p role="alert">"This organization is unavailable."</p></main> }.into_any(),
        Some(Some(current)) if current.lifecycle_status != "ACTIVE" || !current.capabilities.iter().any(|code| code == "PROJECT.CREATE") => view! {
            <main class="administration" aria-labelledby="create-project-title">
                <PageHeader title_id="create-project-title" title="Create project".to_string() />
                <p role="alert">{if current.lifecycle_status != "ACTIVE" { "This organization is archived. Restore it before creating new projects." }
                    else { "You do not have permission to create a project in this organization." }}</p>
            </main>
        }.into_any(),
        Some(Some(_)) => view! {
            <main class="administration" aria-labelledby="create-project-title">
                <PageHeader title_id="create-project-title" title="Create project".to_string()
                    description="Creation takes effect immediately and adds the project to this organization." />
                {move || problem.get().or_else(|| message.get().map(str::to_string)).map(|text| view! { <p role="alert">{text}</p> })}
                <section><form on:submit=create>
                    <label>"Project ID"<input required prop:value=move || slug.get() on:input=move |event| slug.set(event_target_value(&event)) /></label>
                    <label>"Name"<input required prop:value=move || display_name.get() on:input=move |event| display_name.set(event_target_value(&event)) /></label>
                    <label>"Description"<textarea prop:value=move || description.get() on:input=move |event| description.set(event_target_value(&event)) /></label>
                    <button class="primary-action" type="submit" disabled=move || saving.get()>{move || if saving.get() { "Creating project…" } else { "Create project" }}</button>
                </form></section>
            </main>
        }.into_any(),
    }
    }
}
