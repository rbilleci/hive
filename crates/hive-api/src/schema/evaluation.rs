//! Ports `EvaluationGraphql`: the 9 evaluation queries and 8 mutations.
//!
//! Every "long"-typed Java field (`revision`, `number`, `generation`,
//! `byteLength`, `durationMillis`, `expectedRevision`, `actualRevision`) is
//! ported as `i32`, matching this codebase's established convention — Java's
//! own `types(EvaluationService, GraphQLScalarType longScalar)` signature
//! confirms evaluation declares no custom scalar of its own; it reuses the
//! same shared `Long` scalar instance deployment does, which this port
//! substitutes with `i32` everywhere, same as deployment's port already does.

use crate::schema::RequestPrincipal;
use async_graphql::{Context, Enum, InputObject, Interface, Object, SimpleObject};
use hive_application::evaluation::document::EvaluationDiagnostic as AppDiagnostic;
use hive_application::evaluation::{
    Connection as AppConnection, Edge as AppEdge, EvaluationArtifactMetadata as AppArtifact,
    EvaluationAuditEvent as AppAuditEvent, EvaluationCaseRun as AppCaseRun,
    EvaluationDefinition as AppDefinition,
    EvaluationDefinitionConnection as AppDefinitionConnection,
    EvaluationDefinitionDraft as AppDraft, EvaluationDefinitionVersion as AppVersion,
    EvaluationDefinitionVersionConnection as AppVersionConnection,
    EvaluationMetricResult as AppMetric, EvaluationMutationResult as AppMutationResult,
    EvaluationProblem as AppProblem, EvaluationProblemKind as AppProblemKind,
    EvaluationRun as AppRun, EvaluationService, EvaluationTarget as AppTarget,
    EvaluationTargetSnapshot as AppTargetSnapshot,
};
use hive_persistence::evaluation::PgEvaluationRepository;
use uuid::Uuid;

fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    hive_domain::java_offset_date_time_string(value)
}

fn optional_timestamp(value: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    value.map(hive_domain::java_offset_date_time_string)
}

// --- enums ---

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum EvaluationTargetKind {
    AgentVersion,
    Deployment,
}

impl EvaluationTargetKind {
    fn parse(value: &str) -> Self {
        match value {
            "AGENT_VERSION" => Self::AgentVersion,
            "DEPLOYMENT" => Self::Deployment,
            other => panic!("unrecognized evaluation target kind `{other}`"),
        }
    }

    fn value(self) -> &'static str {
        match self {
            Self::AgentVersion => "AGENT_VERSION",
            Self::Deployment => "DEPLOYMENT",
        }
    }
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum EvaluationRunStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Canceled,
}

impl From<hive_application::evaluation::EvaluationRunStatus> for EvaluationRunStatus {
    fn from(value: hive_application::evaluation::EvaluationRunStatus) -> Self {
        use hive_application::evaluation::EvaluationRunStatus as Application;
        match value {
            Application::Queued => Self::Queued,
            Application::Running => Self::Running,
            Application::Completed => Self::Completed,
            Application::Failed => Self::Failed,
            Application::Canceled => Self::Canceled,
        }
    }
}

impl From<EvaluationRunStatus> for hive_application::evaluation::EvaluationRunStatus {
    fn from(value: EvaluationRunStatus) -> Self {
        match value {
            EvaluationRunStatus::Queued => Self::Queued,
            EvaluationRunStatus::Running => Self::Running,
            EvaluationRunStatus::Completed => Self::Completed,
            EvaluationRunStatus::Failed => Self::Failed,
            EvaluationRunStatus::Canceled => Self::Canceled,
        }
    }
}

#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum EvaluationOutcomeCategory {
    Passed,
    CaseFailed,
    TargetFailed,
    RunnerFailed,
    Canceled,
}

impl EvaluationOutcomeCategory {
    fn parse(value: &str) -> Self {
        match value {
            "PASSED" => Self::Passed,
            "CASE_FAILED" => Self::CaseFailed,
            "TARGET_FAILED" => Self::TargetFailed,
            "RUNNER_FAILED" => Self::RunnerFailed,
            "CANCELED" => Self::Canceled,
            other => panic!("unrecognized evaluation outcome category `{other}`"),
        }
    }
}

// --- object types ---

