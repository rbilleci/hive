//! Ports `AdministrationGraphql`/`AdministrationResolver` in full: 2 read fields
//! (`organizationAdministration`/`projectAdministration`) and 10 mutations. No field or argument
//! carries a default, so every root operation is a plain `#[CustomFields]` method, like
//! `agent.rs`/`configuration.rs`.
//!
//! Fifth interface this port builds (`AdministrationProblem`, 6 implementors) — same pattern as
//! the prior four (`agent.rs`'s doc comment has the full trail on variant naming and
//! `#[allow(clippy::enum_variant_names)]`).
//!
//! `FixedApprovalPolicyMatrixInput` cannot go through `#[derive(CustomInputType)]` at all: its 9
//! fields are named `DEVELOPMENT_LOW`/`DEVELOPMENT_MEDIUM`/etc — fixed SCREAMING_SNAKE_CASE risk-
//! tier keys the console's admin UI sends literally, not a camelCase rendering of any Rust
//! identifier (`GSR-WIRE-CASE` doesn't cover this: there is no valid Rust identifier that
//! `stringify!` would print as `DEVELOPMENT_LOW` while still reading naturally as Rust code, and
//! the derive macro has no per-field rename hook — only the struct-level `input_type_name`
//! override exists). Hand-built instead, replicating exactly what
//! `custom_input_type.rs`'s `derive_custom_input_type_struct` generates for a normal struct, just
//! with literal string keys instead of `stringify!(field_ident)` — the same technique
//! `audit.rs`'s `AuditResourceReference` used on the output side for its `type` keyword field.
//!
//! `roleCodes`, `projectAccessSummary`, `requiredEvidence`, `assignableRoles`, `capabilities` are
//! all `scalars::StringList`, per the `Vec<String>` panic `audit.rs`/`configuration.rs` found.

use crate::schema::scalars::{Id, StringList};
use crate::schema::RequestPrincipal;
use async_graphql::dynamic::{InputObject, InputValue, TypeRef, ValueAccessor};
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
use seaography::{
    BuilderContext, CustomFields, CustomInputObject, CustomInputType, CustomOutputObject,
    CustomOutputType, SeaResult, SeaographyError,
};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const ADMINISTRATION_PROBLEM_INTERFACE: &str = "AdministrationProblem";

#[allow(non_snake_case)]
mod wire {
    use super::*;

    #[derive(CustomOutputType, Clone)]
    pub struct AdministrationPrincipal {
        pub id: Id,
        pub displayName: String,
        pub email: String,
    }

