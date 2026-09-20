//! Ports `administration.graphql` and `administrationApi.ts`.

use crate::graphql::{execute, schema, GraphqlError};
use cynic::{MutationBuilder, QueryBuilder};

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AdministrationMembership {
    pub id: cynic::Id,
    pub principal_id: cynic::Id,
    pub display_name: String,
    pub email: String,
    pub role_codes: Vec<String>,
    pub project_access_summary: Vec<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub revision: i32,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct AdministrationPrincipal {
    pub id: cynic::Id,
    pub display_name: String,
    pub email: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "OrganizationAdministration")]
pub struct OrganizationAdministrationFields {
    pub id: cynic::Id,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub revision: i32,
    pub capabilities: Vec<String>,
    pub memberships: Vec<AdministrationMembership>,
    pub available_principals: Vec<AdministrationPrincipal>,
    pub assignable_roles: Vec<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ApprovalPolicyRule {
    pub cell: String,
    pub required_evidence: Vec<String>,
    pub required_approvers: i32,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
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

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ProjectApprovalPolicyVersion {
    pub revision: i32,
    pub digest: String,
    pub change_reason: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
pub struct ProjectApprovalPolicy {
    pub revision: i32,
    pub digest: String,
    pub matrix: Vec<ApprovalPolicyRule>,
    pub history: Vec<ProjectApprovalPolicyVersion>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "ProjectAdministration")]
pub struct ProjectAdministrationFields {
    pub id: cynic::Id,
    pub organization_id: cynic::Id,
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

#[derive(cynic::QueryVariables, Debug)]
pub struct AdministrationVariables {
    pub id: cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AdministrationVariables")]
pub struct OrganizationAdministration {
    #[arguments(id: $id)]
    pub organization_administration: Option<OrganizationAdministrationFields>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "AdministrationVariables")]
pub struct ProjectAdministration {
    #[arguments(id: $id)]
    pub project_administration: Option<ProjectAdministrationFields>,
}

pub async fn request_organization_administration(
    id: &str,
) -> Result<Option<OrganizationAdministrationFields>, GraphqlError> {
    Ok(
        execute(OrganizationAdministration::build(AdministrationVariables {
            id: id.into(),
        }))
        .await?
        .organization_administration,
    )
}

pub async fn request_project_administration(
    id: &str,
) -> Result<Option<ProjectAdministrationFields>, GraphqlError> {
    Ok(
        execute(ProjectAdministration::build(AdministrationVariables {
            id: id.into(),
        }))
        .await?
        .project_administration,
    )
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "AdministrationProblem")]
pub struct AdministrationProblemFields {
    pub message: String,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "ProjectAdministration")]
pub struct ProjectReference {
    pub id: cynic::Id,
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

#[derive(cynic::InputObject, Debug, Clone, PartialEq)]
pub struct ApprovalPolicyCellInput {
    pub required_evidence: Vec<String>,
    pub required_approvers: i32,
}

/// The nine fixed environment-by-risk cells. The schema spells these fields in SCREAMING_SNAKE_CASE.
#[derive(cynic::InputObject, Debug, Clone)]
#[cynic(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct FixedApprovalPolicyMatrixInput {
    pub development_low: ApprovalPolicyCellInput,
    pub development_medium: ApprovalPolicyCellInput,
    pub development_high: ApprovalPolicyCellInput,
    pub staging_low: ApprovalPolicyCellInput,
    pub staging_medium: ApprovalPolicyCellInput,
    pub staging_high: ApprovalPolicyCellInput,
    pub production_low: ApprovalPolicyCellInput,
    pub production_medium: ApprovalPolicyCellInput,
    pub production_high: ApprovalPolicyCellInput,
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

impl FixedApprovalPolicyMatrixInput {
    /// Builds the input from cells in `NINE_CELLS` order.
    pub fn from_cells(cells: [ApprovalPolicyCellInput; 9]) -> Self {
        let [development_low, development_medium, development_high, staging_low, staging_medium, staging_high, production_low, production_medium, production_high] =
            cells;
        Self {
            development_low,
            development_medium,
            development_high,
            staging_low,
            staging_medium,
            staging_high,
            production_low,
            production_medium,
            production_high,
        }
    }
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdateProjectApprovalPolicyInput {
    pub project_id: cynic::Id,
    pub expected_revision: i32,
    pub matrix: FixedApprovalPolicyMatrixInput,
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
