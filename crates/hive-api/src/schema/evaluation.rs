//! Ports `EvaluationGraphql`: 9 queries and 8 mutations. First file to use `scalars::Long`
//! (`revision`, `number`, `generation`, `byteLength`, `durationMillis`, `expectedRevision`,
//! `actualRevision`, `expectedGeneration`) and the first with real value-only GraphQL enums
//! (`EvaluationTargetKind`, `EvaluationRunStatus`, `EvaluationOutcomeCategory`).
//!
//! `#[derive(CustomEnum)]` only builds an enum's own `to_enum()` type definition (for
//! `register_custom_enum`, mirroring `register_custom_output`/`register_custom_input`) — no
//! blanket bridges it to `CustomOutputType`/`CustomInputType`, the two traits a struct field or a
//! resolver argument/return type actually needs (confirmed by reading `custom_enum.rs`). Each enum
//! here pairs `#[derive(CustomEnum)]` with `scalars::wire_enum!`, which hand-rolls both. Every
//! variant is named in full SCREAMING_SNAKE_CASE (`AGENT_VERSION`, not `AgentVersion`) since
//! `#[derive(CustomEnum)]` (like every other derive in this port) uses the Rust identifier
//! verbatim as the wire value (`GSR-WIRE-CASE`), and the frozen contract's enum values are
//! SCREAMING_SNAKE_CASE.
//!
//! Sixth interface this port builds (`EvaluationProblem`, 8 implementors — the most yet).
//!
//! `EvaluationRun` is complex (`GSR-NESTED-FIELDS`): `cases`/`metrics`/`artifacts`/`audit` are all
//! lazily-resolved nested connections with `(after: String, first: Int! = 50)`, hand-built and
//! folded onto the derived object the same way `Organization.projects` was, just four fields
//! instead of one. All 8 connection types here share one shape (`edges`, `hasNextPage: Boolean!`,
//! `endCursor: String` — flat fields, not a nested `PageInfo` object, matching `audit.rs`'s
//! `AuditPageInfo` precedent but inlined rather than wrapped), so a local `connection_type!` macro
//! generates the edge/connection struct pair and `from_app_connection!` generates its `From` impl,
//! directly mirroring the static tier's own two macros of the same names.
//!
//! `path`/`requiredEvidence`-shaped `Vec<String>` fields use `scalars::StringList`, per the panic
//! `audit.rs` found.

