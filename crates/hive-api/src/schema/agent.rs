//! Ports `AgentDraftResolver`/`AgentOperationalViewResolver` and the inline SDL types
//! `GraphqlSchemaFactory` declares for them: the five agent-draft/version read fields, the four
//! agent-draft write mutations, and the read-only `agentOperationalView` field. No field or
//! argument in this file carries a default (`GSR-DEFAULTS`), so every root operation goes through
//! `#[CustomFields]` directly — no hand-built `Field`s needed, unlike `organization.rs`/
//! `project.rs`/`audit.rs`.
//!
//! Second interface this port builds (`AgentDraftProblem`, 4 implementors), same pattern as
//! `console.rs`'s `DisplayPreferencesProblem`: each container-enum variant is named identically to
//! its inner type, since `#[derive(CustomOutputType)]` resolves a variant by its own identifier,
//! not the payload type's name (`console.rs`'s doc comment has the full trail).
//!
//! Several fields here are `Vec<String>` (`AgentDraftDiagnostic.path`, `AgentVersion.dependencies`,
//! `AgentDraftReview.dependencies`/`changedSections`, `AgentVersionComparison.changedSections`) —
//! all use `scalars::StringList`, not a bare `Vec<String>`, per the panic `audit.rs` found and
//! `scalars.rs::StringList`'s own doc comment explains in full.

use crate::schema::scalars::{Id, Json, StringList};
use crate::schema::RequestPrincipal;
use async_graphql::dynamic::{Interface, InterfaceField, TypeRef};
use hive_application::agent::{
    AgentDraft as AppAgentDraft, AgentDraftDiagnostic as AppAgentDraftDiagnostic,
    AgentDraftEditorService, AgentDraftMutationProblem as AppProblem,
    AgentDraftMutationResult as AppMutationResult, AgentDraftProblemKind as AppProblemKind,
    AgentDraftReview as AppAgentDraftReview, AgentOperationalView as AppOperationalView,
    AgentOperationalViewQueryService, AgentVersion as AppAgentVersion,
    AgentVersionComparison as AppAgentVersionComparison,
};
use hive_persistence::agent::{PgAgentDraftRepository, PgAgentOperationalViewRepository};
use seaography::{
    BuilderContext, CustomFields, CustomInputType, CustomOutputObject, CustomOutputType,
};
use uuid::Uuid;

pub const AGENT_DRAFT_PROBLEM_INTERFACE: &str = "AgentDraftProblem";

fn parsed_document(document: &str) -> Json {
    Json(
        serde_json::from_str(document)
            .expect("the stored agent draft document is always valid JSON"),
    )
}

fn timestamp(value: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    value.map(hive_domain::java_offset_date_time_string)
}

#[allow(non_snake_case)]
mod wire {
    use super::*;

    #[derive(CustomOutputType, Clone)]
    pub struct AgentDraftDiagnostic {
        pub code: String,
        pub severity: String,
        pub message: String,
        pub path: StringList,
    }

