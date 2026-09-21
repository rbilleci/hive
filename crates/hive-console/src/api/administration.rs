//! Organization and project administration, read through the generated API, and the nine
//! administration commands. cynic names a GraphQL operation after its root
//! struct, and the end-to-end checks intercept requests by that name, so renaming a root struct
//! here breaks those checks.
//!
//! The scope row is read through `organizations` / `projects`, so a scope the principal cannot
//! see answers with no node ("unavailable"). Memberships, the budget policy and the approval
//! policy are relations of that row; the server returns them only to a holder of the matching
//! view capability, so a plain member reads the scope with empty lists.

use crate::api::generated::{
    is_uuid, OrganizationsFilterInput, ProjectsFilterInput, TextFilterInput,
};
use crate::graphql::{execute, schema, GraphqlError};
use cynic::{MutationBuilder, QueryBuilder};

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Principals")]
pub struct AdministrationPrincipal {
    pub id: String,
    pub display_name: String,
    pub email: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Principals")]
pub struct MemberPrincipal {
    pub display_name: String,
    pub email: Option<String>,
    pub last_seen_at: Option<String>,
}

/// One membership as both pages list it, from either scope's generated row.
#[derive(Debug, Clone, PartialEq)]
pub struct AdministrationMembership {
    pub id: String,
    pub principal_id: String,
    pub display_name: String,
    pub email: String,
    pub role_codes: Vec<String>,
    pub project_access_summary: Vec<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub revision: i32,
}

/// Active memberships first; the server already orders by start, newest first.
fn active_first(mut memberships: Vec<AdministrationMembership>) -> Vec<AdministrationMembership> {
    memberships.sort_by_key(|membership| membership.ended_at.is_some());
    memberships
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "OrganizationMemberships")]
pub struct OrganizationMembershipRow {
    pub id: String,
    pub principal_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub revision: Option<i32>,
    pub role_codes: Vec<String>,
    pub project_access_summary: Vec<String>,
    pub principals: Option<MemberPrincipal>,
}

