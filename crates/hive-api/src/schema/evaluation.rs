//! The evaluation *command* tier: 8 mutations, their inputs, the `EvaluationProblem` interface
//! and the payload shapes they answer with. Every evaluation read is a generated Seaography
//! entity query (`schema/mod.rs`) over the evaluation entities plus their computed fields
//! (`hive_persistence::evaluation::computed`); the hand-built queries, their connection types and
//! their wire projections are gone.
//!
//! `#[derive(CustomEnum)]` only builds an enum's own `to_enum()` type definition (for
//! `register_custom_enum`, mirroring `register_custom_output`/`register_custom_input`) — no
//! blanket bridges it to `CustomOutputType`/`CustomInputType`, the two traits a struct field or a
//! resolver argument/return type actually needs. Each enum here pairs `#[derive(CustomEnum)]`
//! with `scalars::wire_enum!`, which hand-rolls both. Every variant is named in full
//! SCREAMING_SNAKE_CASE (`AGENT_VERSION`, not `AgentVersion`) since `#[derive(CustomEnum)]` uses
//! the Rust identifier verbatim as the wire value (`GSR-WIRE-CASE`).

use crate::schema::scalars::{wire_enum, Id, Long, StringList};
use crate::schema::RequestPrincipal;
use async_graphql::dynamic::TypeRef;
use hive_application::evaluation::document::EvaluationDiagnostic as AppDiagnostic;
use hive_application::evaluation::{
    EvaluationDefinition as AppDefinition, EvaluationDefinitionDraft as AppDraft,
    EvaluationDefinitionVersion as AppVersion, EvaluationMutationResult as AppMutationResult,
    EvaluationProblem as AppProblem, EvaluationProblemKind as AppProblemKind,
    EvaluationRun as AppRun, EvaluationRunStatus as AppRunStatus, EvaluationService,
    EvaluationTargetSnapshot as AppTargetSnapshot,
};
use hive_persistence::evaluation::PgEvaluationRepository;
use seaography::{
    BuilderContext, CustomFields, CustomInputType, CustomOutputObject, CustomOutputType,
};
use uuid::Uuid;

pub const EVALUATION_PROBLEM_INTERFACE: &str = "EvaluationProblem";

fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    hive_domain::java_offset_date_time_string(value)
}

fn optional_timestamp(value: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    value.map(hive_domain::java_offset_date_time_string)
}

fn evaluation_service(
    ctx: &async_graphql::Context<'_>,
) -> async_graphql::Result<EvaluationService<PgEvaluationRepository>> {
    let repository =
        PgEvaluationRepository::new(ctx.data::<sea_orm::DatabaseConnection>()?.clone());
    Ok(EvaluationService::new(repository))
}

fn principal(ctx: &async_graphql::Context<'_>) -> async_graphql::Result<Uuid> {
    Ok(ctx.data::<RequestPrincipal>()?.0)
}

fn map_error(error: impl std::fmt::Display) -> async_graphql::Error {
    async_graphql::Error::new(error.to_string())
}

#[allow(non_snake_case)]
mod wire {
    use super::*;

    // Both allows exist for the same reason across all three enums in this file: the SCREAMING_
    // SNAKE_CASE spelling is the wire value itself (`GSR-WIRE-CASE`), not a stylistic lapse or a
    // real multi-word acronym clippy's heuristic is built for.
    #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
    #[derive(seaography::CustomEnum, Clone, Copy, Eq, PartialEq)]
    pub enum EvaluationTargetKind {
        AGENT_VERSION,
        DEPLOYMENT,
    }
    wire_enum!(EvaluationTargetKind {
        AGENT_VERSION,
        DEPLOYMENT
    });

    impl EvaluationTargetKind {
        fn parse(value: &str) -> Self {
            match value {
                "AGENT_VERSION" => Self::AGENT_VERSION,
                "DEPLOYMENT" => Self::DEPLOYMENT,
                other => panic!("unrecognized evaluation target kind `{other}`"),
            }
        }