use crate::schema::scalars;
use crate::schema::scalars::{wire_enum, Id, Long, StringList};
use crate::schema::RequestPrincipal;
use async_graphql::dynamic::{Field, FieldFuture, InputValue, TypeRef};
use hive_application::evaluation::document::EvaluationDiagnostic as AppDiagnostic;
use hive_application::evaluation::{
    Connection as AppConnection, EvaluationArtifactMetadata as AppArtifact,
    EvaluationAuditEvent as AppAuditEvent, EvaluationCaseRun as AppCaseRun,
    EvaluationDefinition as AppDefinition, EvaluationDefinitionDraft as AppDraft,
    EvaluationDefinitionVersion as AppVersion, EvaluationMetricResult as AppMetric,
    EvaluationMutationResult as AppMutationResult, EvaluationProblem as AppProblem,
    EvaluationProblemKind as AppProblemKind, EvaluationRun as AppRun,
    EvaluationRunStatus as AppRunStatus, EvaluationService, EvaluationTarget as AppTarget,
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

fn after_argument() -> InputValue {
    InputValue::new("after", TypeRef::named(TypeRef::STRING))
}

fn first_argument() -> InputValue {
    InputValue::new("first", TypeRef::named_nn(TypeRef::INT)).default_value(50i32)
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
    pub struct EvaluationDefinitionVersionComparison {
        pub left: Option<EvaluationDefinitionVersion>,
        pub right: Option<EvaluationDefinitionVersion>,
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
    pub struct EvaluationTarget {
        pub kind: EvaluationTargetKind,
        pub id: Id,
        pub agentVersionId: Id,
        pub environmentDefinitionVersionId: Id,
        pub logicalEnvironmentClass: String,
        pub displayName: String,
    }

    impl From<&AppTarget> for EvaluationTarget {
        fn from(value: &AppTarget) -> Self {
            Self {
                kind: EvaluationTargetKind::parse(&value.kind),
                id: value.id.to_string().into(),
                agentVersionId: value.agent_version_id.to_string().into(),
                environmentDefinitionVersionId: value
                    .environment_definition_version_id
                    .to_string()
                    .into(),
                logicalEnvironmentClass: value.logical_environment_class.clone(),
                displayName: value.display_name.clone(),
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
    pub struct EvaluationCaseRun {
        pub id: Id,
        pub key: String,
        pub ordinal: i32,
        pub lifecycleStatus: String,
        pub passed: Option<bool>,
        pub failureCode: Option<String>,
        pub completedAt: Option<String>,
    }

    impl From<&AppCaseRun> for EvaluationCaseRun {
        fn from(value: &AppCaseRun) -> Self {
            Self {
                id: value.id.to_string().into(),
                key: value.key.clone(),
                ordinal: value.ordinal,
                lifecycleStatus: value.lifecycle_status.clone(),
                passed: value.passed,
                failureCode: value.failure_code.clone(),
                completedAt: optional_timestamp(value.completed_at),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationMetricResult {
        pub id: Id,
        pub code: String,
        pub value: f64,
        pub threshold: f64,
        pub passed: bool,
    }

    impl From<&AppMetric> for EvaluationMetricResult {
        fn from(value: &AppMetric) -> Self {
            Self {
                id: value.id.to_string().into(),
                code: value.code.clone(),
                value: value.value,
                threshold: value.threshold,
                passed: value.passed,
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationArtifactMetadata {
        pub id: Id,
        pub kind: String,
        pub contentDigest: String,
        pub mediaType: String,
        pub byteLength: Long,
    }

    impl From<&AppArtifact> for EvaluationArtifactMetadata {
        fn from(value: &AppArtifact) -> Self {
            Self {
                id: value.id.to_string().into(),
                kind: value.kind.clone(),
                contentDigest: value.content_digest.clone(),
                mediaType: value.media_type.clone(),
                byteLength: Long(value.byte_length),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct EvaluationAuditEvent {
        pub id: Id,
        pub action: String,
        pub occurredAt: String,
        pub summary: String,
    }

    impl From<&AppAuditEvent> for EvaluationAuditEvent {
        fn from(value: &AppAuditEvent) -> Self {
            Self {
                id: value.id.to_string().into(),
                action: value.action.clone(),
                occurredAt: timestamp(value.occurred_at),
                summary: value.summary.clone(),
            }
        }
    }

    // Every evaluation connection shares this exact shape (flat `hasNextPage`/`endCursor`, no
    // nested `PageInfo`) — mirrors the static tier's own `connection_type!`/`from_app_connection!`
    // macros of the same names.
    macro_rules! connection_type {
        ($connection_name:ident, $edge_name:ident, $node:ty) => {
            #[derive(CustomOutputType, Clone)]
            pub struct $edge_name {
                pub cursor: String,
                pub node: $node,
            }

            #[derive(CustomOutputType, Clone, Default)]
            pub struct $connection_name {
                pub edges: Vec<$edge_name>,
                pub hasNextPage: bool,
                pub endCursor: Option<String>,
            }
        };
    }

    macro_rules! from_app_connection {
        ($app_type:ty, $connection_name:ident, $edge_name:ident, $node_from:path) => {
            impl From<AppConnection<$app_type>> for $connection_name {
                fn from(value: AppConnection<$app_type>) -> Self {
                    Self {
                        edges: value
                            .edges
                            .iter()
                            .map(|edge| $edge_name {
                                cursor: edge.cursor.clone(),
                                node: $node_from(&edge.node),
                            })
                            .collect(),
                        hasNextPage: value.has_next_page,
                        endCursor: value.end_cursor,
                    }
                }
            }
        };
    }

    connection_type!(
        EvaluationTargetConnection,
        EvaluationTargetEdge,
        EvaluationTarget
    );
    from_app_connection!(
        AppTarget,
        EvaluationTargetConnection,
        EvaluationTargetEdge,
        EvaluationTarget::from
    );

    connection_type!(
        EvaluationCaseRunConnection,
        EvaluationCaseRunEdge,
        EvaluationCaseRun
    );
    from_app_connection!(
        AppCaseRun,
        EvaluationCaseRunConnection,
        EvaluationCaseRunEdge,
        EvaluationCaseRun::from
    );

    connection_type!(
        EvaluationMetricResultConnection,
        EvaluationMetricResultEdge,
        EvaluationMetricResult
    );
    from_app_connection!(
        AppMetric,
        EvaluationMetricResultConnection,
        EvaluationMetricResultEdge,
        EvaluationMetricResult::from
    );

    connection_type!(
        EvaluationArtifactMetadataConnection,
        EvaluationArtifactMetadataEdge,
        EvaluationArtifactMetadata
    );
    from_app_connection!(
        AppArtifact,
        EvaluationArtifactMetadataConnection,
        EvaluationArtifactMetadataEdge,
        EvaluationArtifactMetadata::from
    );

    connection_type!(
        EvaluationAuditEventConnection,
        EvaluationAuditEventEdge,
        EvaluationAuditEvent
    );
    from_app_connection!(
        AppAuditEvent,
        EvaluationAuditEventConnection,
        EvaluationAuditEventEdge,
        EvaluationAuditEvent::from
    );

    connection_type!(
        EvaluationDefinitionVersionConnection,
        EvaluationDefinitionVersionEdge,
        EvaluationDefinitionVersion
    );
    from_app_connection!(
        AppVersion,
        EvaluationDefinitionVersionConnection,
        EvaluationDefinitionVersionEdge,
        EvaluationDefinitionVersion::from
    );

    connection_type!(
        EvaluationDefinitionConnection,
        EvaluationDefinitionEdge,
        EvaluationDefinition
    );
    from_app_connection!(
        AppDefinition,
        EvaluationDefinitionConnection,
        EvaluationDefinitionEdge,
        EvaluationDefinition::from
    );

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

    connection_type!(EvaluationRunConnection, EvaluationRunEdge, EvaluationRun);
    from_app_connection!(
        AppRun,
        EvaluationRunConnection,
        EvaluationRunEdge,
        EvaluationRun::from
    );

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
    #[seaography(input_type_name = "EvaluationRunFilter")]
    pub struct EvaluationRunFilter {
        pub status: Option<EvaluationRunStatus>,
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

    pub struct EvaluationQueries;

    #[CustomFields]
    impl EvaluationQueries {
        async fn evaluationDefinition(
            ctx: &async_graphql::Context<'_>,
            definitionId: Id,
        ) -> async_graphql::Result<Option<EvaluationDefinition>> {
            let value = evaluation_service(ctx)?
                .definition(principal(ctx)?, &definitionId.0)
                .await
                .map_err(map_error)?;
            Ok(value.as_ref().map(EvaluationDefinition::from))
        }

        async fn evaluationDefinitionVersion(
            ctx: &async_graphql::Context<'_>,
            versionId: Id,
        ) -> async_graphql::Result<Option<EvaluationDefinitionVersion>> {
            let value = evaluation_service(ctx)?
                .definition_version(principal(ctx)?, &versionId.0)
                .await
                .map_err(map_error)?;
            Ok(value.as_ref().map(EvaluationDefinitionVersion::from))
        }

        async fn evaluationDefinitionVersionComparison(
            ctx: &async_graphql::Context<'_>,
            leftVersionId: Id,
            rightVersionId: Id,
        ) -> async_graphql::Result<Option<EvaluationDefinitionVersionComparison>> {
            let service = evaluation_service(ctx)?;
            let principal_id = principal(ctx)?;
            let left = service
                .definition_version(principal_id, &leftVersionId.0)
                .await
                .map_err(map_error)?;
            let right = service
                .definition_version(principal_id, &rightVersionId.0)
                .await
                .map_err(map_error)?;
            let (Some(left), Some(right)) = (left, right) else {
                return Ok(None);
            };
            if left.definition_id != right.definition_id {
                return Ok(None);
            }
            Ok(Some(EvaluationDefinitionVersionComparison {
                left: Some(EvaluationDefinitionVersion::from(&left)),
                right: Some(EvaluationDefinitionVersion::from(&right)),
            }))
        }

        async fn evaluationRun(
            ctx: &async_graphql::Context<'_>,
            runId: Id,
        ) -> async_graphql::Result<Option<EvaluationRun>> {
            let value = evaluation_service(ctx)?
                .run(principal(ctx)?, &runId.0)
                .await
                .map_err(map_error)?;
            Ok(value.as_ref().map(EvaluationRun::from))
        }
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
    DuplicateEvaluationDefinitionVersionToDraftInput, EvaluationArtifactMetadata,
    EvaluationArtifactMetadataConnection, EvaluationArtifactMetadataEdge, EvaluationAuditEvent,
    EvaluationAuditEventConnection, EvaluationAuditEventEdge, EvaluationAuthorizationProblem,
    EvaluationCaseRun, EvaluationCaseRunConnection, EvaluationCaseRunEdge, EvaluationDefinition,
    EvaluationDefinitionConnection, EvaluationDefinitionDraft, EvaluationDefinitionEdge,
    EvaluationDefinitionVersion, EvaluationDefinitionVersionComparison,
    EvaluationDefinitionVersionConnection, EvaluationDefinitionVersionEdge, EvaluationDiagnostic,
    EvaluationIdempotencyProblem, EvaluationLifecycleProblem, EvaluationMetricResult,
    EvaluationMetricResultConnection, EvaluationMetricResultEdge, EvaluationMutationPayload,
    EvaluationMutations, EvaluationNotFoundProblem, EvaluationOutcomeCategory, EvaluationQueries,
    EvaluationRevisionConflict, EvaluationRun, EvaluationRunConnection, EvaluationRunEdge,
    EvaluationRunFilter, EvaluationRunStatus, EvaluationTarget,
    EvaluationTargetCompatibilityProblem, EvaluationTargetConnection, EvaluationTargetEdge,
    EvaluationTargetKind, EvaluationTargetSnapshot, EvaluationUnavailableProblem,
    EvaluationValidationProblem, PublishEvaluationDefinitionDraftInput, RerunEvaluationInput,
    RunEvaluationInput, UpdateEvaluationDefinitionDraftInput,
    ValidateEvaluationDefinitionDraftInput,
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

macro_rules! nested_connection_field {
    ($name:literal, $connection_type:ident, $method:ident) => {
        Field::new(
            $name,
            TypeRef::named_nn(stringify!($connection_type)),
            |ctx| {
                FieldFuture::new(async move {
                    let run = ctx.parent_value.try_downcast_ref::<EvaluationRun>()?;
                    let after = scalars::optional_string(ctx.args.get("after"))?;
                    let first = ctx.args.try_get("first")?.i64()? as i32;
                    let principal = ctx.ctx.data::<RequestPrincipal>()?;
                    let service = evaluation_service(ctx.ctx)?;
                    let connection = service
                        .$method(principal.0, &run.id.0, after.as_deref(), first)
                        .await
                        .map_err(|error| async_graphql::Error::new(error.to_string()))?;
                    let connection: $connection_type =
                        connection.map($connection_type::from).unwrap_or_default();
                    Ok(connection.gql_field_value(context()))
                })
            },
        )
        .argument(after_argument())
        .argument(first_argument())
    };
}

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_enum::<EvaluationTargetKind>();
    builder.register_custom_enum::<EvaluationRunStatus>();
    builder.register_custom_enum::<EvaluationOutcomeCategory>();

    builder.register_custom_query::<EvaluationQueries>();
    builder.register_custom_mutation::<EvaluationMutations>();

    builder.register_custom_output::<EvaluationDiagnostic>();
    builder.register_custom_output::<EvaluationDefinitionDraft>();
    builder.register_custom_output::<EvaluationDefinitionVersion>();
    builder.register_custom_output::<EvaluationDefinitionVersionEdge>();
    builder.register_custom_output::<EvaluationDefinitionVersionConnection>();
    builder.register_custom_output::<EvaluationDefinitionVersionComparison>();
    builder.register_custom_output::<EvaluationDefinition>();
    builder.register_custom_output::<EvaluationDefinitionEdge>();
    builder.register_custom_output::<EvaluationDefinitionConnection>();
    builder.register_custom_output::<EvaluationTarget>();
    builder.register_custom_output::<EvaluationTargetSnapshot>();
    builder.register_custom_output::<EvaluationTargetEdge>();
    builder.register_custom_output::<EvaluationTargetConnection>();
    builder.register_custom_output::<EvaluationCaseRun>();
    builder.register_custom_output::<EvaluationCaseRunEdge>();
    builder.register_custom_output::<EvaluationCaseRunConnection>();
    builder.register_custom_output::<EvaluationMetricResult>();
    builder.register_custom_output::<EvaluationMetricResultEdge>();
    builder.register_custom_output::<EvaluationMetricResultConnection>();
    builder.register_custom_output::<EvaluationArtifactMetadata>();
    builder.register_custom_output::<EvaluationArtifactMetadataEdge>();
    builder.register_custom_output::<EvaluationArtifactMetadataConnection>();
    builder.register_custom_output::<EvaluationAuditEvent>();
    builder.register_custom_output::<EvaluationAuditEventEdge>();
    builder.register_custom_output::<EvaluationAuditEventConnection>();

    builder.outputs.push(
        EvaluationRun::basic_object(context())
            .field(nested_connection_field!(
                "cases",
                EvaluationCaseRunConnection,
                cases
            ))
            .field(nested_connection_field!(
                "metrics",
                EvaluationMetricResultConnection,
                metrics
            ))
            .field(nested_connection_field!(
                "artifacts",
                EvaluationArtifactMetadataConnection,
                artifacts
            ))
            .field(nested_connection_field!(
                "audit",
                EvaluationAuditEventConnection,
                audit
            )),
    );
    builder.register_custom_output::<EvaluationRunEdge>();
    builder.register_custom_output::<EvaluationRunConnection>();

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

    builder.register_custom_input::<EvaluationRunFilter>();
    builder.register_custom_input::<CreateEvaluationDefinitionInput>();
    builder.register_custom_input::<UpdateEvaluationDefinitionDraftInput>();
    builder.register_custom_input::<ValidateEvaluationDefinitionDraftInput>();
    builder.register_custom_input::<DuplicateEvaluationDefinitionVersionToDraftInput>();
    builder.register_custom_input::<PublishEvaluationDefinitionDraftInput>();
    builder.register_custom_input::<RunEvaluationInput>();
    builder.register_custom_input::<CancelEvaluationInput>();
    builder.register_custom_input::<RerunEvaluationInput>();

    // `evaluationDefinitions`/`evaluationDefinitionVersions`/`evaluationDefinitionVersionUsage`/
    // `evaluationRuns`/`evaluationTargets` all carry `first: Int! = 50` (`GSR-DEFAULTS`), so each
    // is hand-built like `Organization.accessibleOrganizations`, not `#[CustomFields]`.
    builder.queries.push(
        Field::new(
            "evaluationDefinitions",
            TypeRef::named("EvaluationDefinitionConnection"),
            |ctx| {
                FieldFuture::new(async move {
                    let project_id = ctx.args.try_get("projectId")?.string()?.to_string();
                    let after = scalars::optional_string(ctx.args.get("after"))?;
                    let first = ctx.args.try_get("first")?.i64()? as i32;
                    let principal = ctx.ctx.data::<RequestPrincipal>()?;
                    let connection = evaluation_service(ctx.ctx)?
                        .definitions(principal.0, &project_id, after.as_deref(), first)
                        .await
                        .map_err(|error| async_graphql::Error::new(error.to_string()))?;
                    Ok(connection
                        .map(EvaluationDefinitionConnection::from)
                        .and_then(|connection| connection.gql_field_value(context())))
                })
            },
        )
        .argument(InputValue::new("projectId", TypeRef::named_nn(TypeRef::ID)))
        .argument(after_argument())
        .argument(first_argument()),
    );
    builder.queries.push(
        Field::new(
            "evaluationDefinitionVersions",
            TypeRef::named("EvaluationDefinitionVersionConnection"),
            |ctx| {
                FieldFuture::new(async move {
                    let definition_id = ctx.args.try_get("definitionId")?.string()?.to_string();
                    let after = scalars::optional_string(ctx.args.get("after"))?;
                    let first = ctx.args.try_get("first")?.i64()? as i32;
                    let principal = ctx.ctx.data::<RequestPrincipal>()?;
                    let connection = evaluation_service(ctx.ctx)?
                        .definition_versions(principal.0, &definition_id, after.as_deref(), first)
                        .await
                        .map_err(|error| async_graphql::Error::new(error.to_string()))?;
                    Ok(connection
                        .map(EvaluationDefinitionVersionConnection::from)
                        .and_then(|connection| connection.gql_field_value(context())))
                })
            },
        )
        .argument(InputValue::new(
            "definitionId",
            TypeRef::named_nn(TypeRef::ID),
        ))
        .argument(after_argument())
        .argument(first_argument()),
    );
    builder.queries.push(
        Field::new(
            "evaluationDefinitionVersionUsage",
            TypeRef::named("EvaluationRunConnection"),
            |ctx| {
                FieldFuture::new(async move {
                    let version_id = ctx.args.try_get("versionId")?.string()?.to_string();
                    let after = scalars::optional_string(ctx.args.get("after"))?;
                    let first = ctx.args.try_get("first")?.i64()? as i32;
                    let principal = ctx.ctx.data::<RequestPrincipal>()?;
                    let connection = evaluation_service(ctx.ctx)?
                        .definition_version_usage(principal.0, &version_id, after.as_deref(), first)
                        .await
                        .map_err(|error| async_graphql::Error::new(error.to_string()))?;
                    Ok(connection
                        .map(EvaluationRunConnection::from)
                        .and_then(|connection| connection.gql_field_value(context())))
                })
            },
        )
        .argument(InputValue::new("versionId", TypeRef::named_nn(TypeRef::ID)))
        .argument(after_argument())
        .argument(first_argument()),
    );
    builder.queries.push(
        Field::new(
            "evaluationRuns",
            TypeRef::named("EvaluationRunConnection"),
            |ctx| {
                FieldFuture::new(async move {
                    let project_id = ctx.args.try_get("projectId")?.string()?.to_string();
                    let status = match scalars::defined(ctx.args.get("filter")) {
                        Some(filter) => {
                            EvaluationRunFilter::parse_value(context(), Some(filter))?.status
                        }
                        None => None,
                    }
                    .map(Into::into);
                    let after = scalars::optional_string(ctx.args.get("after"))?;
                    let first = ctx.args.try_get("first")?.i64()? as i32;
                    let principal = ctx.ctx.data::<RequestPrincipal>()?;
                    let connection = evaluation_service(ctx.ctx)?
                        .runs(principal.0, &project_id, status, after.as_deref(), first)
                        .await
                        .map_err(|error| async_graphql::Error::new(error.to_string()))?;
                    Ok(connection
                        .map(EvaluationRunConnection::from)
                        .and_then(|connection| connection.gql_field_value(context())))
                })
            },
        )
        .argument(InputValue::new("projectId", TypeRef::named_nn(TypeRef::ID)))
        .argument(InputValue::new(
            "filter",
            TypeRef::named("EvaluationRunFilter"),
        ))
        .argument(after_argument())
        .argument(first_argument()),
    );
    builder.queries.push(
        Field::new(
            "evaluationTargets",
            TypeRef::named("EvaluationTargetConnection"),
            |ctx| {
                FieldFuture::new(async move {
                    let project_id = ctx.args.try_get("projectId")?.string()?.to_string();
                    let definition_version_id = ctx
                        .args
                        .try_get("definitionVersionId")?
                        .string()?
                        .to_string();
                    let after = scalars::optional_string(ctx.args.get("after"))?;
                    let first = ctx.args.try_get("first")?.i64()? as i32;
                    let principal = ctx.ctx.data::<RequestPrincipal>()?;
                    let connection = evaluation_service(ctx.ctx)?
                        .targets(
                            principal.0,
                            &project_id,
                            &definition_version_id,
                            after.as_deref(),
                            first,
                        )
                        .await
                        .map_err(|error| async_graphql::Error::new(error.to_string()))?;
                    Ok(connection
                        .map(EvaluationTargetConnection::from)
                        .and_then(|connection| connection.gql_field_value(context())))
                })
            },
        )
        .argument(InputValue::new("projectId", TypeRef::named_nn(TypeRef::ID)))
        .argument(InputValue::new(
            "definitionVersionId",
            TypeRef::named_nn(TypeRef::ID),
        ))
        .argument(after_argument())
        .argument(first_argument()),
    );
}