#[derive(SimpleObject)]
pub struct EvaluationDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub path: Vec<String>,
}

impl From<&AppDiagnostic> for EvaluationDiagnostic {
    fn from(value: &AppDiagnostic) -> Self {
        Self {
            code: value.code.clone(),
            severity: value.severity.clone(),
            message: value.message.clone(),
            path: value.path.clone(),
        }
    }
}

#[derive(SimpleObject)]
pub struct EvaluationDefinitionDraft {
    pub definition_id: async_graphql::ID,
    pub canonical_document: String,
    pub revision: i32,
    pub validation_status: String,
    pub diagnostics: Vec<EvaluationDiagnostic>,
    pub based_on_version_id: Option<async_graphql::ID>,
    pub updated_at: Option<String>,
}

impl From<&AppDraft> for EvaluationDefinitionDraft {
    fn from(value: &AppDraft) -> Self {
        Self {
            definition_id: async_graphql::ID(value.definition_id.to_string()),
            canonical_document: value.canonical_document.clone(),
            revision: value.revision as i32,
            validation_status: value.validation_status.clone(),
            diagnostics: value
                .diagnostics
                .iter()
                .map(EvaluationDiagnostic::from)
                .collect(),
            based_on_version_id: value
                .based_on_version_id
                .map(|id| async_graphql::ID(id.to_string())),
            updated_at: optional_timestamp(value.updated_at),
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct EvaluationDefinitionVersion {
    pub id: async_graphql::ID,
    pub definition_id: async_graphql::ID,
    pub number: i32,
    pub canonical_document: String,
    pub content_digest: String,
    pub based_on_version_id: Option<async_graphql::ID>,
    pub published_by: async_graphql::ID,
    pub published_at: String,
}

impl From<&AppVersion> for EvaluationDefinitionVersion {
    fn from(value: &AppVersion) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            definition_id: async_graphql::ID(value.definition_id.to_string()),
            number: value.number as i32,
            canonical_document: value.canonical_document.clone(),
            content_digest: value.content_digest.clone(),
            based_on_version_id: value
                .based_on_version_id
                .map(|id| async_graphql::ID(id.to_string())),
            published_by: async_graphql::ID(value.published_by.to_string()),
            published_at: timestamp(value.published_at),
        }
    }
}

#[derive(SimpleObject)]
pub struct EvaluationDefinitionVersionEdge {
    pub cursor: String,
    pub node: EvaluationDefinitionVersion,
}

#[derive(SimpleObject)]
pub struct EvaluationDefinitionVersionConnection {
    pub edges: Vec<EvaluationDefinitionVersionEdge>,
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

impl From<AppVersionConnection> for EvaluationDefinitionVersionConnection {
    fn from(value: AppVersionConnection) -> Self {
        Self {
            edges: value
                .edges
                .iter()
                .map(
                    |edge: &AppEdge<AppVersion>| EvaluationDefinitionVersionEdge {
                        cursor: edge.cursor.clone(),
                        node: EvaluationDefinitionVersion::from(&edge.node),
                    },
                )
                .collect(),
            has_next_page: value.has_next_page,
            end_cursor: value.end_cursor,
        }
    }
}

#[derive(SimpleObject)]
pub struct EvaluationDefinitionVersionComparison {
    pub left: Option<EvaluationDefinitionVersion>,
    pub right: Option<EvaluationDefinitionVersion>,
}

#[derive(SimpleObject)]
pub struct EvaluationDefinition {
    pub id: async_graphql::ID,
    pub project_id: async_graphql::ID,
    pub slug: String,
    pub lifecycle_status: String,
    pub draft: EvaluationDefinitionDraft,
    pub latest_version: Option<EvaluationDefinitionVersion>,
    pub can_author: bool,
    pub can_publish: bool,
    pub created_at: String,
}

impl From<&AppDefinition> for EvaluationDefinition {
    fn from(value: &AppDefinition) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            project_id: async_graphql::ID(value.project_id.to_string()),
            slug: value.slug.clone(),
            lifecycle_status: value.lifecycle_status.clone(),
            draft: EvaluationDefinitionDraft::from(&value.draft),
            latest_version: value
                .latest_version
                .as_ref()
                .map(EvaluationDefinitionVersion::from),
            can_author: value.can_author,
            can_publish: value.can_publish,
            created_at: timestamp(value.created_at),
        }
    }
}