        fn value(self) -> &'static str {
            match self {
                Self::AGENT_VERSION => "AGENT_VERSION",
                Self::DEPLOYMENT => "DEPLOYMENT",
            }
        }
    }

    #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
    #[derive(seaography::CustomEnum, Clone, Copy, Eq, PartialEq)]
    pub enum EvaluationRunStatus {
        QUEUED,
        RUNNING,
        COMPLETED,
        FAILED,
        CANCELED,
    }
    wire_enum!(EvaluationRunStatus {
        QUEUED,
        RUNNING,
        COMPLETED,
        FAILED,
        CANCELED
    });

    impl From<AppRunStatus> for EvaluationRunStatus {
        fn from(value: AppRunStatus) -> Self {
            match value {
                AppRunStatus::Queued => Self::QUEUED,
                AppRunStatus::Running => Self::RUNNING,
                AppRunStatus::Completed => Self::COMPLETED,
                AppRunStatus::Failed => Self::FAILED,
                AppRunStatus::Canceled => Self::CANCELED,
            }
        }
    }

    impl From<EvaluationRunStatus> for AppRunStatus {
        fn from(value: EvaluationRunStatus) -> Self {
            match value {
                EvaluationRunStatus::QUEUED => Self::Queued,
                EvaluationRunStatus::RUNNING => Self::Running,
                EvaluationRunStatus::COMPLETED => Self::Completed,
                EvaluationRunStatus::FAILED => Self::Failed,
                EvaluationRunStatus::CANCELED => Self::Canceled,
            }
        }
    }

    #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
    #[derive(seaography::CustomEnum, Clone, Copy, Eq, PartialEq)]
    pub enum EvaluationOutcomeCategory {
        PASSED,
        CASE_FAILED,
        TARGET_FAILED,
        RUNNER_FAILED,
        CANCELED,
    }
    wire_enum!(EvaluationOutcomeCategory {
        PASSED,
        CASE_FAILED,
        TARGET_FAILED,
        RUNNER_FAILED,
        CANCELED,
    });

    impl EvaluationOutcomeCategory {
        fn parse(value: &str) -> Self {
            match value {
                "PASSED" => Self::PASSED,
                "CASE_FAILED" => Self::CASE_FAILED,
                "TARGET_FAILED" => Self::TARGET_FAILED,
                "RUNNER_FAILED" => Self::RUNNER_FAILED,
                "CANCELED" => Self::CANCELED,
                other => panic!("unrecognized evaluation outcome category `{other}`"),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationDiagnostic {
        pub code: String,
        pub severity: String,
        pub message: String,
        pub path: StringList,
    }

    impl From<&AppDiagnostic> for EvaluationDiagnostic {
        fn from(value: &AppDiagnostic) -> Self {
            Self {
                code: value.code.clone(),
                severity: value.severity.clone(),
                message: value.message.clone(),
                path: value.path.clone().into(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationDefinitionDraft {
        pub definitionId: Id,
        pub canonicalDocument: String,
        pub revision: Long,
        pub validationStatus: String,
        pub diagnostics: Vec<EvaluationDiagnostic>,
        pub basedOnVersionId: Option<Id>,
        pub updatedAt: Option<String>,
    }

    impl From<&AppDraft> for EvaluationDefinitionDraft {
        fn from(value: &AppDraft) -> Self {
            Self {
                definitionId: value.definition_id.to_string().into(),
                canonicalDocument: value.canonical_document.clone(),
                revision: Long(value.revision),
                validationStatus: value.validation_status.clone(),
                diagnostics: value
                    .diagnostics
                    .iter()
                    .map(EvaluationDiagnostic::from)
                    .collect(),
                basedOnVersionId: value.based_on_version_id.map(|id| id.to_string().into()),
                updatedAt: optional_timestamp(value.updated_at),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationDefinitionVersion {
        pub id: Id,
        pub definitionId: Id,
        pub number: Long,
        pub canonicalDocument: String,
        pub contentDigest: String,
        pub basedOnVersionId: Option<Id>,
        pub publishedBy: Id,
        pub publishedAt: String,
    }

    impl From<&AppVersion> for EvaluationDefinitionVersion {
        fn from(value: &AppVersion) -> Self {
            Self {
                id: value.id.to_string().into(),
                definitionId: value.definition_id.to_string().into(),
                number: Long(value.number),
                canonicalDocument: value.canonical_document.clone(),
                contentDigest: value.content_digest.clone(),
                basedOnVersionId: value.based_on_version_id.map(|id| id.to_string().into()),
                publishedBy: value.published_by.to_string().into(),
                publishedAt: timestamp(value.published_at),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationDefinition {
        pub id: Id,
        pub projectId: Id,
        pub slug: String,
        pub lifecycleStatus: String,
        pub draft: EvaluationDefinitionDraft,
        pub latestVersion: Option<EvaluationDefinitionVersion>,
        pub canAuthor: bool,
        pub canPublish: bool,
        pub createdAt: String,
    }

    impl From<&AppDefinition> for EvaluationDefinition {
        fn from(value: &AppDefinition) -> Self {
            Self {
                id: value.id.to_string().into(),
                projectId: value.project_id.to_string().into(),
                slug: value.slug.clone(),
                lifecycleStatus: value.lifecycle_status.clone(),
                draft: EvaluationDefinitionDraft::from(&value.draft),
                latestVersion: value
                    .latest_version
                    .as_ref()
                    .map(EvaluationDefinitionVersion::from),
                canAuthor: value.can_author,
                canPublish: value.can_publish,
                createdAt: timestamp(value.created_at),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationTargetSnapshot {
        pub agentVersionId: Id,
        pub deploymentId: Option<Id>,
        pub environmentDefinitionVersionId: Id,
        pub logicalEnvironmentClass: String,
        pub agentContentDigest: String,
        pub targetDigest: Option<String>,
        pub planDigest: Option<String>,
        pub packageDigest: Option<String>,
        pub bindingDigest: Option<String>,
        pub catalogReleaseId: String,
        pub catalogReleaseDigest: String,
        pub environmentContentDigest: String,
    }

    impl From<&AppTargetSnapshot> for EvaluationTargetSnapshot {
        fn from(value: &AppTargetSnapshot) -> Self {
            Self {
                agentVersionId: value.agent_version_id.to_string().into(),
                deploymentId: value.deployment_id.map(|id| id.to_string().into()),
                environmentDefinitionVersionId: value
                    .environment_definition_version_id
                    .to_string()
                    .into(),
                logicalEnvironmentClass: value.logical_environment_class.clone(),
                agentContentDigest: value.agent_content_digest.clone(),
                targetDigest: value.target_digest.clone(),
                planDigest: value.plan_digest.clone(),
                packageDigest: value.package_digest.clone(),
                bindingDigest: value.binding_digest.clone(),
                catalogReleaseId: value.catalog_release_id.clone(),
                catalogReleaseDigest: value.catalog_release_digest.clone(),
                environmentContentDigest: value.environment_content_digest.clone(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationRun {
        pub id: Id,
        pub projectId: Id,
        pub definitionVersionId: Id,
        pub targetKind: EvaluationTargetKind,
        pub targetId: Id,
        pub environmentDefinitionVersionId: Id,
        pub sourceRunId: Option<Id>,
        pub lifecycleStatus: EvaluationRunStatus,
        pub generation: Long,
        pub outcomeCategory: Option<EvaluationOutcomeCategory>,
        pub outcomeCode: Option<String>,
        pub createdAt: String,
        pub startedAt: Option<String>,
        pub completedAt: Option<String>,
        pub durationMillis: Option<Long>,
        pub failureSummary: Option<String>,
        pub target: Option<EvaluationTargetSnapshot>,
        pub deploymentEvidenceDisposition: String,
    }

    impl From<&AppRun> for EvaluationRun {
        fn from(value: &AppRun) -> Self {
            Self {
                id: value.id.to_string().into(),
                projectId: value.project_id.to_string().into(),
                definitionVersionId: value.definition_version_id.to_string().into(),
                targetKind: EvaluationTargetKind::parse(&value.target_kind),
                targetId: value.target_id.to_string().into(),
                environmentDefinitionVersionId: value
                    .environment_definition_version_id
                    .to_string()
                    .into(),
                sourceRunId: value.source_run_id.map(|id| id.to_string().into()),
                lifecycleStatus: value.lifecycle_status.into(),
                generation: Long(value.generation),
                outcomeCategory: value
                    .outcome_category
                    .as_deref()
                    .map(EvaluationOutcomeCategory::parse),
                outcomeCode: value.outcome_code.clone(),
                createdAt: timestamp(value.created_at),
                startedAt: optional_timestamp(value.started_at),
                completedAt: optional_timestamp(value.completed_at),
                durationMillis: value.duration_millis().map(Long),
                failureSummary: value.failure_summary(),
                target: value.target.as_ref().map(EvaluationTargetSnapshot::from),
                deploymentEvidenceDisposition: value.deployment_evidence_disposition.clone(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationNotFoundProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationAuthorizationProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationValidationProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationLifecycleProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationIdempotencyProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationTargetCompatibilityProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationUnavailableProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationRevisionConflict {
        pub code: String,
        pub message: String,
        pub resourceId: Option<Id>,
        pub expectedRevision: Option<Long>,
        pub actualRevision: Option<Long>,
    }

    // clippy::enum_variant_names is a false positive here — see `agent.rs`'s `AgentDraftProblem`
    // for the full rationale.
    #[derive(CustomOutputType, Clone)]
    #[allow(clippy::enum_variant_names)]
    pub enum EvaluationProblem {
        EvaluationNotFoundProblem(EvaluationNotFoundProblem),
        EvaluationAuthorizationProblem(EvaluationAuthorizationProblem),
        EvaluationValidationProblem(EvaluationValidationProblem),
        EvaluationLifecycleProblem(EvaluationLifecycleProblem),
        EvaluationIdempotencyProblem(EvaluationIdempotencyProblem),
        EvaluationTargetCompatibilityProblem(EvaluationTargetCompatibilityProblem),
        EvaluationUnavailableProblem(EvaluationUnavailableProblem),
        EvaluationRevisionConflict(EvaluationRevisionConflict),
    }

    impl From<AppProblem> for EvaluationProblem {
        fn from(problem: AppProblem) -> Self {
            let code = match problem.kind {
                AppProblemKind::NotFound => "NOT_FOUND",
                AppProblemKind::Forbidden => "FORBIDDEN",
                AppProblemKind::Validation => "VALIDATION",
                AppProblemKind::RevisionConflict => "REVISION_CONFLICT",
                AppProblemKind::LifecycleConflict => "LIFECYCLE_CONFLICT",
                AppProblemKind::IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
                AppProblemKind::TargetIncompatible => "TARGET_INCOMPATIBLE",
                AppProblemKind::Unavailable => "UNAVAILABLE",
            }
            .to_string();
            match problem.kind {
                AppProblemKind::NotFound => {
                    EvaluationProblem::EvaluationNotFoundProblem(EvaluationNotFoundProblem {
                        code,
                        message: problem.message,
                    })
                }
                AppProblemKind::Forbidden => EvaluationProblem::EvaluationAuthorizationProblem(
                    EvaluationAuthorizationProblem {
                        code,
                        message: problem.message,
                    },
                ),
                AppProblemKind::Validation => {
                    EvaluationProblem::EvaluationValidationProblem(EvaluationValidationProblem {
                        code,
                        message: problem.message,
                    })
                }
                AppProblemKind::RevisionConflict => {
                    EvaluationProblem::EvaluationRevisionConflict(EvaluationRevisionConflict {
                        code,
                        message: problem.message,
                        resourceId: problem.resource_id.map(|id| id.to_string().into()),
                        expectedRevision: (problem.expected_revision >= 0)
                            .then_some(Long(problem.expected_revision)),
                        actualRevision: (problem.actual_revision >= 0)
                            .then_some(Long(problem.actual_revision)),
                    })
                }
                AppProblemKind::LifecycleConflict => {
                    EvaluationProblem::EvaluationLifecycleProblem(EvaluationLifecycleProblem {
                        code,
                        message: problem.message,
                    })
                }
                AppProblemKind::IdempotencyConflict => {
                    EvaluationProblem::EvaluationIdempotencyProblem(EvaluationIdempotencyProblem {
                        code,
                        message: problem.message,
                    })
                }
                AppProblemKind::TargetIncompatible => {
                    EvaluationProblem::EvaluationTargetCompatibilityProblem(
                        EvaluationTargetCompatibilityProblem {
                            code,
                            message: problem.message,
                        },
                    )
                }
                AppProblemKind::Unavailable => {
                    EvaluationProblem::EvaluationUnavailableProblem(EvaluationUnavailableProblem {
                        code,
                        message: problem.message,
                    })
                }
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationMutationPayload {
        pub definition: Option<EvaluationDefinition>,
        pub version: Option<EvaluationDefinitionVersion>,
        pub run: Option<EvaluationRun>,
        pub problems: Vec<EvaluationProblem>,
    }

    impl From<AppMutationResult> for EvaluationMutationPayload {
        fn from(result: AppMutationResult) -> Self {
            Self {
                definition: result.definition.as_ref().map(EvaluationDefinition::from),
                version: result
                    .version
                    .as_ref()
                    .map(EvaluationDefinitionVersion::from),
                run: result.run.as_ref().map(EvaluationRun::from),
                problems: result
                    .problem
                    .into_iter()
                    .map(EvaluationProblem::from)
                    .collect(),
            }
        }
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "CreateEvaluationDefinitionInput")]
    pub struct CreateEvaluationDefinitionInput {
        pub projectId: Id,
        pub slug: String,
        pub document: Option<String>,
        pub idempotencyKey: String,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "UpdateEvaluationDefinitionDraftInput")]
    pub struct UpdateEvaluationDefinitionDraftInput {
        pub definitionId: Id,
        pub expectedRevision: Long,
        pub document: String,
        pub idempotencyKey: String,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "ValidateEvaluationDefinitionDraftInput")]
    pub struct ValidateEvaluationDefinitionDraftInput {
        pub definitionId: Id,
        pub expectedRevision: Long,
        pub idempotencyKey: String,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "DuplicateEvaluationDefinitionVersionToDraftInput")]
    pub struct DuplicateEvaluationDefinitionVersionToDraftInput {
        pub versionId: Id,
        pub expectedRevision: Long,
        pub idempotencyKey: String,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "PublishEvaluationDefinitionDraftInput")]
    pub struct PublishEvaluationDefinitionDraftInput {
        pub definitionId: Id,
        pub expectedRevision: Long,
        pub idempotencyKey: String,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "RunEvaluationInput")]
    pub struct RunEvaluationInput {
        pub projectId: Id,
        pub definitionVersionId: Id,
        pub targetKind: EvaluationTargetKind,
        pub targetId: Id,
        pub environmentDefinitionVersionId: Id,
        pub idempotencyKey: String,
    }

    // `reason` is accepted but never read by any Java resolver (`resolver.cancel()` never touches
    // `i.get("reason")`) — a dead input field, kept here for schema parity rather than silently
    // dropped from the port.
    #[derive(CustomInputType)]
    #[seaography(input_type_name = "CancelEvaluationInput")]
    pub struct CancelEvaluationInput {
        pub runId: Id,
        pub expectedGeneration: Long,
        pub idempotencyKey: String,
        #[allow(dead_code)]
        pub reason: Option<String>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "RerunEvaluationInput")]
    pub struct RerunEvaluationInput {
        pub runId: Id,
        pub idempotencyKey: String,
    }

    pub struct EvaluationMutations;

    #[CustomFields]
    impl EvaluationMutations {
        async fn createEvaluationDefinition(
            ctx: &async_graphql::Context<'_>,
            input: CreateEvaluationDefinitionInput,
        ) -> async_graphql::Result<EvaluationMutationPayload> {
            let result = evaluation_service(ctx)?
                .create_definition(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.slug,
                    input.document.as_deref(),
                    &input.idempotencyKey,
                )
                .await
                .map_err(map_error)?;
            Ok(EvaluationMutationPayload::from(result))
        }

        async fn updateEvaluationDefinitionDraft(
            ctx: &async_graphql::Context<'_>,
            input: UpdateEvaluationDefinitionDraftInput,
        ) -> async_graphql::Result<EvaluationMutationPayload> {
            let result = evaluation_service(ctx)?
                .update_draft(
                    principal(ctx)?,
                    &input.definitionId.0,
                    input.expectedRevision.0,
                    &input.document,
                    &input.idempotencyKey,
                )
                .await
                .map_err(map_error)?;
            Ok(EvaluationMutationPayload::from(result))
        }

        async fn validateEvaluationDefinitionDraft(
            ctx: &async_graphql::Context<'_>,
            input: ValidateEvaluationDefinitionDraftInput,
        ) -> async_graphql::Result<EvaluationMutationPayload> {
            let result = evaluation_service(ctx)?
                .validate_draft(
                    principal(ctx)?,
                    &input.definitionId.0,
                    input.expectedRevision.0,
                    &input.idempotencyKey,
                )
                .await
                .map_err(map_error)?;
            Ok(EvaluationMutationPayload::from(result))
        }

        async fn duplicateEvaluationDefinitionVersionToDraft(
            ctx: &async_graphql::Context<'_>,
            input: DuplicateEvaluationDefinitionVersionToDraftInput,
        ) -> async_graphql::Result<EvaluationMutationPayload> {
            let result = evaluation_service(ctx)?
                .duplicate_version(
                    principal(ctx)?,
                    &input.versionId.0,
                    input.expectedRevision.0,
                    &input.idempotencyKey,
                )
                .await
                .map_err(map_error)?;
            Ok(EvaluationMutationPayload::from(result))
        }

        async fn publishEvaluationDefinitionDraft(
            ctx: &async_graphql::Context<'_>,
            input: PublishEvaluationDefinitionDraftInput,
        ) -> async_graphql::Result<EvaluationMutationPayload> {
            let result = evaluation_service(ctx)?
                .publish_draft(
                    principal(ctx)?,
                    &input.definitionId.0,
                    input.expectedRevision.0,
                    &input.idempotencyKey,
                )
                .await
                .map_err(map_error)?;
            Ok(EvaluationMutationPayload::from(result))
        }

        async fn runEvaluation(
            ctx: &async_graphql::Context<'_>,
            input: RunEvaluationInput,
        ) -> async_graphql::Result<EvaluationMutationPayload> {
            let result = evaluation_service(ctx)?
                .run_evaluation(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.definitionVersionId.0,
                    input.targetKind.value(),
                    &input.targetId.0,
                    &input.environmentDefinitionVersionId.0,
                    &input.idempotencyKey,
                )
                .await
                .map_err(map_error)?;
            Ok(EvaluationMutationPayload::from(result))
        }

        async fn cancelEvaluation(
            ctx: &async_graphql::Context<'_>,
            input: CancelEvaluationInput,
        ) -> async_graphql::Result<EvaluationMutationPayload> {
            let result = evaluation_service(ctx)?
                .cancel(
                    principal(ctx)?,
                    &input.runId.0,
                    input.expectedGeneration.0,
                    &input.idempotencyKey,
                )
                .await
                .map_err(map_error)?;
            Ok(EvaluationMutationPayload::from(result))
        }

        async fn rerunEvaluation(
            ctx: &async_graphql::Context<'_>,
            input: RerunEvaluationInput,
        ) -> async_graphql::Result<EvaluationMutationPayload> {
            let result = evaluation_service(ctx)?
                .rerun(principal(ctx)?, &input.runId.0, &input.idempotencyKey)
                .await
                .map_err(map_error)?;
            Ok(EvaluationMutationPayload::from(result))
        }
    }
}

pub use wire::{
    CancelEvaluationInput, CreateEvaluationDefinitionInput,
    DuplicateEvaluationDefinitionVersionToDraftInput, EvaluationAuthorizationProblem,
    EvaluationDefinition, EvaluationDefinitionDraft, EvaluationDefinitionVersion,
    EvaluationDiagnostic, EvaluationIdempotencyProblem, EvaluationLifecycleProblem,
    EvaluationMutationPayload, EvaluationMutations, EvaluationNotFoundProblem,
    EvaluationOutcomeCategory, EvaluationRevisionConflict, EvaluationRun, EvaluationRunStatus,
    EvaluationTargetCompatibilityProblem, EvaluationTargetKind, EvaluationTargetSnapshot,
    EvaluationUnavailableProblem, EvaluationValidationProblem,
    PublishEvaluationDefinitionDraftInput, RerunEvaluationInput, RunEvaluationInput,
    UpdateEvaluationDefinitionDraftInput, ValidateEvaluationDefinitionDraftInput,
};
fn context() -> &'static BuilderContext {
    crate::schema::context()
}

/// This module's `Interface`, registered directly on the `SchemaBuilder` in `mod.rs::build()`
/// (`Builder` itself has no interface vector to push onto — same as every other interface module).
pub fn interfaces() -> Vec<async_graphql::dynamic::Interface> {
    use async_graphql::dynamic::{Interface, InterfaceField};
    vec![Interface::new(EVALUATION_PROBLEM_INTERFACE)
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
    builder.register_custom_enum::<EvaluationTargetKind>();
    builder.register_custom_enum::<EvaluationRunStatus>();
    builder.register_custom_enum::<EvaluationOutcomeCategory>();

    builder.register_custom_mutation::<EvaluationMutations>();

    // The evaluation *reads* are generated entity queries (`schema/mod.rs`); what is left here is
    // the command tier, whose payload still carries these hand-built shapes until the mutation
    // slice ports them.
    builder.register_custom_output::<EvaluationDiagnostic>();
    builder.register_custom_output::<EvaluationDefinitionDraft>();
    builder.register_custom_output::<EvaluationDefinitionVersion>();
    builder.register_custom_output::<EvaluationDefinition>();
    builder.register_custom_output::<EvaluationTargetSnapshot>();
    builder.register_custom_output::<EvaluationRun>();

    builder.outputs.push(
        EvaluationNotFoundProblem::basic_object(context()).implement(EVALUATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        EvaluationAuthorizationProblem::basic_object(context())
            .implement(EVALUATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        EvaluationValidationProblem::basic_object(context())
            .implement(EVALUATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        EvaluationLifecycleProblem::basic_object(context()).implement(EVALUATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        EvaluationIdempotencyProblem::basic_object(context())
            .implement(EVALUATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        EvaluationTargetCompatibilityProblem::basic_object(context())
            .implement(EVALUATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        EvaluationUnavailableProblem::basic_object(context())
            .implement(EVALUATION_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        EvaluationRevisionConflict::basic_object(context()).implement(EVALUATION_PROBLEM_INTERFACE),
    );
    builder.register_custom_output::<EvaluationMutationPayload>();

    // The return types of the generated objects' computed fields
    // (`hive_persistence::evaluation::computed`): a `#[CustomFields]` method only builds the
    // field, never the object its value is.
    builder.register_custom_output::<hive_persistence::evaluation::computed::EvaluationDraftDiagnostic>();
    builder.register_custom_output::<hive_persistence::evaluation::computed::EvaluationDefinitionVersionComparison>();

    builder.register_custom_input::<CreateEvaluationDefinitionInput>();
    builder.register_custom_input::<UpdateEvaluationDefinitionDraftInput>();
    builder.register_custom_input::<ValidateEvaluationDefinitionDraftInput>();
    builder.register_custom_input::<DuplicateEvaluationDefinitionVersionToDraftInput>();
    builder.register_custom_input::<PublishEvaluationDefinitionDraftInput>();
    builder.register_custom_input::<RunEvaluationInput>();
    builder.register_custom_input::<CancelEvaluationInput>();
    builder.register_custom_input::<RerunEvaluationInput>();
}