impl From<OrganizationMembershipRow> for AdministrationMembership {
    fn from(row: OrganizationMembershipRow) -> Self {
        let principal = row.principals;
        Self {
            id: row.id,
            principal_id: row.principal_id,
            display_name: principal
                .as_ref()
                .map(|principal| principal.display_name.clone())
                .unwrap_or_default(),
            email: principal
                .as_ref()
                .and_then(|principal| principal.email.clone())
                .unwrap_or_default(),
            role_codes: row.role_codes,
            project_access_summary: row.project_access_summary,
            started_at: row.started_at,
            ended_at: row.ended_at,
            last_seen_at: principal.and_then(|principal| principal.last_seen_at),
            revision: row.revision.unwrap_or_default(),
        }
    }
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "OrganizationMembershipsConnection")]
pub struct OrganizationMembershipRows {
    pub nodes: Vec<OrganizationMembershipRow>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectMemberships")]
pub struct ProjectMembershipRow {
    pub id: String,
    pub principal_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub revision: i32,
    pub role_codes: Vec<String>,
    pub principals: Option<MemberPrincipal>,
}

impl From<ProjectMembershipRow> for AdministrationMembership {
    fn from(row: ProjectMembershipRow) -> Self {
        let principal = row.principals;
        Self {
            id: row.id,
            principal_id: row.principal_id,
            display_name: principal
                .as_ref()
                .map(|principal| principal.display_name.clone())
                .unwrap_or_default(),
            email: principal
                .as_ref()
                .and_then(|principal| principal.email.clone())
                .unwrap_or_default(),
            role_codes: row.role_codes,
            project_access_summary: Vec::new(),
            started_at: row.started_at,
            ended_at: row.ended_at,
            last_seen_at: principal.and_then(|principal| principal.last_seen_at),
            revision: row.revision,
        }
    }
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectMembershipsConnection")]
pub struct ProjectMembershipRows {
    pub nodes: Vec<ProjectMembershipRow>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Organizations")]
pub struct OrganizationRow {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub revision: Option<i32>,
    pub capabilities: Vec<String>,
    pub assignable_roles: Vec<String>,
    pub available_principals: Vec<AdministrationPrincipal>,
    #[arguments(orderBy: { startedAt: DESC, id: ASC })]
    pub organization_memberships: OrganizationMembershipRows,
}

/// What the organization administration page renders.
#[derive(Debug, Clone, PartialEq)]
pub struct OrganizationAdministrationFields {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub revision: i32,
    pub capabilities: Vec<String>,
    pub memberships: Vec<AdministrationMembership>,
    pub available_principals: Vec<AdministrationPrincipal>,
    pub assignable_roles: Vec<String>,
}

impl From<OrganizationRow> for OrganizationAdministrationFields {
    fn from(row: OrganizationRow) -> Self {
        Self {
            id: row.id,
            slug: row.slug,
            display_name: row.display_name,
            lifecycle_status: row.lifecycle_status,
            revision: row.revision.unwrap_or_default(),
            capabilities: row.capabilities,
            memberships: active_first(
                row.organization_memberships
                    .nodes
                    .into_iter()
                    .map(AdministrationMembership::from)
                    .collect(),
            ),
            available_principals: row.available_principals,
            assignable_roles: row.assignable_roles,
        }
    }
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ApprovalPolicyRule {
    pub cell: String,
    pub required_evidence: Vec<String>,
    pub required_approvers: i32,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "ProjectBudgetPolicyVersions")]
pub struct ProjectBudgetPolicy {
    pub revision: i32,
    pub currency: String,
    pub monthly_limit_cents: i32,
    pub warning_threshold_cents: i32,
    pub change_reason: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ProjectBudgetStatus {
    pub state: String,
    pub reason: Option<String>,
    pub amount_cents: Option<i32>,
    pub includes_estimates: bool,
    pub currency: Option<String>,
    pub period_start: Option<String>,
    pub period_end: Option<String>,
    pub data_as_of: Option<String>,
    pub last_successful_import_at: Option<String>,
}

impl ProjectBudgetStatus {
    /// A project with no budget policy row.
    fn not_configured() -> Self {
        Self {
            state: "NOT_CONFIGURED".to_string(),
            reason: Some("NO_POLICY".to_string()),
            amount_cents: None,
            includes_estimates: false,
            currency: None,
            period_start: None,
            period_end: None,
            data_as_of: None,
            last_successful_import_at: None,
        }
    }
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectBudgetPolicyVersionsConnection")]
pub struct ProjectBudgetPolicyHistory {
    pub nodes: Vec<ProjectBudgetPolicy>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectBudgetPolicies")]
pub struct ProjectBudgetPolicyRow {
    pub current_version: Option<ProjectBudgetPolicy>,
    pub status: ProjectBudgetStatus,
    #[arguments(orderBy: { revision: DESC })]
    pub project_budget_policy_versions: ProjectBudgetPolicyHistory,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "ProjectApprovalPolicyVersions")]
pub struct ProjectApprovalPolicyVersion {
    pub revision: i32,
    pub digest: String,
    pub change_reason: String,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectApprovalPolicyVersions")]
pub struct CurrentApprovalPolicyVersion {
    pub revision: i32,
    pub digest: String,
    pub rules: Vec<ApprovalPolicyRule>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectApprovalPolicyVersionsConnection")]
pub struct ProjectApprovalPolicyHistory {
    pub nodes: Vec<ProjectApprovalPolicyVersion>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectApprovalPolicies")]
pub struct ProjectApprovalPolicyRow {
    pub current_version: Option<CurrentApprovalPolicyVersion>,
    #[arguments(orderBy: { revision: DESC })]
    pub project_approval_policy_versions: ProjectApprovalPolicyHistory,
}

/// The approval policy in force, with its history, newest first.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectApprovalPolicy {
    pub revision: i32,
    pub digest: String,
    pub matrix: Vec<ApprovalPolicyRule>,
    pub history: Vec<ProjectApprovalPolicyVersion>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Projects")]
pub struct ProjectRow {
    pub id: String,
    pub organization_id: String,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub lifecycle_status: String,
    pub revision: Option<i32>,
    pub capabilities: Vec<String>,
    pub assignable_roles: Vec<String>,
    pub available_principals: Vec<AdministrationPrincipal>,
    #[arguments(orderBy: { startedAt: DESC, id: ASC })]
    pub project_memberships: ProjectMembershipRows,
    pub project_budget_policies: Option<ProjectBudgetPolicyRow>,
    pub project_approval_policies: Option<ProjectApprovalPolicyRow>,
}

/// What the project settings page renders.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectAdministrationFields {
    pub id: String,
    pub organization_id: String,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub lifecycle_status: String,
    pub revision: i32,
    pub capabilities: Vec<String>,
    pub memberships: Vec<AdministrationMembership>,
    pub available_principals: Vec<AdministrationPrincipal>,
    pub assignable_roles: Vec<String>,
    pub budget_policy: Option<ProjectBudgetPolicy>,
    pub budget_history: Vec<ProjectBudgetPolicy>,
    pub budget_status: ProjectBudgetStatus,
    pub approval_policy: Option<ProjectApprovalPolicy>,
}

impl From<ProjectRow> for ProjectAdministrationFields {
    fn from(row: ProjectRow) -> Self {
        let (budget_policy, budget_history, budget_status) = match row.project_budget_policies {
            Some(policy) => (
                policy.current_version,
                policy.project_budget_policy_versions.nodes,
                policy.status,
            ),
            None => (None, Vec::new(), ProjectBudgetStatus::not_configured()),
        };
        let approval_policy = row.project_approval_policies.and_then(|policy| {
            let history = policy.project_approval_policy_versions.nodes;
            policy.current_version.map(|version| ProjectApprovalPolicy {
                revision: version.revision,
                digest: version.digest,
                matrix: version.rules,
                history,
            })
        });
        Self {
            id: row.id,
            organization_id: row.organization_id,
            slug: row.slug,
            display_name: row.display_name,
            description: row.description.unwrap_or_default(),
            lifecycle_status: row.lifecycle_status,
            revision: row.revision.unwrap_or_default(),
            capabilities: row.capabilities,
            memberships: active_first(
                row.project_memberships
                    .nodes
                    .into_iter()
                    .map(AdministrationMembership::from)
                    .collect(),
            ),
            available_principals: row.available_principals,
            assignable_roles: row.assignable_roles,
            budget_policy,
            budget_history,
            budget_status,
            approval_policy,
        }
    }
}

#[derive(cynic::QueryVariables, Debug)]
pub struct OrganizationAdministrationVariables {
    pub organization: OrganizationsFilterInput,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "OrganizationsConnection")]
pub struct OrganizationRows {
    pub nodes: Vec<OrganizationRow>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    graphql_type = "Query",
    variables = "OrganizationAdministrationVariables"
)]
pub struct OrganizationAdministration {
    /// Empty when the organization is not visible to the principal.
    #[arguments(filters: $organization)]
    pub organizations: OrganizationRows,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct ProjectAdministrationVariables {
    pub project: ProjectsFilterInput,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "ProjectsConnection")]
pub struct ProjectRows {
    pub nodes: Vec<ProjectRow>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "ProjectAdministrationVariables")]
pub struct ProjectAdministration {
    /// Empty when the project is not visible to the principal.
    #[arguments(filters: $project)]
    pub projects: ProjectRows,
}

/// `Ok(None)` is "unavailable".
pub async fn request_organization_administration(
    id: &str,
) -> Result<Option<OrganizationAdministrationFields>, GraphqlError> {
    if !is_uuid(id) {
        return Ok(None);
    }
    let organizations = execute(OrganizationAdministration::build(
        OrganizationAdministrationVariables {
            organization: OrganizationsFilterInput {
                id: Some(TextFilterInput::eq(id)),
                ..Default::default()
            },
        },
    ))
    .await?
    .organizations;
    Ok(organizations
        .nodes
        .into_iter()
        .next()
        .map(OrganizationAdministrationFields::from))
}

/// `Ok(None)` is "unavailable".
pub async fn request_project_administration(
    id: &str,
) -> Result<Option<ProjectAdministrationFields>, GraphqlError> {
    if !is_uuid(id) {
        return Ok(None);
    }
    let projects = execute(ProjectAdministration::build(
        ProjectAdministrationVariables {
            project: ProjectsFilterInput {
                id: Some(TextFilterInput::eq(id)),
                ..Default::default()
            },
        },
    ))
    .await?
    .projects;
    Ok(projects
        .nodes
        .into_iter()
        .next()
        .map(ProjectAdministrationFields::from))
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Problem")]
pub struct AdministrationProblemFields {
    pub message: String,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Projects")]
pub struct ProjectReference {
    pub id: String,
}

/// Every administration mutation's payload. The pages reload after a mutation, so only the
/// refusals and the created project's identifier are read from it.
#[derive(cynic::QueryFragment, Debug)]
pub struct AdministrationMutationPayload {
    pub project: Option<ProjectReference>,
    pub problems: Vec<AdministrationProblemFields>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct CreateProjectInput {
    pub organization_id: cynic::Id,
    pub expected_revision: i32,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct AdministrationMembershipInput {
    pub scope: String,
    pub scope_id: cynic::Id,
    pub principal_id: cynic::Id,
    pub role_codes: Vec<String>,
    pub expected_scope_revision: i32,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct ReplaceAdministrationMembershipInput {
    pub scope: String,
    pub scope_id: cynic::Id,
    pub membership_id: cynic::Id,
    pub role_codes: Vec<String>,
    pub expected_revision: i32,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct EndAdministrationMembershipInput {
    pub scope: String,
    pub scope_id: cynic::Id,
    pub membership_id: cynic::Id,
    pub expected_revision: i32,
    pub reason: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct LifecycleAdministrationInput {
    pub scope: String,
    pub scope_id: cynic::Id,
    pub expected_revision: i32,
    pub reason: Option<String>,
    pub confirmation: Option<String>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdateProjectGeneralInput {
    pub project_id: cynic::Id,
    pub expected_revision: i32,
    pub display_name: String,
    pub description: String,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdateProjectBudgetPolicyInput {
    pub project_id: cynic::Id,
    pub expected_revision: i32,
    pub currency: String,
    pub monthly_limit_cents: i32,
    pub warning_threshold_cents: i32,
    pub reason: String,
}

/// One cell of the approval policy matrix. `updateProjectApprovalPolicy` takes all nine.
#[derive(cynic::InputObject, Debug, Clone, PartialEq)]
pub struct ApprovalPolicyRuleInput {
    pub cell: String,
    pub required_evidence: Vec<String>,
    pub required_approvers: i32,
}

pub const NINE_CELLS: [&str; 9] = [
    "DEVELOPMENT_LOW",
    "DEVELOPMENT_MEDIUM",
    "DEVELOPMENT_HIGH",
    "STAGING_LOW",
    "STAGING_MEDIUM",
    "STAGING_HIGH",
    "PRODUCTION_LOW",
    "PRODUCTION_MEDIUM",
    "PRODUCTION_HIGH",
];

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdateProjectApprovalPolicyInput {
    pub project_id: cynic::Id,
    pub expected_revision: i32,
    pub matrix: Vec<ApprovalPolicyRuleInput>,
    pub reason: String,
}

/// One mutation root per operation, each returning the shared payload under its own field. `$d` is
/// a literal `$`: cynic spells a variable `$input`, which a macro would otherwise read as its own.
macro_rules! administration_mutation {
    ($d:tt, $root:ident, $variables:ident, $variables_name:literal, $input_type:ty, $field:ident, $call:ident) => {
        #[derive(cynic::QueryVariables, Debug)]
        pub struct $variables {
            pub input: $input_type,
        }

        #[derive(cynic::QueryFragment, Debug)]
        #[cynic(graphql_type = "Mutation", variables = $variables_name)]
        pub struct $root {
            #[arguments(input: $d input)]
            pub $field: AdministrationMutationPayload,
        }

        pub async fn $call(
            input: $input_type,
        ) -> Result<AdministrationMutationPayload, GraphqlError> {
            Ok(execute($root::build($variables { input })).await?.$field)
        }
    };
}

administration_mutation!($, CreateProject, CreateProjectVariables, "CreateProjectVariables", CreateProjectInput, create_project, create_project);
administration_mutation!($, AddAdministrationMembership, AddAdministrationMembershipVariables, "AddAdministrationMembershipVariables", AdministrationMembershipInput, add_administration_membership, add_membership);
administration_mutation!($, ReplaceAdministrationMembershipRoles, ReplaceAdministrationMembershipRolesVariables, "ReplaceAdministrationMembershipRolesVariables", ReplaceAdministrationMembershipInput, replace_administration_membership_roles, replace_membership_roles);
administration_mutation!($, EndAdministrationMembership, EndAdministrationMembershipVariables, "EndAdministrationMembershipVariables", EndAdministrationMembershipInput, end_administration_membership, end_membership);
administration_mutation!($, ArchiveAdministrationScope, ArchiveAdministrationScopeVariables, "ArchiveAdministrationScopeVariables", LifecycleAdministrationInput, archive_administration_scope, archive_scope);
administration_mutation!($, RestoreAdministrationScope, RestoreAdministrationScopeVariables, "RestoreAdministrationScopeVariables", LifecycleAdministrationInput, restore_administration_scope, restore_scope);
administration_mutation!($, UpdateProjectGeneral, UpdateProjectGeneralVariables, "UpdateProjectGeneralVariables", UpdateProjectGeneralInput, update_project_general, update_project_general);
administration_mutation!($, UpdateProjectBudgetPolicy, UpdateProjectBudgetPolicyVariables, "UpdateProjectBudgetPolicyVariables", UpdateProjectBudgetPolicyInput, update_project_budget_policy, update_budget_policy);
administration_mutation!($, UpdateProjectApprovalPolicy, UpdateProjectApprovalPolicyVariables, "UpdateProjectApprovalPolicyVariables", UpdateProjectApprovalPolicyInput, update_project_approval_policy, update_approval_policy);