#[derive(SimpleObject)]
pub struct EvaluationDefinitionEdge {
    pub cursor: String,
    pub node: EvaluationDefinition,
}

#[derive(SimpleObject)]
pub struct EvaluationDefinitionConnection {
    pub edges: Vec<EvaluationDefinitionEdge>,
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

impl From<AppDefinitionConnection> for EvaluationDefinitionConnection {
    fn from(value: AppDefinitionConnection) -> Self {
        Self {
            edges: value
                .edges
                .iter()
                .map(|edge: &AppEdge<AppDefinition>| EvaluationDefinitionEdge {
                    cursor: edge.cursor.clone(),
                    node: EvaluationDefinition::from(&edge.node),
                })
                .collect(),
            has_next_page: value.has_next_page,
            end_cursor: value.end_cursor,
        }
    }
}

#[derive(SimpleObject)]
pub struct EvaluationTarget {
    pub kind: EvaluationTargetKind,
    pub id: async_graphql::ID,
    pub agent_version_id: async_graphql::ID,
    pub environment_definition_version_id: async_graphql::ID,
    pub logical_environment_class: String,
    pub display_name: String,
}

impl From<&AppTarget> for EvaluationTarget {
    fn from(value: &AppTarget) -> Self {
        Self {
            kind: EvaluationTargetKind::parse(&value.kind),
            id: async_graphql::ID(value.id.to_string()),
            agent_version_id: async_graphql::ID(value.agent_version_id.to_string()),
            environment_definition_version_id: async_graphql::ID(
                value.environment_definition_version_id.to_string(),
            ),
            logical_environment_class: value.logical_environment_class.clone(),
            display_name: value.display_name.clone(),
        }
    }
}

#[derive(SimpleObject)]
pub struct EvaluationTargetSnapshot {
    pub agent_version_id: async_graphql::ID,
    pub deployment_id: Option<async_graphql::ID>,
    pub environment_definition_version_id: async_graphql::ID,
    pub logical_environment_class: String,
    pub agent_content_digest: String,
    pub target_digest: Option<String>,
    pub plan_digest: Option<String>,
    pub package_digest: Option<String>,
    pub binding_digest: Option<String>,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub environment_content_digest: String,
}

impl From<&AppTargetSnapshot> for EvaluationTargetSnapshot {
    fn from(value: &AppTargetSnapshot) -> Self {
        Self {
            agent_version_id: async_graphql::ID(value.agent_version_id.to_string()),
            deployment_id: value
                .deployment_id
                .map(|id| async_graphql::ID(id.to_string())),
            environment_definition_version_id: async_graphql::ID(
                value.environment_definition_version_id.to_string(),
            ),
            logical_environment_class: value.logical_environment_class.clone(),
            agent_content_digest: value.agent_content_digest.clone(),
            target_digest: value.target_digest.clone(),
            plan_digest: value.plan_digest.clone(),
            package_digest: value.package_digest.clone(),
            binding_digest: value.binding_digest.clone(),
            catalog_release_id: value.catalog_release_id.clone(),
            catalog_release_digest: value.catalog_release_digest.clone(),
            environment_content_digest: value.environment_content_digest.clone(),
        }
    }
}

#[derive(SimpleObject)]
pub struct EvaluationCaseRun {
    pub id: async_graphql::ID,
    pub key: String,
    pub ordinal: i32,
    pub lifecycle_status: String,
    pub passed: Option<bool>,
    pub failure_code: Option<String>,
    pub completed_at: Option<String>,
}

impl From<&AppCaseRun> for EvaluationCaseRun {
    fn from(value: &AppCaseRun) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            key: value.key.clone(),
            ordinal: value.ordinal,
            lifecycle_status: value.lifecycle_status.clone(),
            passed: value.passed,
            failure_code: value.failure_code.clone(),
            completed_at: optional_timestamp(value.completed_at),
        }
    }
}

#[derive(SimpleObject)]
pub struct EvaluationMetricResult {
    pub id: async_graphql::ID,
    pub code: String,
    pub value: f64,
    pub threshold: f64,
    pub passed: bool,
}

impl From<&AppMetric> for EvaluationMetricResult {
    fn from(value: &AppMetric) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            code: value.code.clone(),
            value: value.value,
            threshold: value.threshold,
            passed: value.passed,
        }
    }
}

