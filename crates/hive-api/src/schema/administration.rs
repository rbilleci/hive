//! Ports `AdministrationGraphql`/`AdministrationResolver` in full: the two read
//! fields (`organizationAdministration`/`projectAdministration`) and the ten
//! write fields (`createProject`, `addAdministrationMembership`,
//! `replaceAdministrationMembershipRoles`, `endAdministrationMembership`,
//! `archiveAdministrationScope`, `restoreAdministrationScope`,
//! `updateProjectBudgetPolicy`, `updateProjectApprovalPolicy`,
//! `updateProjectGeneral`, `saveProjectSettingsConnection`).

use crate::schema::RequestPrincipal;
use async_graphql::{Context, InputObject, Interface, Object, SimpleObject};
use hive_application::administration::{
    AdministrationMembership as AppMembership, AdministrationMutationResult as AppMutationResult,
    AdministrationPrincipal as AppPrincipal, AdministrationProblem as AppProblem,
    AdministrationProblemKind as AppProblemKind, AdministrationService,
    ApprovalPolicy as AppApprovalPolicy, ApprovalPolicyVersion as AppApprovalPolicyVersion,
    ApprovalRule as AppApprovalRule, BudgetPolicy as AppBudgetPolicy,
    BudgetStatus as AppBudgetStatus, OrganizationAdministration as AppOrganizationAdministration,
    ProjectAdministration as AppProjectAdministration,
    ProjectSettingsConnection as AppProjectSettingsConnection,
};
use hive_persistence::administration::PgAdministrationRepository;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(SimpleObject)]
pub struct AdministrationPrincipal {
    pub id: async_graphql::ID,
    pub display_name: String,
    pub email: String,
}

impl From<AppPrincipal> for AdministrationPrincipal {
    fn from(principal: AppPrincipal) -> Self {
        Self {
            id: async_graphql::ID(principal.id.to_string()),
            display_name: principal.display_name,
            email: principal.email,
        }
    }
}

#[derive(SimpleObject)]
pub struct AdministrationMembership {
    pub id: async_graphql::ID,
    pub principal_id: async_graphql::ID,
    pub display_name: String,
    pub email: String,
    pub role_codes: Vec<String>,
    pub project_access_summary: Vec<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub revision: i32,
}

impl From<AppMembership> for AdministrationMembership {
    fn from(membership: AppMembership) -> Self {
        Self {
            id: async_graphql::ID(membership.id.to_string()),
            principal_id: async_graphql::ID(membership.principal_id.to_string()),
            display_name: membership.display_name,
            email: membership.email,
            role_codes: membership.role_codes,
            project_access_summary: membership.project_access_summary,
            started_at: hive_domain::java_offset_date_time_string(membership.started_at),
            ended_at: membership
                .ended_at
                .map(hive_domain::java_offset_date_time_string),
            last_seen_at: membership
                .last_seen_at
                .map(hive_domain::java_offset_date_time_string),
            revision: membership.revision as i32,
        }
    }
}

#[derive(SimpleObject)]
pub struct ProjectBudgetPolicy {
    pub revision: i32,
    pub currency: String,
    pub monthly_limit_cents: i32,
    pub warning_threshold_cents: i32,
    pub change_reason: String,
    pub created_at: String,
}

impl From<AppBudgetPolicy> for ProjectBudgetPolicy {
    fn from(budget: AppBudgetPolicy) -> Self {
        Self {
            revision: budget.revision as i32,
            currency: budget.currency,
            monthly_limit_cents: budget.monthly_limit_cents,
            warning_threshold_cents: budget.warning_threshold_cents,
            change_reason: budget.change_reason,
            created_at: hive_domain::java_offset_date_time_string(budget.created_at),
        }
    }
}

#[derive(SimpleObject)]
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

impl From<AppBudgetStatus> for ProjectBudgetStatus {
    fn from(status: AppBudgetStatus) -> Self {
        Self {
            state: status.state,
            reason: status.reason,
            amount_cents: status.amount_cents,
            includes_estimates: status.includes_estimates,
            currency: status.currency,
            period_start: status
                .period_start
                .map(hive_domain::java_offset_date_time_string),
            period_end: status
                .period_end
                .map(hive_domain::java_offset_date_time_string),
            data_as_of: status
                .data_as_of
                .map(hive_domain::java_offset_date_time_string),
            last_successful_import_at: status
                .last_successful_import_at
                .map(hive_domain::java_offset_date_time_string),
        }
    }
}