    impl From<AppPrincipal> for AdministrationPrincipal {
        fn from(principal: AppPrincipal) -> Self {
            Self {
                id: principal.id.to_string().into(),
                displayName: principal.display_name,
                email: principal.email,
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AdministrationMembership {
        pub id: Id,
        pub principalId: Id,
        pub displayName: String,
        pub email: String,
        pub roleCodes: StringList,
        pub projectAccessSummary: StringList,
        pub startedAt: String,
        pub endedAt: Option<String>,
        pub lastSeenAt: Option<String>,
        pub revision: i32,
    }

    impl From<AppMembership> for AdministrationMembership {
        fn from(membership: AppMembership) -> Self {
            Self {
                id: membership.id.to_string().into(),
                principalId: membership.principal_id.to_string().into(),
                displayName: membership.display_name,
                email: membership.email,
                roleCodes: membership.role_codes.into(),
                projectAccessSummary: membership.project_access_summary.into(),
                startedAt: hive_domain::java_offset_date_time_string(membership.started_at),
                endedAt: membership
                    .ended_at
                    .map(hive_domain::java_offset_date_time_string),
                lastSeenAt: membership
                    .last_seen_at
                    .map(hive_domain::java_offset_date_time_string),
                revision: membership.revision as i32,
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ProjectBudgetPolicy {
        pub revision: i32,
        pub currency: String,
        pub monthlyLimitCents: i32,
        pub warningThresholdCents: i32,
        pub changeReason: String,
        pub createdAt: String,
    }

    impl From<AppBudgetPolicy> for ProjectBudgetPolicy {
        fn from(budget: AppBudgetPolicy) -> Self {
            Self {
                revision: budget.revision as i32,
                currency: budget.currency,
                monthlyLimitCents: budget.monthly_limit_cents,
                warningThresholdCents: budget.warning_threshold_cents,
                changeReason: budget.change_reason,
                createdAt: hive_domain::java_offset_date_time_string(budget.created_at),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ProjectBudgetStatus {
        pub state: String,
        pub reason: Option<String>,
        pub amountCents: Option<i32>,
        pub includesEstimates: bool,
        pub currency: Option<String>,
        pub periodStart: Option<String>,
        pub periodEnd: Option<String>,
        pub dataAsOf: Option<String>,
        pub lastSuccessfulImportAt: Option<String>,
    }

    impl From<AppBudgetStatus> for ProjectBudgetStatus {
        fn from(status: AppBudgetStatus) -> Self {
            Self {
                state: status.state,
                reason: status.reason,
                amountCents: status.amount_cents,
                includesEstimates: status.includes_estimates,
                currency: status.currency,
                periodStart: status
                    .period_start
                    .map(hive_domain::java_offset_date_time_string),
                periodEnd: status
                    .period_end
                    .map(hive_domain::java_offset_date_time_string),
                dataAsOf: status
                    .data_as_of
                    .map(hive_domain::java_offset_date_time_string),
                lastSuccessfulImportAt: status
                    .last_successful_import_at
                    .map(hive_domain::java_offset_date_time_string),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ApprovalPolicyRule {
        pub cell: String,
        pub requiredEvidence: StringList,
        pub requiredApprovers: i32,
    }

    fn rules(matrix: BTreeMap<String, AppApprovalRule>) -> Vec<ApprovalPolicyRule> {
        matrix
            .into_iter()
            .map(|(cell, rule)| ApprovalPolicyRule {
                cell,
                requiredEvidence: rule.required_evidence.into(),
                requiredApprovers: rule.required_approvers,
            })
            .collect()
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ProjectApprovalPolicyVersion {
        pub revision: i32,
        pub digest: String,
        pub matrix: Vec<ApprovalPolicyRule>,
        pub changeReason: String,
        pub createdAt: String,
    }

    impl From<AppApprovalPolicyVersion> for ProjectApprovalPolicyVersion {
        fn from(version: AppApprovalPolicyVersion) -> Self {
            Self {
                revision: version.revision as i32,
                digest: version.digest,
                matrix: rules(version.matrix),
                changeReason: version.change_reason,
                createdAt: hive_domain::java_offset_date_time_string(version.created_at),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ProjectApprovalPolicy {
        pub id: Id,
        pub revision: i32,
        pub digest: String,
        pub matrix: Vec<ApprovalPolicyRule>,
        pub changeReason: String,
        pub createdAt: String,
        pub history: Vec<ProjectApprovalPolicyVersion>,
    }

    impl From<AppApprovalPolicy> for ProjectApprovalPolicy {
        fn from(policy: AppApprovalPolicy) -> Self {
            Self {
                id: policy.id.to_string().into(),
                revision: policy.revision as i32,
                digest: policy.digest,
                matrix: rules(policy.matrix),
                changeReason: policy.change_reason,
                createdAt: hive_domain::java_offset_date_time_string(policy.created_at),
                history: policy
                    .history
                    .into_iter()
                    .map(ProjectApprovalPolicyVersion::from)
                    .collect(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ProjectSettingsConnection {
        pub id: Id,
        pub displayName: String,
        pub definitionVersion: String,
        pub environment: String,
        pub credentialStatus: String,
        pub lifecycleStatus: String,
        pub agentCount: i32,
        pub revision: i32,
    }

    impl From<AppProjectSettingsConnection> for ProjectSettingsConnection {
        fn from(connection: AppProjectSettingsConnection) -> Self {
            Self {
                id: connection.id.to_string().into(),
                displayName: connection.display_name,
                definitionVersion: connection.definition_version,
                environment: connection.environment,
                credentialStatus: connection.credential_status,
                lifecycleStatus: connection.lifecycle_status,
                agentCount: connection.agent_count,
                revision: connection.revision as i32,
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct OrganizationAdministration {
        pub id: Id,
        pub slug: String,
        pub displayName: String,
        pub lifecycleStatus: String,
        pub revision: i32,
        pub memberships: Vec<AdministrationMembership>,
        pub availablePrincipals: Vec<AdministrationPrincipal>,
        pub assignableRoles: StringList,
        pub capabilities: StringList,
    }

    impl From<AppOrganizationAdministration> for OrganizationAdministration {
        fn from(organization: AppOrganizationAdministration) -> Self {
            Self {
                id: organization.id.to_string().into(),
                slug: organization.slug,
                displayName: organization.display_name,
                lifecycleStatus: organization.lifecycle_status,
                revision: organization.revision as i32,
                memberships: organization
                    .memberships
                    .into_iter()
                    .map(AdministrationMembership::from)
                    .collect(),
                availablePrincipals: organization
                    .available_principals
                    .into_iter()
                    .map(AdministrationPrincipal::from)
                    .collect(),
                assignableRoles: organization.assignable_roles.into(),
                capabilities: organization.capabilities.into(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ProjectAdministration {
        pub id: Id,
        pub organizationId: Id,
        pub slug: String,
        pub displayName: String,
        pub description: String,
        pub lifecycleStatus: String,
        pub revision: i32,
        pub memberships: Vec<AdministrationMembership>,
        pub availablePrincipals: Vec<AdministrationPrincipal>,
        pub assignableRoles: StringList,
        pub budgetPolicy: Option<ProjectBudgetPolicy>,
        pub budgetHistory: Vec<ProjectBudgetPolicy>,
        pub budgetStatus: ProjectBudgetStatus,
        pub approvalPolicy: Option<ProjectApprovalPolicy>,
        pub connections: Vec<ProjectSettingsConnection>,
        pub capabilities: StringList,
    }

    impl From<AppProjectAdministration> for ProjectAdministration {
        fn from(project: AppProjectAdministration) -> Self {
            Self {
                id: project.id.to_string().into(),
                organizationId: project.organization_id.to_string().into(),
                slug: project.slug,
                displayName: project.display_name,
                description: project.description,
                lifecycleStatus: project.lifecycle_status,
                revision: project.revision as i32,
                memberships: project
                    .memberships
                    .into_iter()
                    .map(AdministrationMembership::from)
                    .collect(),
                availablePrincipals: project
                    .available_principals
                    .into_iter()
                    .map(AdministrationPrincipal::from)
                    .collect(),
                assignableRoles: project.assignable_roles.into(),
                budgetPolicy: project.budget_policy.map(ProjectBudgetPolicy::from),
                budgetHistory: project
                    .budget_history
                    .into_iter()
                    .map(ProjectBudgetPolicy::from)
                    .collect(),
                budgetStatus: ProjectBudgetStatus::from(project.budget_status),
                approvalPolicy: project.approval_policy.map(ProjectApprovalPolicy::from),
                connections: project
                    .connections
                    .into_iter()
                    .map(ProjectSettingsConnection::from)
                    .collect(),
                capabilities: project.capabilities.into(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AdministrationNotFoundProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AdministrationAuthorizationProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AdministrationValidationProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AdministrationLifecycleProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AdministrationPolicyWeakeningProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AdministrationRevisionConflict {
        pub code: String,
        pub message: String,
        pub resourceId: Id,
        pub expectedRevision: i32,
        pub actualRevision: i32,
    }

    // clippy::enum_variant_names is a false positive here — see `agent.rs`'s `AgentDraftProblem`
    // for the full rationale.
    #[derive(CustomOutputType, Clone)]
    #[allow(clippy::enum_variant_names)]
    pub enum AdministrationProblem {
        AdministrationNotFoundProblem(AdministrationNotFoundProblem),
        AdministrationAuthorizationProblem(AdministrationAuthorizationProblem),
        AdministrationValidationProblem(AdministrationValidationProblem),
        AdministrationLifecycleProblem(AdministrationLifecycleProblem),
        AdministrationPolicyWeakeningProblem(AdministrationPolicyWeakeningProblem),
        AdministrationRevisionConflict(AdministrationRevisionConflict),
    }

    fn problem_message(kind: AppProblemKind) -> &'static str {
        match kind {
            AppProblemKind::NotFound | AppProblemKind::Forbidden => {
                "This administration resource is unavailable."
            }
            AppProblemKind::RevisionConflict => "This resource changed after you opened it.",
            AppProblemKind::InvalidInput => {
                "The submitted administration values are not supported."
            }
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
                AppProblemKind::NotFound => AdministrationProblem::AdministrationNotFoundProblem(
                    AdministrationNotFoundProblem { code, message },
                ),
                AppProblemKind::Forbidden => {
                    AdministrationProblem::AdministrationAuthorizationProblem(
                        AdministrationAuthorizationProblem { code, message },
                    )
                }
                AppProblemKind::InvalidInput => {
                    AdministrationProblem::AdministrationValidationProblem(
                        AdministrationValidationProblem { code, message },
                    )
                }
                AppProblemKind::ProtectedLifecycle => {
                    AdministrationProblem::AdministrationLifecycleProblem(
                        AdministrationLifecycleProblem { code, message },
                    )
                }
                AppProblemKind::PolicyWeakening => {
                    AdministrationProblem::AdministrationPolicyWeakeningProblem(
                        AdministrationPolicyWeakeningProblem { code, message },
                    )
                }
                AppProblemKind::RevisionConflict => {
                    AdministrationProblem::AdministrationRevisionConflict(
                        AdministrationRevisionConflict {
                            code,
                            message,
                            resourceId: problem.resource_id.unwrap_or_default().into(),
                            expectedRevision: problem.expected_revision as i32,
                            actualRevision: problem.actual_revision as i32,
                        },
                    )
                }
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
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

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "CreateProjectInput")]
    pub struct CreateProjectInput {
        pub organizationId: Id,
        pub expectedRevision: i32,
        pub slug: String,
        pub displayName: String,
        pub description: Option<String>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "AdministrationMembershipInput")]
    pub struct AdministrationMembershipInput {
        pub scope: String,
        pub scopeId: Id,
        pub principalId: Id,
        pub roleCodes: StringList,
        pub expectedScopeRevision: i32,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "ReplaceAdministrationMembershipInput")]
    pub struct ReplaceAdministrationMembershipInput {
        pub scope: String,
        pub scopeId: Id,
        pub membershipId: Id,
        pub roleCodes: StringList,
        pub expectedRevision: i32,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "EndAdministrationMembershipInput")]
    pub struct EndAdministrationMembershipInput {
        pub scope: String,
        pub scopeId: Id,
        pub membershipId: Id,
        pub expectedRevision: i32,
        pub reason: String,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "LifecycleAdministrationInput")]
    pub struct LifecycleAdministrationInput {
        pub scope: String,
        pub scopeId: Id,
        pub expectedRevision: i32,
        pub reason: Option<String>,
        pub confirmation: Option<String>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "UpdateProjectBudgetPolicyInput")]
    pub struct UpdateProjectBudgetPolicyInput {
        pub projectId: Id,
        pub expectedRevision: i32,
        pub currency: String,
        pub monthlyLimitCents: i32,
        pub warningThresholdCents: i32,
        pub reason: String,
    }

    #[derive(CustomInputType, Clone)]
    #[seaography(input_type_name = "ApprovalPolicyCellInput")]
    pub struct ApprovalPolicyCellInput {
        pub requiredEvidence: StringList,
        pub requiredApprovers: i32,
    }

    /// Ports `FixedApprovalPolicyMatrixInput`: nine required fields, one per fixed
    /// environment/risk cell, not a free-form map. Hand-built (see this file's own doc comment):
    /// Java's wire field names (`DEVELOPMENT_LOW`, ...) are not a camelCase rendering of any Rust
    /// identifier the derive macro's `stringify!(field_ident)` could ever produce.
    #[derive(Clone)]
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

    impl CustomInputType for FixedApprovalPolicyMatrixInput {
        fn gql_input_type_ref(_ctx: &'static BuilderContext) -> TypeRef {
            TypeRef::named_nn("FixedApprovalPolicyMatrixInput")
        }

        fn parse_value(
            context: &'static BuilderContext,
            value: Option<ValueAccessor<'_>>,
        ) -> SeaResult<Self> {
            let object = value
                .ok_or_else(|| SeaographyError::AsyncGraphQLError("Expected a value".into()))?
                .object()?;
            let cell = |key: &str| ApprovalPolicyCellInput::parse_value(context, object.get(key));
            Ok(Self {
                development_low: cell("DEVELOPMENT_LOW")?,
                development_medium: cell("DEVELOPMENT_MEDIUM")?,
                development_high: cell("DEVELOPMENT_HIGH")?,
                staging_low: cell("STAGING_LOW")?,
                staging_medium: cell("STAGING_MEDIUM")?,
                staging_high: cell("STAGING_HIGH")?,
                production_low: cell("PRODUCTION_LOW")?,
                production_medium: cell("PRODUCTION_MEDIUM")?,
                production_high: cell("PRODUCTION_HIGH")?,
            })
        }
    }

    impl CustomInputObject for FixedApprovalPolicyMatrixInput {
        fn input_object(context: &'static BuilderContext) -> InputObject {
            let field_type = ApprovalPolicyCellInput::gql_input_type_ref(context);
            [
                "DEVELOPMENT_LOW",
                "DEVELOPMENT_MEDIUM",
                "DEVELOPMENT_HIGH",
                "STAGING_LOW",
                "STAGING_MEDIUM",
                "STAGING_HIGH",
                "PRODUCTION_LOW",
                "PRODUCTION_MEDIUM",
                "PRODUCTION_HIGH",
            ]
            .into_iter()
            .fold(
                InputObject::new("FixedApprovalPolicyMatrixInput"),
                |object, key| object.field(InputValue::new(key, field_type.clone())),
            )
        }
    }

    impl From<FixedApprovalPolicyMatrixInput> for BTreeMap<String, AppApprovalRule> {
        fn from(matrix: FixedApprovalPolicyMatrixInput) -> Self {
            let cell = |cell: ApprovalPolicyCellInput| AppApprovalRule {
                required_evidence: cell.requiredEvidence.0,
                required_approvers: cell.requiredApprovers,
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

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "UpdateProjectApprovalPolicyInput")]
    pub struct UpdateProjectApprovalPolicyInput {
        pub projectId: Id,
        pub expectedRevision: i32,
        pub matrix: FixedApprovalPolicyMatrixInput,
        pub reason: String,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "UpdateProjectGeneralInput")]
    pub struct UpdateProjectGeneralInput {
        pub projectId: Id,
        pub expectedRevision: i32,
        pub displayName: String,
        pub description: String,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "SaveProjectSettingsConnectionInput")]
    pub struct SaveProjectSettingsConnectionInput {
        pub projectId: Id,
        pub connectionId: Option<Id>,
        pub expectedRevision: i32,
        pub displayName: String,
        pub definitionVersion: String,
        pub environment: String,
        pub credentialStatus: String,
        pub lifecycleStatus: String,
    }

    fn service(
        ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<AdministrationService<PgAdministrationRepository>> {
        let repository =
            PgAdministrationRepository::new(ctx.data::<sea_orm::DatabaseConnection>()?.clone());
        Ok(AdministrationService::new(repository))
    }

    fn principal(ctx: &async_graphql::Context<'_>) -> async_graphql::Result<Uuid> {
        Ok(ctx.data::<RequestPrincipal>()?.0)
    }

    pub struct AdministrationQueries;

    #[CustomFields]
    impl AdministrationQueries {
        // Ports `AdministrationResolver.organization`.
        async fn organizationAdministration(
            ctx: &async_graphql::Context<'_>,
            id: Id,
        ) -> async_graphql::Result<Option<OrganizationAdministration>> {
            let organization = service(ctx)?
                .find_organization(principal(ctx)?, &id.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(organization.map(OrganizationAdministration::from))
        }

        // Ports `AdministrationResolver.project`.
        async fn projectAdministration(
            ctx: &async_graphql::Context<'_>,
            id: Id,
        ) -> async_graphql::Result<Option<ProjectAdministration>> {
            let project = service(ctx)?
                .find_project(principal(ctx)?, &id.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(project.map(ProjectAdministration::from))
        }
    }

    pub struct AdministrationMutations;

    #[CustomFields]
    impl AdministrationMutations {
        // Ports `AdministrationResolver.createProject`.
        async fn createProject(
            ctx: &async_graphql::Context<'_>,
            input: CreateProjectInput,
        ) -> async_graphql::Result<AdministrationMutationPayload> {
            let result = service(ctx)?
                .create_project(
                    principal(ctx)?,
                    &input.organizationId.0,
                    input.expectedRevision as i64,
                    input.slug,
                    input.displayName,
                    input.description,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }

        // Ports `AdministrationResolver.addMembership`.
        async fn addAdministrationMembership(
            ctx: &async_graphql::Context<'_>,
            input: AdministrationMembershipInput,
        ) -> async_graphql::Result<AdministrationMutationPayload> {
            let result = service(ctx)?
                .add_membership(
                    principal(ctx)?,
                    &input.scope,
                    &input.scopeId.0,
                    &input.principalId.0,
                    input.roleCodes.0,
                    input.expectedScopeRevision as i64,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }

        // Ports `AdministrationResolver.replaceMembership`.
        async fn replaceAdministrationMembershipRoles(
            ctx: &async_graphql::Context<'_>,
            input: ReplaceAdministrationMembershipInput,
        ) -> async_graphql::Result<AdministrationMutationPayload> {
            let result = service(ctx)?
                .replace_membership(
                    principal(ctx)?,
                    &input.scope,
                    &input.scopeId.0,
                    &input.membershipId.0,
                    input.roleCodes.0,
                    input.expectedRevision as i64,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }

        // Ports `AdministrationResolver.endMembership`.
        async fn endAdministrationMembership(
            ctx: &async_graphql::Context<'_>,
            input: EndAdministrationMembershipInput,
        ) -> async_graphql::Result<AdministrationMutationPayload> {
            let result = service(ctx)?
                .end_membership(
                    principal(ctx)?,
                    &input.scope,
                    &input.scopeId.0,
                    &input.membershipId.0,
                    input.expectedRevision as i64,
                    &input.reason,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }

        // Ports `AdministrationResolver.archive`.
        async fn archiveAdministrationScope(
            ctx: &async_graphql::Context<'_>,
            input: LifecycleAdministrationInput,
        ) -> async_graphql::Result<AdministrationMutationPayload> {
            let result = service(ctx)?
                .lifecycle(
                    principal(ctx)?,
                    &input.scope,
                    &input.scopeId.0,
                    input.expectedRevision as i64,
                    input.reason.as_deref(),
                    input.confirmation.as_deref(),
                    true,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }

        // Ports `AdministrationResolver.restore`.
        async fn restoreAdministrationScope(
            ctx: &async_graphql::Context<'_>,
            input: LifecycleAdministrationInput,
        ) -> async_graphql::Result<AdministrationMutationPayload> {
            let result = service(ctx)?
                .lifecycle(
                    principal(ctx)?,
                    &input.scope,
                    &input.scopeId.0,
                    input.expectedRevision as i64,
                    None,
                    None,
                    false,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }

        // Ports `AdministrationResolver.budget`.
        async fn updateProjectBudgetPolicy(
            ctx: &async_graphql::Context<'_>,
            input: UpdateProjectBudgetPolicyInput,
        ) -> async_graphql::Result<AdministrationMutationPayload> {
            let result = service(ctx)?
                .update_budget(
                    principal(ctx)?,
                    &input.projectId.0,
                    input.expectedRevision as i64,
                    Some(input.currency.as_str()),
                    input.monthlyLimitCents,
                    input.warningThresholdCents,
                    &input.reason,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }

        // Ports `AdministrationResolver.approval`.
        async fn updateProjectApprovalPolicy(
            ctx: &async_graphql::Context<'_>,
            input: UpdateProjectApprovalPolicyInput,
        ) -> async_graphql::Result<AdministrationMutationPayload> {
            let matrix: BTreeMap<String, AppApprovalRule> = input.matrix.into();
            let result = service(ctx)?
                .update_approval_policy(
                    principal(ctx)?,
                    &input.projectId.0,
                    input.expectedRevision as i64,
                    matrix,
                    &input.reason,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }

        // Ports `AdministrationResolver.general`.
        async fn updateProjectGeneral(
            ctx: &async_graphql::Context<'_>,
            input: UpdateProjectGeneralInput,
        ) -> async_graphql::Result<AdministrationMutationPayload> {
            let result = service(ctx)?
                .update_project_general(
                    principal(ctx)?,
                    &input.projectId.0,
                    input.expectedRevision as i64,
                    &input.displayName,
                    Some(&input.description),
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }

        // Ports `AdministrationResolver.connection`.
        async fn saveProjectSettingsConnection(
            ctx: &async_graphql::Context<'_>,
            input: SaveProjectSettingsConnectionInput,
        ) -> async_graphql::Result<AdministrationMutationPayload> {
            let connection_id = input.connectionId.as_ref().map(|id| id.0.as_str());
            let result = service(ctx)?
                .save_project_connection(
                    principal(ctx)?,
                    &input.projectId.0,
                    connection_id,
                    input.expectedRevision as i64,
                    &input.displayName,
                    &input.definitionVersion,
                    &input.environment,
                    &input.credentialStatus,
                    &input.lifecycleStatus,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }
    }
}

pub use wire::{
    AdministrationAuthorizationProblem, AdministrationLifecycleProblem,
    AdministrationMembershipInput, AdministrationMutationPayload, AdministrationMutations,
    AdministrationNotFoundProblem, AdministrationPolicyWeakeningProblem, AdministrationQueries,
    AdministrationRevisionConflict, AdministrationValidationProblem, CreateProjectInput,
    EndAdministrationMembershipInput, FixedApprovalPolicyMatrixInput, LifecycleAdministrationInput,
    OrganizationAdministration, ProjectAdministration, ReplaceAdministrationMembershipInput,
    SaveProjectSettingsConnectionInput, UpdateProjectApprovalPolicyInput,
    UpdateProjectBudgetPolicyInput, UpdateProjectGeneralInput,
};

fn context() -> &'static BuilderContext {
    crate::schema::context()
}

/// This module's `Interface`, registered directly on the `SchemaBuilder` in `mod.rs::build()`
/// (`Builder` itself has no interface vector to push onto — same as the other interface modules).
pub fn interfaces() -> Vec<async_graphql::dynamic::Interface> {
    use async_graphql::dynamic::{Interface, InterfaceField};
    vec![Interface::new(ADMINISTRATION_PROBLEM_INTERFACE)
        .field(InterfaceField::new(
            "code",
            TypeRef::named_nn(TypeRef::STRING),
        ))
        .field(InterfaceField::new(
            "message",
            TypeRef::named_nn(TypeRef::STRING),
        ))]
}

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_query::<AdministrationQueries>();
    builder.register_custom_mutation::<AdministrationMutations>();
    builder.register_custom_output::<wire::AdministrationPrincipal>();
    builder.register_custom_output::<wire::AdministrationMembership>();
    builder.register_custom_output::<wire::ProjectBudgetPolicy>();
    builder.register_custom_output::<wire::ProjectBudgetStatus>();
    builder.register_custom_output::<wire::ApprovalPolicyRule>();
    builder.register_custom_output::<wire::ProjectApprovalPolicyVersion>();
    builder.register_custom_output::<wire::ProjectApprovalPolicy>();
    builder.register_custom_output::<wire::ProjectSettingsConnection>();
    builder.register_custom_output::<OrganizationAdministration>();
    builder.register_custom_output::<ProjectAdministration>();
    builder.outputs.push(
        AdministrationNotFoundProblem::basic_object(context())
            .implement(ADMINISTRATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        AdministrationAuthorizationProblem::basic_object(context())
            .implement(ADMINISTRATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        AdministrationValidationProblem::basic_object(context())
            .implement(ADMINISTRATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        AdministrationLifecycleProblem::basic_object(context())
            .implement(ADMINISTRATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        AdministrationPolicyWeakeningProblem::basic_object(context())
            .implement(ADMINISTRATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        AdministrationRevisionConflict::basic_object(context())
            .implement(ADMINISTRATION_PROBLEM_INTERFACE),
    );
    builder.register_custom_output::<AdministrationMutationPayload>();
    builder.register_custom_input::<CreateProjectInput>();
    builder.register_custom_input::<AdministrationMembershipInput>();
    builder.register_custom_input::<ReplaceAdministrationMembershipInput>();
    builder.register_custom_input::<EndAdministrationMembershipInput>();
    builder.register_custom_input::<LifecycleAdministrationInput>();
    builder.register_custom_input::<UpdateProjectBudgetPolicyInput>();
    builder.register_custom_input::<wire::ApprovalPolicyCellInput>();
    builder
        .inputs
        .push(FixedApprovalPolicyMatrixInput::input_object(context()));
    builder.register_custom_input::<UpdateProjectApprovalPolicyInput>();
    builder.register_custom_input::<UpdateProjectGeneralInput>();
    builder.register_custom_input::<SaveProjectSettingsConnectionInput>();
}

#[cfg(test)]
mod tests {
    //! `FixedApprovalPolicyMatrixInput`'s hand-built `parse_value` is complex enough (a nested
    //! object accessor keyed by 9 literal strings) that the SDL-shape comparison against the
    //! frozen contract isn't enough proof — it shows the *declared* input shape matches, not that
    //! parsing actually reads each of the 9 SCREAMING_SNAKE_CASE keys into the right field. This
    //! executes a real throwaway schema and asserts on the parsed struct's actual field values.
    use super::wire::{ApprovalPolicyCellInput, FixedApprovalPolicyMatrixInput};
    use super::*;
    use async_graphql::dynamic::{Field, FieldFuture, InputValue, Object, Schema};
    use seaography::async_graphql;
    use std::sync::LazyLock;

    static CONTEXT: LazyLock<BuilderContext> = LazyLock::new(BuilderContext::default);

    fn schema() -> Schema {
        let query = Object::new("Query").field(
            Field::new(
                "developmentLowApprovers",
                TypeRef::named_nn(TypeRef::INT),
                |ctx| {
                    FieldFuture::new(async move {
                        let matrix = FixedApprovalPolicyMatrixInput::parse_value(
                            &CONTEXT,
                            ctx.args.get("matrix"),
                        )?;
                        Ok(Some(async_graphql::Value::from(
                            matrix.development_low.requiredApprovers,
                        )))
                    })
                },
            )
            .argument(InputValue::new(
                "matrix",
                FixedApprovalPolicyMatrixInput::gql_input_type_ref(&CONTEXT),
            )),
        );
        Schema::build("Query", None, None)
            .register(query)
            .register(FixedApprovalPolicyMatrixInput::input_object(&CONTEXT))
            .register(ApprovalPolicyCellInput::input_object(&CONTEXT))
            .finish()
            .expect("the matrix smoke-test schema composes")
    }

    #[tokio::test]
    async fn each_of_the_nine_screaming_snake_case_keys_parses_into_its_own_field() {
        let cell = |approvers: i32| {
            format!(r#"{{ requiredEvidence: [], requiredApprovers: {approvers} }}"#)
        };
        let query = format!(
            r#"{{ developmentLowApprovers(matrix: {{
                DEVELOPMENT_LOW: {}, DEVELOPMENT_MEDIUM: {}, DEVELOPMENT_HIGH: {},
                STAGING_LOW: {}, STAGING_MEDIUM: {}, STAGING_HIGH: {},
                PRODUCTION_LOW: {}, PRODUCTION_MEDIUM: {}, PRODUCTION_HIGH: {}
            }}) }}"#,
            cell(1),
            cell(2),
            cell(3),
            cell(4),
            cell(5),
            cell(6),
            cell(7),
            cell(8),
            cell(9),
        );
        let response = schema().execute(query).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"developmentLowApprovers": 1})
        );
    }

    #[tokio::test]
    async fn a_missing_key_is_rejected_rather_than_silently_defaulted() {
        let response = schema()
            .execute(r#"{ developmentLowApprovers(matrix: { DEVELOPMENT_LOW: { requiredEvidence: [], requiredApprovers: 1 } }) }"#)
            .await;
        assert!(!response.errors.is_empty());
    }
}
