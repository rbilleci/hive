//! The ten administration commands. Memberships with their roles, the budget and approval
//! policies with their versions, and settings connections are generated reads
//! (`organizationMemberships`, `projectMemberships`, `projectBudgetPolicies`,
//! `projectApprovalPolicies`, `projectSettingsConnections`, and the relations from `Organizations`
//! and `Projects`) with the computed fields in `hive_persistence::administration::computed`.
//!
//! A payload returns the stored row itself: `organizations::Model` and `projects::Model` are
//! Seaography output types as they are, resolving to the generated `Organizations` and `Projects`
//! objects with their relations and computed fields.
//!
//! The approval policy matrix is a list of cells, `[{ cell, requiredEvidence, requiredApprovers }]`,
//! the same shape `ProjectApprovalPolicyVersions.rules` reads. It must name each of the nine
//! cells exactly once.

use crate::schema::problem::Problem;
use crate::schema::scalars::Id;
use crate::schema::RequestPrincipal;
use hive_application::administration::{
    AdministrationProblem as AppProblem, AdministrationProblemKind as AppProblemKind,
    AdministrationService, ApprovalRule as AppApprovalRule,
};
use hive_persistence::administration::computed::{ApprovalPolicyRule, ProjectBudgetStatus};
use hive_persistence::administration::{MutationResult, PgAdministrationRepository};
use hive_persistence::entity::{organizations, projects};
use seaography::{CustomFields, CustomInputType, CustomOutputType};
use uuid::Uuid;

#[allow(non_snake_case)]
mod wire {
    use super::*;

    impl From<AppProblem> for Problem {
        fn from(problem: AppProblem) -> Self {
            const UNAVAILABLE: &str = "This administration resource is unavailable.";
            match problem.kind {
                AppProblemKind::NotFound => Problem::new("NOT_FOUND", UNAVAILABLE),
                AppProblemKind::Forbidden => Problem::new("FORBIDDEN", UNAVAILABLE),
                AppProblemKind::InvalidInput => Problem::new(
                    "INVALID_INPUT",
                    "The submitted administration values are not supported.",
                ),
                AppProblemKind::ProtectedLifecycle => Problem::new(
                    "PROTECTED_LIFECYCLE",
                    "This lifecycle transition is unavailable.",
                ),
                AppProblemKind::PolicyWeakening => Problem::new(
                    "POLICY_WEAKENING",
                    "The fixed local approval policy can only become stronger.",
                ),
                AppProblemKind::RevisionConflict => Problem::revision_conflict(
                    "This resource changed after you opened it.",
                    problem.resource_id,
                    problem.expected_revision,
                    problem.actual_revision,
                ),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AdministrationMutationPayload {
        pub organization: Option<organizations::Model>,
        pub project: Option<projects::Model>,
        pub problems: Vec<Problem>,
    }

    impl From<MutationResult> for AdministrationMutationPayload {
        fn from(result: MutationResult) -> Self {
            Self {
                organization: result.organization,
                project: result.project,
                problems: result.problem.into_iter().map(Problem::from).collect(),
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
        pub roleCodes: Vec<String>,
        pub expectedScopeRevision: i32,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "ReplaceAdministrationMembershipInput")]
    pub struct ReplaceAdministrationMembershipInput {
        pub scope: String,
        pub scopeId: Id,
        pub membershipId: Id,
        pub roleCodes: Vec<String>,
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

    /// One cell of the approval policy matrix.
    #[derive(CustomInputType, Clone)]
    #[seaography(input_type_name = "ApprovalPolicyRuleInput")]
    pub struct ApprovalPolicyRuleInput {
        /// `DEVELOPMENT_LOW` … `PRODUCTION_HIGH`.
        pub cell: String,
        pub requiredEvidence: Vec<String>,
        pub requiredApprovers: i32,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "UpdateProjectApprovalPolicyInput")]
    pub struct UpdateProjectApprovalPolicyInput {
        pub projectId: Id,
        pub expectedRevision: i32,
        pub matrix: Vec<ApprovalPolicyRuleInput>,
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

    pub struct AdministrationMutations;

    #[CustomFields]
    impl AdministrationMutations {
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
                    input.roleCodes,
                    input.expectedScopeRevision as i64,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }

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
                    input.roleCodes,
                    input.expectedRevision as i64,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AdministrationMutationPayload::from(result))
        }

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

        async fn updateProjectApprovalPolicy(
            ctx: &async_graphql::Context<'_>,
            input: UpdateProjectApprovalPolicyInput,
        ) -> async_graphql::Result<AdministrationMutationPayload> {
            let matrix = input
                .matrix
                .into_iter()
                .map(|rule| {
                    (
                        rule.cell,
                        AppApprovalRule {
                            required_evidence: rule.requiredEvidence,
                            required_approvers: rule.requiredApprovers,
                        },
                    )
                })
                .collect();
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
    AdministrationMembershipInput, AdministrationMutationPayload, AdministrationMutations,
    ApprovalPolicyRuleInput, CreateProjectInput, EndAdministrationMembershipInput,
    LifecycleAdministrationInput, ReplaceAdministrationMembershipInput,
    SaveProjectSettingsConnectionInput, UpdateProjectApprovalPolicyInput,
    UpdateProjectBudgetPolicyInput, UpdateProjectGeneralInput,
};

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_mutation::<AdministrationMutations>();
    builder.register_custom_output::<ApprovalPolicyRule>();
    builder.register_custom_output::<ProjectBudgetStatus>();
    builder.register_custom_output::<AdministrationMutationPayload>();
    builder.register_custom_input::<CreateProjectInput>();
    builder.register_custom_input::<AdministrationMembershipInput>();
    builder.register_custom_input::<ReplaceAdministrationMembershipInput>();
    builder.register_custom_input::<EndAdministrationMembershipInput>();
    builder.register_custom_input::<LifecycleAdministrationInput>();
    builder.register_custom_input::<UpdateProjectBudgetPolicyInput>();
    builder.register_custom_input::<ApprovalPolicyRuleInput>();
    builder.register_custom_input::<UpdateProjectApprovalPolicyInput>();
    builder.register_custom_input::<UpdateProjectGeneralInput>();
    builder.register_custom_input::<SaveProjectSettingsConnectionInput>();
}