#[derive(SimpleObject)]
pub struct ApprovalPolicyRule {
    pub cell: String,
    pub required_evidence: Vec<String>,
    pub required_approvers: i32,
}

fn rules(matrix: std::collections::BTreeMap<String, AppApprovalRule>) -> Vec<ApprovalPolicyRule> {
    matrix
        .into_iter()
        .map(|(cell, rule)| ApprovalPolicyRule {
            cell,
            required_evidence: rule.required_evidence,
            required_approvers: rule.required_approvers,
        })
        .collect()
}

#[derive(SimpleObject)]
pub struct ProjectApprovalPolicyVersion {
    pub revision: i32,
    pub digest: String,
    pub matrix: Vec<ApprovalPolicyRule>,
    pub change_reason: String,
    pub created_at: String,
}

impl From<AppApprovalPolicyVersion> for ProjectApprovalPolicyVersion {
    fn from(version: AppApprovalPolicyVersion) -> Self {
        Self {
            revision: version.revision as i32,
            digest: version.digest,
            matrix: rules(version.matrix),
            change_reason: version.change_reason,
            created_at: hive_domain::java_offset_date_time_string(version.created_at),
        }
    }
}

#[derive(SimpleObject)]
pub struct ProjectApprovalPolicy {
    pub id: async_graphql::ID,
    pub revision: i32,
    pub digest: String,
    pub matrix: Vec<ApprovalPolicyRule>,
    pub change_reason: String,
    pub created_at: String,
    pub history: Vec<ProjectApprovalPolicyVersion>,
}

impl From<AppApprovalPolicy> for ProjectApprovalPolicy {
    fn from(policy: AppApprovalPolicy) -> Self {
        Self {
            id: async_graphql::ID(policy.id.to_string()),
            revision: policy.revision as i32,
            digest: policy.digest,
            matrix: rules(policy.matrix),
            change_reason: policy.change_reason,
            created_at: hive_domain::java_offset_date_time_string(policy.created_at),
            history: policy
                .history
                .into_iter()
                .map(ProjectApprovalPolicyVersion::from)
                .collect(),
        }
    }
}

#[derive(SimpleObject)]
pub struct ProjectSettingsConnection {
    pub id: async_graphql::ID,
    pub display_name: String,
    pub definition_version: String,
    pub environment: String,
    pub credential_status: String,
    pub lifecycle_status: String,
    pub agent_count: i32,
    pub revision: i32,
}

impl From<AppProjectSettingsConnection> for ProjectSettingsConnection {
    fn from(connection: AppProjectSettingsConnection) -> Self {
        Self {
            id: async_graphql::ID(connection.id.to_string()),
            display_name: connection.display_name,
            definition_version: connection.definition_version,
            environment: connection.environment,
            credential_status: connection.credential_status,
            lifecycle_status: connection.lifecycle_status,
            agent_count: connection.agent_count,
            revision: connection.revision as i32,
        }
    }
}

#[derive(SimpleObject)]
pub struct OrganizationAdministration {
    pub id: async_graphql::ID,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub revision: i32,
    pub memberships: Vec<AdministrationMembership>,
    pub available_principals: Vec<AdministrationPrincipal>,
    pub assignable_roles: Vec<String>,
    pub capabilities: Vec<String>,
}

impl From<AppOrganizationAdministration> for OrganizationAdministration {
    fn from(organization: AppOrganizationAdministration) -> Self {
        Self {
            id: async_graphql::ID(organization.id.to_string()),
            slug: organization.slug,
            display_name: organization.display_name,
            lifecycle_status: organization.lifecycle_status,
            revision: organization.revision as i32,
            memberships: organization
                .memberships
                .into_iter()
                .map(AdministrationMembership::from)
                .collect(),
            available_principals: organization
                .available_principals
                .into_iter()
                .map(AdministrationPrincipal::from)
                .collect(),
            assignable_roles: organization.assignable_roles,
            capabilities: organization.capabilities,
        }
    }
}

#[derive(SimpleObject)]
pub struct ProjectAdministration {
    pub id: async_graphql::ID,
    pub organization_id: async_graphql::ID,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub lifecycle_status: String,
    pub revision: i32,
    pub memberships: Vec<AdministrationMembership>,
    pub available_principals: Vec<AdministrationPrincipal>,
    pub assignable_roles: Vec<String>,
    pub budget_policy: Option<ProjectBudgetPolicy>,
    pub budget_history: Vec<ProjectBudgetPolicy>,
    pub budget_status: ProjectBudgetStatus,
    pub approval_policy: Option<ProjectApprovalPolicy>,
    pub connections: Vec<ProjectSettingsConnection>,
    pub capabilities: Vec<String>,
}

