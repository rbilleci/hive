//! The evaluation *command* tier: the eight mutations, their inputs and the one payload shape
//! they answer with. Every evaluation read is a generated Seaography entity query
//! (`schema/mod.rs`) over the evaluation entities plus their computed fields
//! (`hive_persistence::evaluation::computed`).
//!
//! A payload returns the stored rows themselves: `evaluation_definitions::Model`,
//! `evaluation_definition_versions::Model` and `evaluation_runs::Model` are Seaography output
//! types as they are, resolving to the generated `EvaluationDefinitions`,
//! `EvaluationDefinitionVersions` and `EvaluationRuns` objects with their relations and computed
//! fields. Refusals are listed with the shared `Problem` type (`schema/problem.rs`); its `code` is
//! the stable, machine-readable reason.
//!
//! `#[derive(CustomEnum)]` only builds an enum's own `to_enum()` type definition (for
//! `register_custom_enum`, mirroring `register_custom_output`/`register_custom_input`) — no
//! blanket bridges it to `CustomOutputType`/`CustomInputType`, the two traits a struct field or a
//! resolver argument/return type actually needs. Each enum here pairs `#[derive(CustomEnum)]`
//! with `scalars::wire_enum!`, which hand-rolls both. Every variant is named in full
//! SCREAMING_SNAKE_CASE (`AGENT_VERSION`, not `AgentVersion`) since `#[derive(CustomEnum)]` uses
//! the Rust identifier verbatim as the wire value (`GSR-WIRE-CASE`).

use crate::schema::problem::Problem;
use crate::schema::scalars::{wire_enum, Id, Long};
use crate::schema::RequestPrincipal;
use hive_application::evaluation::{
    EvaluationMutationResult as AppMutationResult, EvaluationProblem as AppProblem,
    EvaluationProblemKind as AppProblemKind, EvaluationService,
};
use hive_persistence::entity::{
    evaluation_definition_versions, evaluation_definitions, evaluation_runs,
};
use hive_persistence::evaluation::PgEvaluationRepository;
use seaography::{CustomFields, CustomInputType, CustomOutputType};
use uuid::Uuid;

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

/// What a command answers with: the rows it left behind, exposed as the generated entity types.
type MutationResult = AppMutationResult<
    evaluation_definitions::Model,
    evaluation_definition_versions::Model,
    evaluation_runs::Model,
>;

#[allow(non_snake_case)]
mod wire {
    use super::*;

    // Both allows exist for the same reason across every enum in this file: the SCREAMING_
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
        pub(super) fn value(self) -> &'static str {
            match self {
                Self::AGENT_VERSION => "AGENT_VERSION",
                Self::DEPLOYMENT => "DEPLOYMENT",
            }
        }
    }

    // `EvaluationRunStatus` and `EvaluationOutcomeCategory` are the wire vocabulary of the
    // generated `EvaluationRuns` object's two text columns. They stay registered enums because
    // the console declares them as `cynic::Enum`s, which fails its build if the server adds or
    // removes a value — the check that keeps a status string from reaching a page as unknown text.
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

    impl From<AppProblem> for Problem {
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
            };
            if problem.kind == AppProblemKind::RevisionConflict {
                return Problem::revision_conflict(
                    &problem.message,
                    problem.resource_id.map(|id| id.to_string()),
                    problem.expected_revision,
                    problem.actual_revision,
                );
            }
            Problem::new(code, &problem.message)
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationMutationPayload {
        pub definition: Option<evaluation_definitions::Model>,
        pub version: Option<evaluation_definition_versions::Model>,
        pub run: Option<evaluation_runs::Model>,
        pub problems: Vec<Problem>,
    }

    impl From<MutationResult> for EvaluationMutationPayload {
        fn from(result: MutationResult) -> Self {
            Self {
                definition: result.definition,
                version: result.version,
                run: result.run,
                problems: result.problem.into_iter().map(Problem::from).collect(),
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

    // `reason` is accepted but never read by any resolver — a dead input field, kept for schema
    // parity rather than silently dropped.
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
    DuplicateEvaluationDefinitionVersionToDraftInput, EvaluationMutationPayload,
    EvaluationMutations, EvaluationOutcomeCategory, EvaluationRunStatus, EvaluationTargetKind,
    PublishEvaluationDefinitionDraftInput, RerunEvaluationInput, RunEvaluationInput,
    UpdateEvaluationDefinitionDraftInput, ValidateEvaluationDefinitionDraftInput,
};

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_enum::<EvaluationTargetKind>();
    builder.register_custom_enum::<EvaluationRunStatus>();
    builder.register_custom_enum::<EvaluationOutcomeCategory>();

    builder.register_custom_mutation::<EvaluationMutations>();
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