#[derive(SimpleObject)]
pub struct EvaluationArtifactMetadata {
    pub id: async_graphql::ID,
    pub kind: String,
    pub content_digest: String,
    pub media_type: String,
    pub byte_length: i32,
}

impl From<&AppArtifact> for EvaluationArtifactMetadata {
    fn from(value: &AppArtifact) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            kind: value.kind.clone(),
            content_digest: value.content_digest.clone(),
            media_type: value.media_type.clone(),
            byte_length: value.byte_length as i32,
        }
    }
}

#[derive(SimpleObject)]
pub struct EvaluationAuditEvent {
    pub id: async_graphql::ID,
    pub action: String,
    pub occurred_at: String,
    pub summary: String,
}

impl From<&AppAuditEvent> for EvaluationAuditEvent {
    fn from(value: &AppAuditEvent) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            action: value.action.clone(),
            occurred_at: timestamp(value.occurred_at),
            summary: value.summary.clone(),
        }
    }
}

macro_rules! connection_type {
    ($connection_name:ident, $edge_name:ident, $node:ty) => {
        #[derive(SimpleObject)]
        pub struct $edge_name {
            pub cursor: String,
            pub node: $node,
        }

        #[derive(SimpleObject)]
        pub struct $connection_name {
            pub edges: Vec<$edge_name>,
            pub has_next_page: bool,
            pub end_cursor: Option<String>,
        }
    };
}

connection_type!(
    EvaluationTargetConnection,
    EvaluationTargetEdge,
    EvaluationTarget
);
connection_type!(
    EvaluationCaseRunConnection,
    EvaluationCaseRunEdge,
    EvaluationCaseRun
);
connection_type!(
    EvaluationMetricResultConnection,
    EvaluationMetricResultEdge,
    EvaluationMetricResult
);
connection_type!(
    EvaluationArtifactMetadataConnection,
    EvaluationArtifactMetadataEdge,
    EvaluationArtifactMetadata
);
connection_type!(
    EvaluationAuditEventConnection,
    EvaluationAuditEventEdge,
    EvaluationAuditEvent
);

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
                    has_next_page: value.has_next_page,
                    end_cursor: value.end_cursor,
                }
            }
        }
    };
}

from_app_connection!(
    AppTarget,
    EvaluationTargetConnection,
    EvaluationTargetEdge,
    EvaluationTarget::from
);
from_app_connection!(
    AppCaseRun,
    EvaluationCaseRunConnection,
    EvaluationCaseRunEdge,
    EvaluationCaseRun::from
);
from_app_connection!(
    AppMetric,
    EvaluationMetricResultConnection,
    EvaluationMetricResultEdge,
    EvaluationMetricResult::from
);
from_app_connection!(
    AppArtifact,
    EvaluationArtifactMetadataConnection,
    EvaluationArtifactMetadataEdge,
    EvaluationArtifactMetadata::from
);
from_app_connection!(
    AppAuditEvent,
    EvaluationAuditEventConnection,
    EvaluationAuditEventEdge,
    EvaluationAuditEvent::from
);

#[derive(SimpleObject)]
#[graphql(complex)]
pub struct EvaluationRun {
    pub id: async_graphql::ID,
    pub project_id: async_graphql::ID,
    pub definition_version_id: async_graphql::ID,
    pub target_kind: EvaluationTargetKind,
    pub target_id: async_graphql::ID,
    pub environment_definition_version_id: async_graphql::ID,
    pub source_run_id: Option<async_graphql::ID>,
    pub lifecycle_status: EvaluationRunStatus,
    pub generation: i32,
    pub outcome_category: Option<EvaluationOutcomeCategory>,
    pub outcome_code: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub duration_millis: Option<i32>,
    pub failure_summary: Option<String>,
    pub target: Option<EvaluationTargetSnapshot>,
    pub deployment_evidence_disposition: String,
}