impl From<AppProjectAdministration> for ProjectAdministration {
    fn from(project: AppProjectAdministration) -> Self {
        Self {
            id: async_graphql::ID(project.id.to_string()),
            organization_id: async_graphql::ID(project.organization_id.to_string()),
            slug: project.slug,
            display_name: project.display_name,
            description: project.description,
            lifecycle_status: project.lifecycle_status,
            revision: project.revision as i32,
            memberships: project
                .memberships
                .into_iter()
                .map(AdministrationMembership::from)
                .collect(),
            available_principals: project
                .available_principals
                .into_iter()
                .map(AdministrationPrincipal::from)
                .collect(),
            assignable_roles: project.assignable_roles,
            budget_policy: project.budget_policy.map(ProjectBudgetPolicy::from),
            budget_history: project
                .budget_history
                .into_iter()
                .map(ProjectBudgetPolicy::from)
                .collect(),
            budget_status: ProjectBudgetStatus::from(project.budget_status),
            approval_policy: project.approval_policy.map(ProjectApprovalPolicy::from),
            connections: project
                .connections
                .into_iter()
                .map(ProjectSettingsConnection::from)
                .collect(),
            capabilities: project.capabilities,
        }
    }
}

fn service(
    ctx: &Context<'_>,
) -> async_graphql::Result<AdministrationService<PgAdministrationRepository>> {
    let repository = PgAdministrationRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
    Ok(AdministrationService::new(repository))
}

fn principal(ctx: &Context<'_>) -> async_graphql::Result<Uuid> {
    Ok(ctx.data::<RequestPrincipal>()?.0)
}

pub struct AdministrationQueries;