    impl From<AppAgentDraftDiagnostic> for AgentDraftDiagnostic {
        fn from(value: AppAgentDraftDiagnostic) -> Self {
            Self {
                code: value.code,
                severity: value.severity,
                message: value.message,
                path: value.path.into(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentDraft {
        pub id: Id,
        pub agentId: Id,
        pub slug: String,
        pub displayName: String,
        pub lifecycleStatus: String,
        pub document: Json,
        pub revision: i32,
        pub validationStatus: String,
        pub validationDiagnostics: Vec<AgentDraftDiagnostic>,
        pub validatedAt: Option<String>,
        pub canUpdate: bool,
        pub canPublish: bool,
        pub latestVersion: Option<i32>,
    }

    impl From<AppAgentDraft> for AgentDraft {
        fn from(value: AppAgentDraft) -> Self {
            Self {
                id: value.agent_id.to_string().into(),
                agentId: value.agent_id.to_string().into(),
                slug: value.slug,
                displayName: value.display_name,
                lifecycleStatus: value.lifecycle_status,
                document: parsed_document(&value.document),
                revision: value.revision as i32,
                validationStatus: value.validation_status,
                validationDiagnostics: value
                    .validation_diagnostics
                    .into_iter()
                    .map(AgentDraftDiagnostic::from)
                    .collect(),
                validatedAt: timestamp(value.validated_at),
                canUpdate: value.can_update,
                canPublish: value.can_publish,
                latestVersion: value.latest_version.map(|version| version as i32),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentVersion {
        pub id: Id,
        pub agentId: Id,
        pub number: i32,
        pub slug: String,
        pub displayName: String,
        pub canonicalDocument: Json,
        pub contentDigest: String,
        pub dependencies: StringList,
        pub catalogReleaseId: String,
        pub catalogReleaseDigest: String,
        pub publishedBy: Id,
        pub publishedAt: String,
    }

    impl From<AppAgentVersion> for AgentVersion {
        fn from(value: AppAgentVersion) -> Self {
            Self {
                id: value.id.to_string().into(),
                agentId: value.agent_id.to_string().into(),
                number: value.number as i32,
                slug: value.slug,
                displayName: value.display_name,
                canonicalDocument: parsed_document(&value.canonical_document),
                contentDigest: value.content_digest,
                dependencies: value.dependencies.into(),
                catalogReleaseId: value.catalog_release_id,
                catalogReleaseDigest: value.catalog_release_digest,
                publishedBy: value.published_by.to_string().into(),
                publishedAt: hive_domain::java_offset_date_time_string(value.published_at),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentVersionComparison {
        pub from: AgentVersion,
        pub to: AgentVersion,
        pub changedSections: StringList,
    }

    impl From<AppAgentVersionComparison> for AgentVersionComparison {
        fn from(value: AppAgentVersionComparison) -> Self {
            Self {
                from: value.from.into(),
                to: value.to.into(),
                changedSections: value.changed_sections.into(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentDraftReview {
        pub draft: AgentDraft,
        pub canonicalDocument: Json,
        pub contentDigest: String,
        pub dependencies: StringList,
        pub catalogReleaseId: String,
        pub catalogReleaseDigest: String,
        pub changedSections: StringList,
        pub diagnostics: Vec<AgentDraftDiagnostic>,
    }

    impl From<AppAgentDraftReview> for AgentDraftReview {
        fn from(value: AppAgentDraftReview) -> Self {
            Self {
                draft: value.draft.into(),
                canonicalDocument: parsed_document(&value.canonical_document),
                contentDigest: value.content_digest,
                dependencies: value.dependencies.into(),
                catalogReleaseId: value.catalog_release_id,
                catalogReleaseDigest: value.catalog_release_digest,
                changedSections: value.changed_sections.into(),
                diagnostics: value
                    .diagnostics
                    .into_iter()
                    .map(AgentDraftDiagnostic::from)
                    .collect(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentDraftNotFoundProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentDraftAuthorizationProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentDraftValidationProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentDraftRevisionConflict {
        pub code: String,
        pub message: String,
        pub resourceId: Id,
        pub expectedRevision: i32,
        pub actualRevision: i32,
    }

    // clippy::enum_variant_names is a false positive here: each variant's identifier is not a
    // discretionary name, it is the exact GraphQL type name `#[derive(CustomOutputType)]` resolves
    // it to at query time (`with_type(stringify!(variant))`, this file's own doc comment and
    // `console.rs`'s have the trail) — shortening it to `NotFound`/`Authorization`/etc. would
    // silently break interface resolution.
    #[derive(CustomOutputType, Clone)]
    #[allow(clippy::enum_variant_names)]
    pub enum AgentDraftProblem {
        AgentDraftNotFoundProblem(AgentDraftNotFoundProblem),
        AgentDraftAuthorizationProblem(AgentDraftAuthorizationProblem),
        AgentDraftValidationProblem(AgentDraftValidationProblem),
        AgentDraftRevisionConflict(AgentDraftRevisionConflict),
    }

    impl From<AppProblem> for AgentDraftProblem {
        fn from(problem: AppProblem) -> Self {
            match problem.kind {
                AppProblemKind::NotFound => {
                    AgentDraftProblem::AgentDraftNotFoundProblem(AgentDraftNotFoundProblem {
                        code: "NOT_FOUND".to_string(),
                        message: "This agent is unavailable.".to_string(),
                    })
                }
                AppProblemKind::Forbidden => AgentDraftProblem::AgentDraftAuthorizationProblem(
                    AgentDraftAuthorizationProblem {
                        code: "FORBIDDEN".to_string(),
                        message: "You do not have permission to edit this draft.".to_string(),
                    },
                ),
                AppProblemKind::RevisionConflict => {
                    AgentDraftProblem::AgentDraftRevisionConflict(AgentDraftRevisionConflict {
                        code: "REVISION_CONFLICT".to_string(),
                        message: "This draft changed after you opened it.".to_string(),
                        resourceId: problem
                            .resource_id
                            .map(|id| id.to_string())
                            .unwrap_or_default()
                            .into(),
                        expectedRevision: problem.expected_revision as i32,
                        actualRevision: problem.actual_revision as i32,
                    })
                }
                AppProblemKind::InvalidDocument => {
                    AgentDraftProblem::AgentDraftValidationProblem(AgentDraftValidationProblem {
                        code: "INVALID_DOCUMENT".to_string(),
                        message: "The draft document must be an object.".to_string(),
                    })
                }
                AppProblemKind::InvalidDraft => {
                    AgentDraftProblem::AgentDraftValidationProblem(AgentDraftValidationProblem {
                        code: "INVALID_DRAFT".to_string(),
                        message: "Resolve the server validation errors before publication."
                            .to_string(),
                    })
                }
                AppProblemKind::WarningAcknowledgementRequired => {
                    AgentDraftProblem::AgentDraftValidationProblem(AgentDraftValidationProblem {
                        code: "WARNING_ACKNOWLEDGEMENT_REQUIRED".to_string(),
                        message: "Review and acknowledge the server warnings before publication."
                            .to_string(),
                    })
                }
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentDraftMutationPayload {
        pub agentDraft: Option<AgentDraft>,
        pub agentVersion: Option<AgentVersion>,
        pub problems: Vec<AgentDraftProblem>,
    }

    impl From<AppMutationResult> for AgentDraftMutationPayload {
        fn from(result: AppMutationResult) -> Self {
            Self {
                agentDraft: result.agent_draft.map(AgentDraft::from),
                agentVersion: result.agent_version.map(AgentVersion::from),
                problems: result
                    .problem
                    .into_iter()
                    .map(AgentDraftProblem::from)
                    .collect(),
            }
        }
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "CreateAgentDraftInput")]
    pub struct CreateAgentDraftInput {
        pub projectId: Id,
        pub displayName: String,
        pub slug: Option<String>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "UpdateAgentDraftInput")]
    pub struct UpdateAgentDraftInput {
        pub projectId: Id,
        pub agentId: Id,
        pub expectedRevision: i32,
        pub document: Json,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "ValidateAgentDraftInput")]
    pub struct ValidateAgentDraftInput {
        pub projectId: Id,
        pub agentId: Id,
        pub expectedRevision: i32,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "PublishAgentDraftInput")]
    pub struct PublishAgentDraftInput {
        pub projectId: Id,
        pub agentId: Id,
        pub expectedRevision: i32,
        pub warningsAcknowledged: bool,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentDraftValidationSummary {
        pub status: String,
        pub errorCount: i32,
        pub warningCount: i32,
        pub validatedAt: Option<String>,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentPublishedVersionSummary {
        pub status: String,
        pub version: Option<String>,
        pub publishedAt: Option<String>,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentAliasTargetsSummary {
        pub totalCount: i32,
        pub activeCount: i32,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentDeploymentSummary {
        pub status: String,
        pub observedAt: Option<String>,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentEvaluationSummary {
        pub outcome: String,
        pub completedAt: Option<String>,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentRuntimeHealthSummary {
        pub status: String,
        pub observedAt: Option<String>,
        pub freshness: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct AgentOperationalView {
        pub id: Id,
        pub slug: String,
        pub displayName: String,
        pub lifecycleStatus: String,
        pub draftValidation: AgentDraftValidationSummary,
        pub latestPublishedVersion: AgentPublishedVersionSummary,
        pub aliasTargets: AgentAliasTargetsSummary,
        pub activeDeployment: AgentDeploymentSummary,
        pub recentEvaluation: AgentEvaluationSummary,
        pub runtimeHealth: AgentRuntimeHealthSummary,
    }

    impl From<AppOperationalView> for AgentOperationalView {
        fn from(value: AppOperationalView) -> Self {
            Self {
                id: value.id.to_string().into(),
                slug: value.slug,
                displayName: value.display_name,
                lifecycleStatus: value.lifecycle_status,
                draftValidation: AgentDraftValidationSummary {
                    status: value.draft_validation.status,
                    errorCount: value.draft_validation.error_count,
                    warningCount: value.draft_validation.warning_count,
                    validatedAt: timestamp(value.draft_validation.validated_at),
                },
                latestPublishedVersion: AgentPublishedVersionSummary {
                    status: value.latest_published_version.status,
                    version: value.latest_published_version.version,
                    publishedAt: timestamp(value.latest_published_version.published_at),
                },
                aliasTargets: AgentAliasTargetsSummary {
                    totalCount: value.alias_targets.total_count,
                    activeCount: value.alias_targets.active_count,
                },
                activeDeployment: AgentDeploymentSummary {
                    status: value.active_deployment.status,
                    observedAt: timestamp(value.active_deployment.observed_at),
                },
                recentEvaluation: AgentEvaluationSummary {
                    outcome: value.recent_evaluation.outcome,
                    completedAt: timestamp(value.recent_evaluation.completed_at),
                },
                runtimeHealth: AgentRuntimeHealthSummary {
                    status: value.runtime_health.status,
                    observedAt: timestamp(value.runtime_health.observed_at),
                    freshness: value.runtime_health.freshness,
                },
            }
        }
    }

    fn draft_service(
        ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<AgentDraftEditorService<PgAgentDraftRepository>> {
        let repository =
            PgAgentDraftRepository::new(ctx.data::<sea_orm::DatabaseConnection>()?.clone());
        Ok(AgentDraftEditorService::new(repository))
    }

    fn operational_view_service(
        ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<AgentOperationalViewQueryService<PgAgentOperationalViewRepository>>
    {
        let repository = PgAgentOperationalViewRepository::new(
            ctx.data::<sea_orm::DatabaseConnection>()?.clone(),
        );
        Ok(AgentOperationalViewQueryService::new(repository))
    }

    fn principal(ctx: &async_graphql::Context<'_>) -> async_graphql::Result<Uuid> {
        Ok(ctx.data::<RequestPrincipal>()?.0)
    }

    pub struct AgentQueries;

    #[CustomFields]
    impl AgentQueries {
        // Ports `AgentDraftResolver.resolveDraft`.
        async fn agentDraft(
            ctx: &async_graphql::Context<'_>,
            projectId: Id,
            agentId: Id,
        ) -> async_graphql::Result<Option<AgentDraft>> {
            let draft = draft_service(ctx)?
                .find_draft(principal(ctx)?, &projectId.0, &agentId.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(draft.map(AgentDraft::from))
        }

        // Ports `AgentDraftResolver.resolveReview`.
        async fn agentDraftReview(
            ctx: &async_graphql::Context<'_>,
            projectId: Id,
            agentId: Id,
        ) -> async_graphql::Result<Option<AgentDraftReview>> {
            let review = draft_service(ctx)?
                .review_draft(principal(ctx)?, &projectId.0, &agentId.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(review.map(AgentDraftReview::from))
        }

        // Ports `AgentDraftResolver.resolveComparison`.
        async fn compareAgentVersions(
            ctx: &async_graphql::Context<'_>,
            projectId: Id,
            agentId: Id,
            fromVersionId: Id,
            toVersionId: Id,
        ) -> async_graphql::Result<Option<AgentVersionComparison>> {
            let comparison = draft_service(ctx)?
                .compare_versions(
                    principal(ctx)?,
                    &projectId.0,
                    &agentId.0,
                    &fromVersionId.0,
                    &toVersionId.0,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(comparison.map(<AgentVersionComparison as From<AppAgentVersionComparison>>::from))
        }

        // Ports `AgentOperationalViewResolver.resolve`.
        async fn agentOperationalView(
            ctx: &async_graphql::Context<'_>,
            projectId: Id,
            agentId: Id,
        ) -> async_graphql::Result<Option<AgentOperationalView>> {
            let overview = operational_view_service(ctx)?
                .find_overview(principal(ctx)?, &projectId.0, &agentId.0)
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(overview.map(AgentOperationalView::from))
        }
    }

    pub struct AgentMutations;

    #[CustomFields]
    impl AgentMutations {
        // Ports `AgentDraftResolver.createDraft`.
        async fn createAgentDraft(
            ctx: &async_graphql::Context<'_>,
            input: CreateAgentDraftInput,
        ) -> async_graphql::Result<AgentDraftMutationPayload> {
            let result = draft_service(ctx)?
                .create_draft(
                    principal(ctx)?,
                    &input.projectId.0,
                    input.displayName,
                    input.slug,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AgentDraftMutationPayload::from(result))
        }

        // Ports `AgentDraftResolver.updateDraft`.
        async fn updateAgentDraft(
            ctx: &async_graphql::Context<'_>,
            input: UpdateAgentDraftInput,
        ) -> async_graphql::Result<AgentDraftMutationPayload> {
            let document = serde_json::to_string(&input.document.0)
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            let result = draft_service(ctx)?
                .update_draft(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.agentId.0,
                    input.expectedRevision as i64,
                    document,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AgentDraftMutationPayload::from(result))
        }

        // Ports `AgentDraftResolver.validateDraft`.
        async fn validateAgentDraft(
            ctx: &async_graphql::Context<'_>,
            input: ValidateAgentDraftInput,
        ) -> async_graphql::Result<AgentDraftMutationPayload> {
            let result = draft_service(ctx)?
                .validate_draft(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.agentId.0,
                    input.expectedRevision as i64,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AgentDraftMutationPayload::from(result))
        }

        // Ports `AgentDraftResolver.publishDraft`.
        async fn publishAgentDraft(
            ctx: &async_graphql::Context<'_>,
            input: PublishAgentDraftInput,
        ) -> async_graphql::Result<AgentDraftMutationPayload> {
            let result = draft_service(ctx)?
                .publish_draft(
                    principal(ctx)?,
                    &input.projectId.0,
                    &input.agentId.0,
                    input.expectedRevision as i64,
                    input.warningsAcknowledged,
                )
                .await
                .map_err(|error| async_graphql::Error::new(error.to_string()))?;
            Ok(AgentDraftMutationPayload::from(result))
        }
    }
}

pub use wire::{
    AgentAliasTargetsSummary, AgentDeploymentSummary, AgentDraft, AgentDraftAuthorizationProblem,
    AgentDraftDiagnostic, AgentDraftMutationPayload, AgentDraftNotFoundProblem, AgentDraftReview,
    AgentDraftRevisionConflict, AgentDraftValidationProblem, AgentDraftValidationSummary,
    AgentEvaluationSummary, AgentMutations, AgentOperationalView, AgentPublishedVersionSummary,
    AgentQueries, AgentRuntimeHealthSummary, AgentVersion, AgentVersionComparison,
    CreateAgentDraftInput, PublishAgentDraftInput, UpdateAgentDraftInput, ValidateAgentDraftInput,
};

/// This module's `Interface`, registered directly on the `SchemaBuilder` in `mod.rs::build()`
/// (`Builder` itself has no interface vector to push onto — same as `console.rs`).
pub fn interfaces() -> Vec<Interface> {
    vec![Interface::new(AGENT_DRAFT_PROBLEM_INTERFACE)
        .field(InterfaceField::new(
            "code",
            TypeRef::named_nn(TypeRef::STRING),
        ))
        .field(InterfaceField::new(
            "message",
            TypeRef::named_nn(TypeRef::STRING),
        ))]
}

fn context() -> &'static BuilderContext {
    crate::schema::context()
}

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_query::<AgentQueries>();
    builder.register_custom_mutation::<AgentMutations>();
    builder.register_custom_output::<AgentDraftDiagnostic>();
    builder.register_custom_output::<AgentDraft>();
    builder.register_custom_output::<AgentVersion>();
    builder.register_custom_output::<AgentVersionComparison>();
    builder.register_custom_output::<AgentDraftReview>();
    builder.outputs.push(
        AgentDraftNotFoundProblem::basic_object(context()).implement(AGENT_DRAFT_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        AgentDraftAuthorizationProblem::basic_object(context())
            .implement(AGENT_DRAFT_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        AgentDraftValidationProblem::basic_object(context())
            .implement(AGENT_DRAFT_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        AgentDraftRevisionConflict::basic_object(context())
            .implement(AGENT_DRAFT_PROBLEM_INTERFACE),
    );
    builder.register_custom_output::<AgentDraftMutationPayload>();
    builder.register_custom_input::<CreateAgentDraftInput>();
    builder.register_custom_input::<UpdateAgentDraftInput>();
    builder.register_custom_input::<ValidateAgentDraftInput>();
    builder.register_custom_input::<PublishAgentDraftInput>();
    builder.register_custom_output::<AgentDraftValidationSummary>();
    builder.register_custom_output::<AgentPublishedVersionSummary>();
    builder.register_custom_output::<AgentAliasTargetsSummary>();
    builder.register_custom_output::<AgentDeploymentSummary>();
    builder.register_custom_output::<AgentEvaluationSummary>();
    builder.register_custom_output::<AgentRuntimeHealthSummary>();
    builder.register_custom_output::<AgentOperationalView>();
}
