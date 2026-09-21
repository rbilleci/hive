//! The Hive console: a client-side rendered Leptos application.

mod agent_tabs;
mod api;
mod code_editor;
mod confirmation_dialog;
mod dom;
mod draft_editor;
mod graphql;
mod json_viewer;
mod navigation;
mod navigation_guard;
mod page_header;
mod pages;
mod shell;

use leptos::prelude::*;
use leptos_router::components::{ParentRoute, Route, Router, Routes};
use leptos_router::path;
use pages::organizations::{OrganizationDirectory, OrganizationOverviewPage};
use pages::preferences::PreferencesPage;
use wasm_bindgen::JsCast;

/// The route table of `main.tsx`. Static segments precede their `:param` siblings: the router
/// takes the first match.
#[component]
fn App() -> impl IntoView {
    view! {
        <Router>
            <Routes fallback=|| view! { <p role="status">"Loading page…"</p> }>
                <Route path=path!("/session-error") view=shell::SessionErrorPage />
                <Route path=path!("/access-denied") view=shell::AccessDeniedPage />
                <ParentRoute path=path!("") view=shell::ConsoleShell>
                    <Route path=path!("/") view=shell::ConsoleHome />
                    <Route path=path!("/organizations") view=OrganizationDirectory />
                    <Route path=path!("/organizations/:organization_id") view=OrganizationOverviewPage />
                    <Route path=path!("/projects/:project_id/agents/:agent_id/edit") view=draft_editor::AgentDraftEditor />
                    <Route path=path!("/preferences") view=PreferencesPage />
                    <Route path=path!("/projects/:project_id") view=pages::dashboard::ProjectDashboardPage />
                    <Route path=path!("/organizations/:organization_id/projects") view=pages::directory::OrganizationProjectDirectory />
                    <Route path=path!("/projects/:project_id/agents") view=pages::directory::ProjectAgentDirectory />
                    <Route path=path!("/organizations/:organization_id/projects/new") view=pages::create_project::CreateProjectPage />
                    // Not ported yet.
                    <Route path=path!("/organizations/:organization_id/settings") view=pages::administration::OrganizationAdministrationPage />
                    <Route path=path!("/organizations/:organization_id/catalog") view=pages::configuration::CatalogPage />
                    <Route path=path!("/organizations/:organization_id/environments") view=pages::configuration::EnvironmentsPage />
                    <Route path=path!("/organizations/:organization_id/approvals") view=pages::approval::OrganizationApprovalInboxPage />
                    <Route path=path!("/organizations/:organization_id/audit") view=pages::audit::OrganizationAuditPage />
                    <Route path=path!("/projects/:project_id/agents/new") view=pages::agent_versions::CreateAgentDraftPage />
                    <Route path=path!("/projects/:project_id/settings") view=pages::administration::ProjectAdministrationPage />
                    <Route path=path!("/projects/:project_id/prompts") view=pages::configuration::PromptLibraryPage />
                    <Route path=path!("/projects/:project_id/prompts/new") view=pages::configuration::PromptEditorPage />
                    <Route path=path!("/projects/:project_id/prompts/:resource_id/edit") view=pages::configuration::PromptEditorPage />
                    <Route path=path!("/projects/:project_id/policies") view=pages::configuration::PoliciesPage />
                    <Route path=path!("/projects/:project_id/model-profiles") view=pages::configuration::ModelProfilesPage />
                    <Route path=path!("/projects/:project_id/tools") view=pages::mcp_servers::McpServersPage />
                    <Route path=path!("/projects/:project_id/agents/:agent_id/deployments") view=pages::deployment::AgentDeploymentsPage />
                    <Route path=path!("/projects/:project_id/agents/:agent_id/evaluations") view=pages::evaluation::AgentEvaluationsPage />
                    <Route path=path!("/projects/:project_id/agents/:agent_id/versions") view=pages::agent_versions::AgentVersionsPage />
                    <Route path=path!("/projects/:project_id/agents/:agent_id/versions/compare") view=pages::agent_versions::AgentVersionComparisonPage />
                    <Route path=path!("/projects/:project_id/agents/:agent_id/versions/:version_id/deploy") view=pages::deployment::DeploymentRequestPage />
                    <Route path=path!("/projects/:project_id/agents/:agent_id/versions/:version_id") view=pages::agent_versions::AgentVersionDetailPage />
                    <Route path=path!("/projects/:project_id/deployments") view=pages::deployment::DeploymentsPage />
                    <Route path=path!("/projects/:project_id/deployments/:deployment_id") view=pages::deployment::DeploymentDetailPage />
                    <Route path=path!("/projects/:project_id/deployments/:deployment_id/approvals/:approval_requirement_id") view=pages::approval::ApprovalDetailPage />
                    <Route path=path!("/projects/:project_id/evaluations") view=pages::evaluation::EvaluationListPage />
                    <Route path=path!("/projects/:project_id/evaluations/definitions/:definition_id/review") view=pages::evaluation::EvaluationPublicationReviewPage />
                    <Route path=path!("/projects/:project_id/evaluations/definitions/:definition_id") view=pages::evaluation::EvaluationDefinitionPage />
                    <Route path=path!("/projects/:project_id/evaluations/definitions/:definition_id/versions") view=pages::evaluation::EvaluationVersionHistoryPage />
                    <Route path=path!("/projects/:project_id/evaluations/definitions/:definition_id/versions/compare") view=pages::evaluation::EvaluationVersionComparisonPage />
                    <Route path=path!("/projects/:project_id/evaluations/definitions/:definition_id/versions/:version_id/usage") view=pages::evaluation::EvaluationVersionUsagePage />
                    <Route path=path!("/projects/:project_id/evaluations/definitions/:definition_id/versions/:version_id") view=pages::evaluation::EvaluationVersionDetailPage />
                    <Route path=path!("/projects/:project_id/evaluations/runs/:run_id") view=pages::evaluation::EvaluationRunPage />
                    <Route path=path!("/projects/:project_id/audit") view=pages::audit::ProjectAuditPage />
                    <Route path=path!("/approvals") view=pages::approval::ApprovalInboxPage />
                    // After every `/agents/<static segment>` route: the router takes the first match.
                    <Route path=path!("/projects/:project_id/agents/:agent_id") view=pages::agent_overview::AgentOperationalOverview />
                    <Route path=path!("/*any") view=shell::NotFoundPage />
                </ParentRoute>
            </Routes>
        </Router>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    navigation_guard::install();
    let root = document()
        .get_element_by_id("root")
        .expect("index.html declares #root")
        .unchecked_into::<web_sys::HtmlElement>();
    leptos::mount::mount_to(root, App).forget();
}