#[Object]
impl AdministrationQueries {
    /// Ports `AdministrationResolver.organization`.
    async fn organization_administration(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> async_graphql::Result<Option<OrganizationAdministration>> {
        let organization = service(ctx)?
            .find_organization(principal(ctx)?, id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(organization.map(OrganizationAdministration::from))
    }

    /// Ports `AdministrationResolver.project`.
    async fn project_administration(
        &self,
        ctx: &Context<'_>,
        id: async_graphql::ID,
    ) -> async_graphql::Result<Option<ProjectAdministration>> {
        let project = service(ctx)?
            .find_project(principal(ctx)?, id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(project.map(ProjectAdministration::from))
    }
}

#[derive(SimpleObject)]
pub struct AdministrationNotFoundProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct AdministrationAuthorizationProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct AdministrationValidationProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct AdministrationLifecycleProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct AdministrationPolicyWeakeningProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct AdministrationRevisionConflict {
    pub code: String,
    pub message: String,
    pub resource_id: async_graphql::ID,
    pub expected_revision: i32,
    pub actual_revision: i32,
}

/// Ports the `AdministrationProblem` GraphQL interface: every variant shares
/// exactly `code`/`message`; `AdministrationRevisionConflict` carries three
/// more fields of its own that a client reaches through an inline fragment.
#[derive(Interface)]
#[allow(clippy::duplicated_attributes)]
#[graphql(field(name = "code", ty = "String"))]
#[graphql(field(name = "message", ty = "String"))]
pub enum AdministrationProblem {
    NotFound(AdministrationNotFoundProblem),
    Authorization(AdministrationAuthorizationProblem),
    Validation(AdministrationValidationProblem),
    Lifecycle(AdministrationLifecycleProblem),
    PolicyWeakening(AdministrationPolicyWeakeningProblem),
    RevisionConflict(AdministrationRevisionConflict),
}

fn problem_message(kind: AppProblemKind) -> &'static str {
    match kind {
        AppProblemKind::NotFound | AppProblemKind::Forbidden => {
            "This administration resource is unavailable."
        }
        AppProblemKind::RevisionConflict => "This resource changed after you opened it.",
        AppProblemKind::InvalidInput => "The submitted administration values are not supported.",
        AppProblemKind::ProtectedLifecycle => "This lifecycle transition is unavailable.",
        AppProblemKind::PolicyWeakening => {
            "The fixed local approval policy can only become stronger."
        }
    }
}

fn problem_code(kind: AppProblemKind) -> &'static str {
    match kind {
        AppProblemKind::NotFound => "NOT_FOUND",
        AppProblemKind::Forbidden => "FORBIDDEN",
        AppProblemKind::RevisionConflict => "REVISION_CONFLICT",
        AppProblemKind::InvalidInput => "INVALID_INPUT",
        AppProblemKind::ProtectedLifecycle => "PROTECTED_LIFECYCLE",
        AppProblemKind::PolicyWeakening => "POLICY_WEAKENING",
    }
}

impl From<AppProblem> for AdministrationProblem {
    fn from(problem: AppProblem) -> Self {
        let code = problem_code(problem.kind).to_string();
        let message = problem_message(problem.kind).to_string();
        match problem.kind {
            AppProblemKind::NotFound => {
                AdministrationProblem::NotFound(AdministrationNotFoundProblem { code, message })
            }
            AppProblemKind::Forbidden => {
                AdministrationProblem::Authorization(AdministrationAuthorizationProblem {
                    code,
                    message,
                })
            }
            AppProblemKind::InvalidInput => {
                AdministrationProblem::Validation(AdministrationValidationProblem { code, message })
            }
            AppProblemKind::ProtectedLifecycle => {
                AdministrationProblem::Lifecycle(AdministrationLifecycleProblem { code, message })
            }
            AppProblemKind::PolicyWeakening => {
                AdministrationProblem::PolicyWeakening(AdministrationPolicyWeakeningProblem {
                    code,
                    message,
                })
            }
            AppProblemKind::RevisionConflict => {
                AdministrationProblem::RevisionConflict(AdministrationRevisionConflict {
                    code,
                    message,
                    resource_id: async_graphql::ID(problem.resource_id.unwrap_or_default()),
                    expected_revision: problem.expected_revision as i32,
                    actual_revision: problem.actual_revision as i32,
                })
            }
        }
    }
}

#[derive(SimpleObject)]
pub struct AdministrationMutationPayload {
    pub organization: Option<OrganizationAdministration>,
    pub project: Option<ProjectAdministration>,
    pub problems: Vec<AdministrationProblem>,
}

impl From<AppMutationResult> for AdministrationMutationPayload {
    fn from(result: AppMutationResult) -> Self {
        Self {
            organization: result.organization.map(OrganizationAdministration::from),
            project: result.project.map(ProjectAdministration::from),
            problems: result
                .problem
                .into_iter()
                .map(AdministrationProblem::from)
                .collect(),
        }
    }
}

#[derive(InputObject)]
pub struct CreateProjectInput {
    pub organization_id: async_graphql::ID,
    pub expected_revision: i32,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
}

#[derive(InputObject)]
pub struct AdministrationMembershipInput {
    pub scope: String,
    pub scope_id: async_graphql::ID,
    pub principal_id: async_graphql::ID,
    pub role_codes: Vec<String>,
    pub expected_scope_revision: i32,
}

#[derive(InputObject)]
pub struct ReplaceAdministrationMembershipInput {
    pub scope: String,
    pub scope_id: async_graphql::ID,
    pub membership_id: async_graphql::ID,
    pub role_codes: Vec<String>,
    pub expected_revision: i32,
}

#[derive(InputObject)]
pub struct EndAdministrationMembershipInput {
    pub scope: String,
    pub scope_id: async_graphql::ID,
    pub membership_id: async_graphql::ID,
    pub expected_revision: i32,
    pub reason: String,
}

#[derive(InputObject)]
pub struct LifecycleAdministrationInput {
    pub scope: String,
    pub scope_id: async_graphql::ID,
    pub expected_revision: i32,
    pub reason: Option<String>,
    pub confirmation: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateProjectBudgetPolicyInput {
    pub project_id: async_graphql::ID,
    pub expected_revision: i32,
    pub currency: String,
    pub monthly_limit_cents: i32,
    pub warning_threshold_cents: i32,
    pub reason: String,
}

#[derive(InputObject)]
pub struct ApprovalPolicyCellInput {
    pub required_evidence: Vec<String>,
    pub required_approvers: i32,
}

/// Ports `FixedApprovalPolicyMatrixInput`: nine required fields, one per fixed
/// environment/risk cell, not a free-form map. Java declares these nine wire field names in
/// SCREAMING_SNAKE_CASE (matching the cell's own risk-tier key, e.g. `DEVELOPMENT_LOW`), not
/// async-graphql's default camelCase conversion of the Rust field name — the console's admin UI
/// sends exactly these keys, so the wire name must match Java's schema field-for-field.
#[derive(InputObject)]
pub struct FixedApprovalPolicyMatrixInput {
    #[graphql(name = "DEVELOPMENT_LOW")]
    pub development_low: ApprovalPolicyCellInput,
    #[graphql(name = "DEVELOPMENT_MEDIUM")]
    pub development_medium: ApprovalPolicyCellInput,
    #[graphql(name = "DEVELOPMENT_HIGH")]
    pub development_high: ApprovalPolicyCellInput,
    #[graphql(name = "STAGING_LOW")]
    pub staging_low: ApprovalPolicyCellInput,
    #[graphql(name = "STAGING_MEDIUM")]
    pub staging_medium: ApprovalPolicyCellInput,
    #[graphql(name = "STAGING_HIGH")]
    pub staging_high: ApprovalPolicyCellInput,
    #[graphql(name = "PRODUCTION_LOW")]
    pub production_low: ApprovalPolicyCellInput,
    #[graphql(name = "PRODUCTION_MEDIUM")]
    pub production_medium: ApprovalPolicyCellInput,
    #[graphql(name = "PRODUCTION_HIGH")]
    pub production_high: ApprovalPolicyCellInput,
}

impl From<FixedApprovalPolicyMatrixInput> for BTreeMap<String, AppApprovalRule> {
    fn from(matrix: FixedApprovalPolicyMatrixInput) -> Self {
        let cell = |cell: ApprovalPolicyCellInput| AppApprovalRule {
            required_evidence: cell.required_evidence,
            required_approvers: cell.required_approvers,
        };
        BTreeMap::from([
            ("DEVELOPMENT_LOW".to_string(), cell(matrix.development_low)),
            (
                "DEVELOPMENT_MEDIUM".to_string(),
                cell(matrix.development_medium),
            ),
            (
                "DEVELOPMENT_HIGH".to_string(),
                cell(matrix.development_high),
            ),
            ("STAGING_LOW".to_string(), cell(matrix.staging_low)),
            ("STAGING_MEDIUM".to_string(), cell(matrix.staging_medium)),
            ("STAGING_HIGH".to_string(), cell(matrix.staging_high)),
            ("PRODUCTION_LOW".to_string(), cell(matrix.production_low)),
            (
                "PRODUCTION_MEDIUM".to_string(),
                cell(matrix.production_medium),
            ),
            ("PRODUCTION_HIGH".to_string(), cell(matrix.production_high)),
        ])
    }
}

#[derive(InputObject)]
pub struct UpdateProjectApprovalPolicyInput {
    pub project_id: async_graphql::ID,
    pub expected_revision: i32,
    pub matrix: FixedApprovalPolicyMatrixInput,
    pub reason: String,
}

#[derive(InputObject)]
pub struct UpdateProjectGeneralInput {
    pub project_id: async_graphql::ID,
    pub expected_revision: i32,
    pub display_name: String,
    pub description: String,
}

#[derive(InputObject)]
pub struct SaveProjectSettingsConnectionInput {
    pub project_id: async_graphql::ID,
    pub connection_id: Option<async_graphql::ID>,
    pub expected_revision: i32,
    pub display_name: String,
    pub definition_version: String,
    pub environment: String,
    pub credential_status: String,
    pub lifecycle_status: String,
}

pub struct AdministrationMutations;

#[Object]
impl AdministrationMutations {
    /// Ports `AdministrationResolver.createProject`.
    async fn create_project(
        &self,
        ctx: &Context<'_>,
        input: CreateProjectInput,
    ) -> async_graphql::Result<AdministrationMutationPayload> {
        let result = service(ctx)?
            .create_project(
                principal(ctx)?,
                input.organization_id.as_str(),
                input.expected_revision as i64,
                input.slug,
                input.display_name,
                input.description,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AdministrationMutationPayload::from(result))
    }