impl From<&AppRun> for EvaluationRun {
    fn from(value: &AppRun) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            project_id: async_graphql::ID(value.project_id.to_string()),
            definition_version_id: async_graphql::ID(value.definition_version_id.to_string()),
            target_kind: EvaluationTargetKind::parse(&value.target_kind),
            target_id: async_graphql::ID(value.target_id.to_string()),
            environment_definition_version_id: async_graphql::ID(
                value.environment_definition_version_id.to_string(),
            ),
            source_run_id: value
                .source_run_id
                .map(|id| async_graphql::ID(id.to_string())),
            lifecycle_status: value.lifecycle_status.into(),
            generation: value.generation as i32,
            outcome_category: value
                .outcome_category
                .as_deref()
                .map(EvaluationOutcomeCategory::parse),
            outcome_code: value.outcome_code.clone(),
            created_at: timestamp(value.created_at),
            started_at: optional_timestamp(value.started_at),
            completed_at: optional_timestamp(value.completed_at),
            duration_millis: value.duration_millis().map(|value| value as i32),
            failure_summary: value.failure_summary(),
            target: value.target.as_ref().map(EvaluationTargetSnapshot::from),
            deployment_evidence_disposition: value.deployment_evidence_disposition.clone(),
        }
    }
}

#[async_graphql::ComplexObject]
impl EvaluationRun {
    async fn cases(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<EvaluationCaseRunConnection> {
        let connection = evaluation_service(ctx)?
            .cases(principal(ctx)?, self.id.as_str(), after.as_deref(), first)
            .await
            .map_err(map_error)?;
        Ok(connection.map(EvaluationCaseRunConnection::from).unwrap_or(
            EvaluationCaseRunConnection {
                edges: Vec::new(),
                has_next_page: false,
                end_cursor: None,
            },
        ))
    }

    async fn metrics(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<EvaluationMetricResultConnection> {
        let connection = evaluation_service(ctx)?
            .metrics(principal(ctx)?, self.id.as_str(), after.as_deref(), first)
            .await
            .map_err(map_error)?;
        Ok(connection
            .map(EvaluationMetricResultConnection::from)
            .unwrap_or(EvaluationMetricResultConnection {
                edges: Vec::new(),
                has_next_page: false,
                end_cursor: None,
            }))
    }

    async fn artifacts(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<EvaluationArtifactMetadataConnection> {
        let connection = evaluation_service(ctx)?
            .artifacts(principal(ctx)?, self.id.as_str(), after.as_deref(), first)
            .await
            .map_err(map_error)?;
        Ok(connection
            .map(EvaluationArtifactMetadataConnection::from)
            .unwrap_or(EvaluationArtifactMetadataConnection {
                edges: Vec::new(),
                has_next_page: false,
                end_cursor: None,
            }))
    }

    async fn audit(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<EvaluationAuditEventConnection> {
        let connection = evaluation_service(ctx)?
            .audit(principal(ctx)?, self.id.as_str(), after.as_deref(), first)
            .await
            .map_err(map_error)?;
        Ok(connection
            .map(EvaluationAuditEventConnection::from)
            .unwrap_or(EvaluationAuditEventConnection {
                edges: Vec::new(),
                has_next_page: false,
                end_cursor: None,
            }))
    }
}

connection_type!(EvaluationRunConnection, EvaluationRunEdge, EvaluationRun);
from_app_connection!(
    AppRun,
    EvaluationRunConnection,
    EvaluationRunEdge,
    EvaluationRun::from
);

// --- problem interface ---

#[derive(SimpleObject)]
pub struct EvaluationNotFoundProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct EvaluationAuthorizationProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct EvaluationValidationProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct EvaluationLifecycleProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct EvaluationIdempotencyProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct EvaluationTargetCompatibilityProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct EvaluationUnavailableProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct EvaluationRevisionConflict {
    pub code: String,
    pub message: String,
    pub resource_id: Option<async_graphql::ID>,
    pub expected_revision: Option<i32>,
    pub actual_revision: Option<i32>,
}

#[derive(Interface)]
#[allow(clippy::duplicated_attributes)]
#[graphql(field(name = "code", ty = "String"))]
#[graphql(field(name = "message", ty = "String"))]
pub enum EvaluationProblem {
    NotFound(EvaluationNotFoundProblem),
    Authorization(EvaluationAuthorizationProblem),
    Validation(EvaluationValidationProblem),
    Lifecycle(EvaluationLifecycleProblem),
    Idempotency(EvaluationIdempotencyProblem),
    TargetCompatibility(EvaluationTargetCompatibilityProblem),
    Unavailable(EvaluationUnavailableProblem),
    RevisionConflict(EvaluationRevisionConflict),
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
            AppProblemKind::NotFound => EvaluationProblem::NotFound(EvaluationNotFoundProblem {
                code,
                message: problem.message,
            }),
            AppProblemKind::Forbidden => {
                EvaluationProblem::Authorization(EvaluationAuthorizationProblem {
                    code,
                    message: problem.message,
                })
            }
            AppProblemKind::Validation => {
                EvaluationProblem::Validation(EvaluationValidationProblem {
                    code,
                    message: problem.message,
                })
            }
            AppProblemKind::RevisionConflict => {
                EvaluationProblem::RevisionConflict(EvaluationRevisionConflict {
                    code,
                    message: problem.message,
                    resource_id: problem
                        .resource_id
                        .map(|id| async_graphql::ID(id.to_string())),
                    expected_revision: (problem.expected_revision >= 0)
                        .then_some(problem.expected_revision as i32),
                    actual_revision: (problem.actual_revision >= 0)
                        .then_some(problem.actual_revision as i32),
                })
            }
            AppProblemKind::LifecycleConflict => {
                EvaluationProblem::Lifecycle(EvaluationLifecycleProblem {
                    code,
                    message: problem.message,
                })
            }
            AppProblemKind::IdempotencyConflict => {
                EvaluationProblem::Idempotency(EvaluationIdempotencyProblem {
                    code,
                    message: problem.message,
                })
            }
            AppProblemKind::TargetIncompatible => {
                EvaluationProblem::TargetCompatibility(EvaluationTargetCompatibilityProblem {
                    code,
                    message: problem.message,
                })
            }
            AppProblemKind::Unavailable => {
                EvaluationProblem::Unavailable(EvaluationUnavailableProblem {
                    code,
                    message: problem.message,
                })
            }
        }
    }
}

#[derive(SimpleObject)]
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

// --- inputs ---

#[derive(InputObject)]
pub struct EvaluationRunFilter {
    pub status: Option<EvaluationRunStatus>,
}

#[derive(InputObject)]
pub struct CreateEvaluationDefinitionInput {
    pub project_id: async_graphql::ID,
    pub slug: String,
    pub document: Option<String>,
    pub idempotency_key: String,
}

#[derive(InputObject)]
pub struct UpdateEvaluationDefinitionDraftInput {
    pub definition_id: async_graphql::ID,
    pub expected_revision: i32,
    pub document: String,
    pub idempotency_key: String,
}

#[derive(InputObject)]
pub struct ValidateEvaluationDefinitionDraftInput {
    pub definition_id: async_graphql::ID,
    pub expected_revision: i32,
    pub idempotency_key: String,
}

#[derive(InputObject)]
pub struct DuplicateEvaluationDefinitionVersionToDraftInput {
    pub version_id: async_graphql::ID,
    pub expected_revision: i32,
    pub idempotency_key: String,
}

#[derive(InputObject)]
pub struct PublishEvaluationDefinitionDraftInput {
    pub definition_id: async_graphql::ID,
    pub expected_revision: i32,
    pub idempotency_key: String,
}

#[derive(InputObject)]
pub struct RunEvaluationInput {
    pub project_id: async_graphql::ID,
    pub definition_version_id: async_graphql::ID,
    pub target_kind: EvaluationTargetKind,
    pub target_id: async_graphql::ID,
    pub environment_definition_version_id: async_graphql::ID,
    pub idempotency_key: String,
}

/// `reason` is accepted but never read by any Java resolver (`resolver.cancel()` never touches
/// `i.get("reason")`) — a dead input field, kept here for schema parity rather than silently
/// dropped from the port.
#[derive(InputObject)]
pub struct CancelEvaluationInput {
    pub run_id: async_graphql::ID,
    pub expected_generation: i32,
    pub idempotency_key: String,
    #[allow(dead_code)]
    pub reason: Option<String>,
}

#[derive(InputObject)]
pub struct RerunEvaluationInput {
    pub run_id: async_graphql::ID,
    pub idempotency_key: String,
}

// --- resolvers ---

fn evaluation_service(
    ctx: &Context<'_>,
) -> async_graphql::Result<EvaluationService<PgEvaluationRepository>> {
    let repository = PgEvaluationRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
    Ok(EvaluationService::new(repository))
}

fn principal(ctx: &Context<'_>) -> async_graphql::Result<Uuid> {
    Ok(ctx.data::<RequestPrincipal>()?.0)
}

fn map_error(error: impl std::fmt::Display) -> async_graphql::Error {
    async_graphql::Error::new(error.to_string())
}

pub struct EvaluationQueries;

#[Object]
impl EvaluationQueries {
    async fn evaluation_definitions(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        #[graphql(default = 50)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<Option<EvaluationDefinitionConnection>> {
        let connection = evaluation_service(ctx)?
            .definitions(
                principal(ctx)?,
                project_id.as_str(),
                after.as_deref(),
                first,
            )
            .await
            .map_err(map_error)?;
        Ok(connection.map(EvaluationDefinitionConnection::from))
    }

    async fn evaluation_definition(
        &self,
        ctx: &Context<'_>,
        definition_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<EvaluationDefinition>> {
        let value = evaluation_service(ctx)?
            .definition(principal(ctx)?, definition_id.as_str())
            .await
            .map_err(map_error)?;
        Ok(value.as_ref().map(EvaluationDefinition::from))
    }

    async fn evaluation_definition_version(
        &self,
        ctx: &Context<'_>,
        version_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<EvaluationDefinitionVersion>> {
        let value = evaluation_service(ctx)?
            .definition_version(principal(ctx)?, version_id.as_str())
            .await
            .map_err(map_error)?;
        Ok(value.as_ref().map(EvaluationDefinitionVersion::from))
    }

    async fn evaluation_definition_versions(
        &self,
        ctx: &Context<'_>,
        definition_id: async_graphql::ID,
        #[graphql(default = 50)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<Option<EvaluationDefinitionVersionConnection>> {
        let connection = evaluation_service(ctx)?
            .definition_versions(
                principal(ctx)?,
                definition_id.as_str(),
                after.as_deref(),
                first,
            )
            .await
            .map_err(map_error)?;
        Ok(connection.map(EvaluationDefinitionVersionConnection::from))
    }

    async fn evaluation_definition_version_usage(
        &self,
        ctx: &Context<'_>,
        version_id: async_graphql::ID,
        #[graphql(default = 50)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<Option<EvaluationRunConnection>> {
        let connection = evaluation_service(ctx)?
            .definition_version_usage(
                principal(ctx)?,
                version_id.as_str(),
                after.as_deref(),
                first,
            )
            .await
            .map_err(map_error)?;
        Ok(connection.map(EvaluationRunConnection::from))
    }

    async fn evaluation_definition_version_comparison(
        &self,
        ctx: &Context<'_>,
        left_version_id: async_graphql::ID,
        right_version_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<EvaluationDefinitionVersionComparison>> {
        let service = evaluation_service(ctx)?;
        let principal_id = principal(ctx)?;
        let left = service
            .definition_version(principal_id, left_version_id.as_str())
            .await
            .map_err(map_error)?;
        let right = service
            .definition_version(principal_id, right_version_id.as_str())
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

    async fn evaluation_runs(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        filter: Option<EvaluationRunFilter>,
        #[graphql(default = 50)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<Option<EvaluationRunConnection>> {
        let status = filter.and_then(|filter| filter.status).map(Into::into);
        let connection = evaluation_service(ctx)?
            .runs(
                principal(ctx)?,
                project_id.as_str(),
                status,
                after.as_deref(),
                first,
            )
            .await
            .map_err(map_error)?;
        Ok(connection.map(EvaluationRunConnection::from))
    }

    async fn evaluation_run(
        &self,
        ctx: &Context<'_>,
        run_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<EvaluationRun>> {
        let value = evaluation_service(ctx)?
            .run(principal(ctx)?, run_id.as_str())
            .await
            .map_err(map_error)?;
        Ok(value.as_ref().map(EvaluationRun::from))
    }

    async fn evaluation_targets(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        definition_version_id: async_graphql::ID,
        #[graphql(default = 50)] first: i32,
        after: Option<String>,
    ) -> async_graphql::Result<Option<EvaluationTargetConnection>> {
        let connection = evaluation_service(ctx)?
            .targets(
                principal(ctx)?,
                project_id.as_str(),
                definition_version_id.as_str(),
                after.as_deref(),
                first,
            )
            .await
            .map_err(map_error)?;
        Ok(connection.map(EvaluationTargetConnection::from))
    }
}

pub struct EvaluationMutations;

#[Object]
impl EvaluationMutations {
    async fn create_evaluation_definition(
        &self,
        ctx: &Context<'_>,
        input: CreateEvaluationDefinitionInput,
    ) -> async_graphql::Result<EvaluationMutationPayload> {
        let result = evaluation_service(ctx)?
            .create_definition(
                principal(ctx)?,
                input.project_id.as_str(),
                &input.slug,
                input.document.as_deref(),
                &input.idempotency_key,
            )
            .await
            .map_err(map_error)?;
        Ok(EvaluationMutationPayload::from(result))
    }

    async fn update_evaluation_definition_draft(
        &self,
        ctx: &Context<'_>,
        input: UpdateEvaluationDefinitionDraftInput,
    ) -> async_graphql::Result<EvaluationMutationPayload> {
        let result = evaluation_service(ctx)?
            .update_draft(
                principal(ctx)?,
                input.definition_id.as_str(),
                input.expected_revision as i64,
                &input.document,
                &input.idempotency_key,
            )
            .await
            .map_err(map_error)?;
        Ok(EvaluationMutationPayload::from(result))
    }

    async fn validate_evaluation_definition_draft(
        &self,
        ctx: &Context<'_>,
        input: ValidateEvaluationDefinitionDraftInput,
    ) -> async_graphql::Result<EvaluationMutationPayload> {
        let result = evaluation_service(ctx)?
            .validate_draft(
                principal(ctx)?,
                input.definition_id.as_str(),
                input.expected_revision as i64,
                &input.idempotency_key,
            )
            .await
            .map_err(map_error)?;
        Ok(EvaluationMutationPayload::from(result))
    }

    async fn duplicate_evaluation_definition_version_to_draft(
        &self,
        ctx: &Context<'_>,
        input: DuplicateEvaluationDefinitionVersionToDraftInput,
    ) -> async_graphql::Result<EvaluationMutationPayload> {
        let result = evaluation_service(ctx)?
            .duplicate_version(
                principal(ctx)?,
                input.version_id.as_str(),
                input.expected_revision as i64,
                &input.idempotency_key,
            )
            .await
            .map_err(map_error)?;
        Ok(EvaluationMutationPayload::from(result))
    }

    async fn publish_evaluation_definition_draft(
        &self,
        ctx: &Context<'_>,
        input: PublishEvaluationDefinitionDraftInput,
    ) -> async_graphql::Result<EvaluationMutationPayload> {
        let result = evaluation_service(ctx)?
            .publish_draft(
                principal(ctx)?,
                input.definition_id.as_str(),
                input.expected_revision as i64,
                &input.idempotency_key,
            )
            .await
            .map_err(map_error)?;
        Ok(EvaluationMutationPayload::from(result))
    }

    async fn run_evaluation(
        &self,
        ctx: &Context<'_>,
        input: RunEvaluationInput,
    ) -> async_graphql::Result<EvaluationMutationPayload> {
        let result = evaluation_service(ctx)?
            .run_evaluation(
                principal(ctx)?,
                input.project_id.as_str(),
                input.definition_version_id.as_str(),
                input.target_kind.value(),
                input.target_id.as_str(),
                input.environment_definition_version_id.as_str(),
                &input.idempotency_key,
            )
            .await
            .map_err(map_error)?;
        Ok(EvaluationMutationPayload::from(result))
    }

    async fn cancel_evaluation(
        &self,
        ctx: &Context<'_>,
        input: CancelEvaluationInput,
    ) -> async_graphql::Result<EvaluationMutationPayload> {
        let result = evaluation_service(ctx)?
            .cancel(
                principal(ctx)?,
                input.run_id.as_str(),
                input.expected_generation as i64,
                &input.idempotency_key,
            )
            .await
            .map_err(map_error)?;
        Ok(EvaluationMutationPayload::from(result))
    }

    async fn rerun_evaluation(
        &self,
        ctx: &Context<'_>,
        input: RerunEvaluationInput,
    ) -> async_graphql::Result<EvaluationMutationPayload> {
        let result = evaluation_service(ctx)?
            .rerun(
                principal(ctx)?,
                input.run_id.as_str(),
                &input.idempotency_key,
            )
            .await
            .map_err(map_error)?;
        Ok(EvaluationMutationPayload::from(result))
    }
}