    /// Ports `AdministrationResolver.addMembership`.
    async fn add_administration_membership(
        &self,
        ctx: &Context<'_>,
        input: AdministrationMembershipInput,
    ) -> async_graphql::Result<AdministrationMutationPayload> {
        let result = service(ctx)?
            .add_membership(
                principal(ctx)?,
                &input.scope,
                input.scope_id.as_str(),
                input.principal_id.as_str(),
                input.role_codes,
                input.expected_scope_revision as i64,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AdministrationMutationPayload::from(result))
    }

    /// Ports `AdministrationResolver.replaceMembership`.
    async fn replace_administration_membership_roles(
        &self,
        ctx: &Context<'_>,
        input: ReplaceAdministrationMembershipInput,
    ) -> async_graphql::Result<AdministrationMutationPayload> {
        let result = service(ctx)?
            .replace_membership(
                principal(ctx)?,
                &input.scope,
                input.scope_id.as_str(),
                input.membership_id.as_str(),
                input.role_codes,
                input.expected_revision as i64,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AdministrationMutationPayload::from(result))
    }

    /// Ports `AdministrationResolver.endMembership`.
    async fn end_administration_membership(
        &self,
        ctx: &Context<'_>,
        input: EndAdministrationMembershipInput,
    ) -> async_graphql::Result<AdministrationMutationPayload> {
        let result = service(ctx)?
            .end_membership(
                principal(ctx)?,
                &input.scope,
                input.scope_id.as_str(),
                input.membership_id.as_str(),
                input.expected_revision as i64,
                &input.reason,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AdministrationMutationPayload::from(result))
    }

    /// Ports `AdministrationResolver.archive`.
    async fn archive_administration_scope(
        &self,
        ctx: &Context<'_>,
        input: LifecycleAdministrationInput,
    ) -> async_graphql::Result<AdministrationMutationPayload> {
        let result = service(ctx)?
            .lifecycle(
                principal(ctx)?,
                &input.scope,
                input.scope_id.as_str(),
                input.expected_revision as i64,
                input.reason.as_deref(),
                input.confirmation.as_deref(),
                true,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AdministrationMutationPayload::from(result))
    }

    /// Ports `AdministrationResolver.restore`.
    async fn restore_administration_scope(
        &self,
        ctx: &Context<'_>,
        input: LifecycleAdministrationInput,
    ) -> async_graphql::Result<AdministrationMutationPayload> {
        let result = service(ctx)?
            .lifecycle(
                principal(ctx)?,
                &input.scope,
                input.scope_id.as_str(),
                input.expected_revision as i64,
                None,
                None,
                false,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AdministrationMutationPayload::from(result))
    }

    /// Ports `AdministrationResolver.budget`.
    async fn update_project_budget_policy(
        &self,
        ctx: &Context<'_>,
        input: UpdateProjectBudgetPolicyInput,
    ) -> async_graphql::Result<AdministrationMutationPayload> {
        let result = service(ctx)?
            .update_budget(
                principal(ctx)?,
                input.project_id.as_str(),
                input.expected_revision as i64,
                Some(input.currency.as_str()),
                input.monthly_limit_cents,
                input.warning_threshold_cents,
                &input.reason,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AdministrationMutationPayload::from(result))
    }

    /// Ports `AdministrationResolver.approval`.
    async fn update_project_approval_policy(
        &self,
        ctx: &Context<'_>,
        input: UpdateProjectApprovalPolicyInput,
    ) -> async_graphql::Result<AdministrationMutationPayload> {
        let matrix: BTreeMap<String, AppApprovalRule> = input.matrix.into();
        let result = service(ctx)?
            .update_approval_policy(
                principal(ctx)?,
                input.project_id.as_str(),
                input.expected_revision as i64,
                matrix,
                &input.reason,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AdministrationMutationPayload::from(result))
    }

    /// Ports `AdministrationResolver.general`.
    async fn update_project_general(
        &self,
        ctx: &Context<'_>,
        input: UpdateProjectGeneralInput,
    ) -> async_graphql::Result<AdministrationMutationPayload> {
        let result = service(ctx)?
            .update_project_general(
                principal(ctx)?,
                input.project_id.as_str(),
                input.expected_revision as i64,
                &input.display_name,
                Some(&input.description),
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AdministrationMutationPayload::from(result))
    }

    /// Ports `AdministrationResolver.connection`.
    async fn save_project_settings_connection(
        &self,
        ctx: &Context<'_>,
        input: SaveProjectSettingsConnectionInput,
    ) -> async_graphql::Result<AdministrationMutationPayload> {
        let connection_id = input.connection_id.as_ref().map(|id| id.as_str());
        let result = service(ctx)?
            .save_project_connection(
                principal(ctx)?,
                input.project_id.as_str(),
                connection_id,
                input.expected_revision as i64,
                &input.display_name,
                &input.definition_version,
                &input.environment,
                &input.credential_status,
                &input.lifecycle_status,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AdministrationMutationPayload::from(result))
    }
}
