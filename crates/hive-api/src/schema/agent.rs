//! Ports `AgentDraftResolver`/`AgentOperationalViewResolver` and the inline
//! SDL types `GraphqlSchemaFactory` declares for them (there is no
//! `AgentGraphql.java`): the five agent-draft/version read fields, the four
//! agent-draft write mutations, and the read-only `agentOperationalView`
//! field.

use crate::schema::RequestPrincipal;
use async_graphql::{
    Context, InputObject, InputValueError, InputValueResult, Interface, Object, Scalar, ScalarType,
    SimpleObject,
};
use hive_application::agent::{
    AgentDraft as AppAgentDraft, AgentDraftDiagnostic as AppAgentDraftDiagnostic,
    AgentDraftEditorService, AgentDraftMutationProblem as AppProblem,
    AgentDraftMutationResult as AppMutationResult, AgentDraftProblemKind as AppProblemKind,
    AgentDraftReview as AppAgentDraftReview, AgentOperationalView as AppOperationalView,
    AgentOperationalViewQueryService, AgentVersion as AppAgentVersion,
    AgentVersionComparison as AppAgentVersionComparison,
};
use hive_persistence::agent::{PgAgentDraftRepository, PgAgentOperationalViewRepository};
use uuid::Uuid;

/// Ports the ad hoc `JSON` scalar `GraphqlSchemaFactory` declares: a pass-
/// through structured value, no schema validation of its shape.
#[derive(Clone)]
pub struct Json(pub serde_json::Value);

#[Scalar(name = "JSON")]
impl ScalarType for Json {
    fn parse(value: async_graphql::Value) -> InputValueResult<Self> {
        value
            .into_json()
            .map(Json)
            .map_err(|error| InputValueError::custom(error.to_string()))
    }

    fn to_value(&self) -> async_graphql::Value {
        async_graphql::Value::from_json(self.0.clone()).unwrap_or(async_graphql::Value::Null)
    }
}

fn parsed_document(document: &str) -> Json {
    Json(
        serde_json::from_str(document)
            .expect("the stored agent draft document is always valid JSON"),
    )
}

fn timestamp(value: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    value.map(hive_domain::java_offset_date_time_string)
}

#[derive(SimpleObject)]
pub struct AgentDraftDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub path: Vec<String>,
}

impl From<AppAgentDraftDiagnostic> for AgentDraftDiagnostic {
    fn from(value: AppAgentDraftDiagnostic) -> Self {
        Self {
            code: value.code,
            severity: value.severity,
            message: value.message,
            path: value.path,
        }
    }
}

#[derive(SimpleObject)]
pub struct AgentDraft {
    pub id: async_graphql::ID,
    pub agent_id: async_graphql::ID,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub document: Json,
    pub revision: i32,
    pub validation_status: String,
    pub validation_diagnostics: Vec<AgentDraftDiagnostic>,
    pub validated_at: Option<String>,
    pub can_update: bool,
    pub can_publish: bool,
    pub latest_version: Option<i32>,
}

impl From<AppAgentDraft> for AgentDraft {
    fn from(value: AppAgentDraft) -> Self {
        Self {
            id: async_graphql::ID(value.agent_id.to_string()),
            agent_id: async_graphql::ID(value.agent_id.to_string()),
            slug: value.slug,
            display_name: value.display_name,
            lifecycle_status: value.lifecycle_status,
            document: parsed_document(&value.document),
            revision: value.revision as i32,
            validation_status: value.validation_status,
            validation_diagnostics: value
                .validation_diagnostics
                .into_iter()
                .map(AgentDraftDiagnostic::from)
                .collect(),
            validated_at: timestamp(value.validated_at),
            can_update: value.can_update,
            can_publish: value.can_publish,
            latest_version: value.latest_version.map(|version| version as i32),
        }
    }
}

#[derive(SimpleObject)]
pub struct AgentVersion {
    pub id: async_graphql::ID,
    pub agent_id: async_graphql::ID,
    pub number: i32,
    pub slug: String,
    pub display_name: String,
    pub canonical_document: Json,
    pub content_digest: String,
    pub dependencies: Vec<String>,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub published_by: async_graphql::ID,
    pub published_at: String,
}

impl From<AppAgentVersion> for AgentVersion {
    fn from(value: AppAgentVersion) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            agent_id: async_graphql::ID(value.agent_id.to_string()),
            number: value.number as i32,
            slug: value.slug,
            display_name: value.display_name,
            canonical_document: parsed_document(&value.canonical_document),
            content_digest: value.content_digest,
            dependencies: value.dependencies,
            catalog_release_id: value.catalog_release_id,
            catalog_release_digest: value.catalog_release_digest,
            published_by: async_graphql::ID(value.published_by.to_string()),
            published_at: hive_domain::java_offset_date_time_string(value.published_at),
        }
    }
}

#[derive(SimpleObject)]
pub struct AgentVersionComparison {
    pub from: AgentVersion,
    pub to: AgentVersion,
    pub changed_sections: Vec<String>,
}

impl From<AppAgentVersionComparison> for AgentVersionComparison {
    fn from(value: AppAgentVersionComparison) -> Self {
        Self {
            from: value.from.into(),
            to: value.to.into(),
            changed_sections: value.changed_sections,
        }
    }
}

#[derive(SimpleObject)]
pub struct AgentDraftReview {
    pub draft: AgentDraft,
    pub canonical_document: Json,
    pub content_digest: String,
    pub dependencies: Vec<String>,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub changed_sections: Vec<String>,
    pub diagnostics: Vec<AgentDraftDiagnostic>,
}

impl From<AppAgentDraftReview> for AgentDraftReview {
    fn from(value: AppAgentDraftReview) -> Self {
        Self {
            draft: value.draft.into(),
            canonical_document: parsed_document(&value.canonical_document),
            content_digest: value.content_digest,
            dependencies: value.dependencies,
            catalog_release_id: value.catalog_release_id,
            catalog_release_digest: value.catalog_release_digest,
            changed_sections: value.changed_sections,
            diagnostics: value
                .diagnostics
                .into_iter()
                .map(AgentDraftDiagnostic::from)
                .collect(),
        }
    }
}

#[derive(SimpleObject)]
pub struct AgentDraftNotFoundProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct AgentDraftAuthorizationProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct AgentDraftValidationProblem {
    pub code: String,
    pub message: String,
}

#[derive(SimpleObject)]
pub struct AgentDraftRevisionConflict {
    pub code: String,
    pub message: String,
    pub resource_id: async_graphql::ID,
    pub expected_revision: i32,
    pub actual_revision: i32,
}

/// Ports the `AgentDraftProblem` GraphQL interface: every variant shares
/// exactly `code`/`message`; `AgentDraftRevisionConflict` carries three more
/// fields of its own that a client reaches through an inline fragment.
#[derive(Interface)]
#[allow(clippy::duplicated_attributes)]
#[graphql(field(name = "code", ty = "String"))]
#[graphql(field(name = "message", ty = "String"))]
pub enum AgentDraftProblem {
    NotFound(AgentDraftNotFoundProblem),
    Authorization(AgentDraftAuthorizationProblem),
    Validation(AgentDraftValidationProblem),
    RevisionConflict(AgentDraftRevisionConflict),
}

impl From<AppProblem> for AgentDraftProblem {
    fn from(problem: AppProblem) -> Self {
        match problem.kind {
            AppProblemKind::NotFound => AgentDraftProblem::NotFound(AgentDraftNotFoundProblem {
                code: "NOT_FOUND".to_string(),
                message: "This agent is unavailable.".to_string(),
            }),
            AppProblemKind::Forbidden => {
                AgentDraftProblem::Authorization(AgentDraftAuthorizationProblem {
                    code: "FORBIDDEN".to_string(),
                    message: "You do not have permission to edit this draft.".to_string(),
                })
            }
            AppProblemKind::RevisionConflict => {
                AgentDraftProblem::RevisionConflict(AgentDraftRevisionConflict {
                    code: "REVISION_CONFLICT".to_string(),
                    message: "This draft changed after you opened it.".to_string(),
                    resource_id: async_graphql::ID(
                        problem
                            .resource_id
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                    ),
                    expected_revision: problem.expected_revision as i32,
                    actual_revision: problem.actual_revision as i32,
                })
            }
            AppProblemKind::InvalidDocument => {
                AgentDraftProblem::Validation(AgentDraftValidationProblem {
                    code: "INVALID_DOCUMENT".to_string(),
                    message: "The draft document must be an object.".to_string(),
                })
            }
            AppProblemKind::InvalidDraft => {
                AgentDraftProblem::Validation(AgentDraftValidationProblem {
                    code: "INVALID_DRAFT".to_string(),
                    message: "Resolve the server validation errors before publication.".to_string(),
                })
            }
            AppProblemKind::WarningAcknowledgementRequired => {
                AgentDraftProblem::Validation(AgentDraftValidationProblem {
                    code: "WARNING_ACKNOWLEDGEMENT_REQUIRED".to_string(),
                    message: "Review and acknowledge the server warnings before publication."
                        .to_string(),
                })
            }
        }
    }
}

#[derive(SimpleObject)]
pub struct AgentDraftMutationPayload {
    pub agent_draft: Option<AgentDraft>,
    pub agent_version: Option<AgentVersion>,
    pub problems: Vec<AgentDraftProblem>,
}

impl From<AppMutationResult> for AgentDraftMutationPayload {
    fn from(result: AppMutationResult) -> Self {
        Self {
            agent_draft: result.agent_draft.map(AgentDraft::from),
            agent_version: result.agent_version.map(AgentVersion::from),
            problems: result
                .problem
                .into_iter()
                .map(AgentDraftProblem::from)
                .collect(),
        }
    }
}

#[derive(InputObject)]
pub struct CreateAgentDraftInput {
    pub project_id: async_graphql::ID,
    pub display_name: String,
    pub slug: Option<String>,
}

#[derive(InputObject)]
pub struct UpdateAgentDraftInput {
    pub project_id: async_graphql::ID,
    pub agent_id: async_graphql::ID,
    pub expected_revision: i32,
    pub document: Json,
}

#[derive(InputObject)]
pub struct ValidateAgentDraftInput {
    pub project_id: async_graphql::ID,
    pub agent_id: async_graphql::ID,
    pub expected_revision: i32,
}

#[derive(InputObject)]
pub struct PublishAgentDraftInput {
    pub project_id: async_graphql::ID,
    pub agent_id: async_graphql::ID,
    pub expected_revision: i32,
    pub warnings_acknowledged: bool,
}

#[derive(SimpleObject)]
pub struct AgentDraftValidationSummary {
    pub status: String,
    pub error_count: i32,
    pub warning_count: i32,
    pub validated_at: Option<String>,
}

#[derive(SimpleObject)]
pub struct AgentPublishedVersionSummary {
    pub status: String,
    pub version: Option<String>,
    pub published_at: Option<String>,
}

#[derive(SimpleObject)]
pub struct AgentAliasTargetsSummary {
    pub total_count: i32,
    pub active_count: i32,
}

#[derive(SimpleObject)]
pub struct AgentDeploymentSummary {
    pub status: String,
    pub observed_at: Option<String>,
}

#[derive(SimpleObject)]
pub struct AgentEvaluationSummary {
    pub outcome: String,
    pub completed_at: Option<String>,
}

#[derive(SimpleObject)]
pub struct AgentRuntimeHealthSummary {
    pub status: String,
    pub observed_at: Option<String>,
    pub freshness: String,
}

#[derive(SimpleObject)]
pub struct AgentOperationalView {
    pub id: async_graphql::ID,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub draft_validation: AgentDraftValidationSummary,
    pub latest_published_version: AgentPublishedVersionSummary,
    pub alias_targets: AgentAliasTargetsSummary,
    pub active_deployment: AgentDeploymentSummary,
    pub recent_evaluation: AgentEvaluationSummary,
    pub runtime_health: AgentRuntimeHealthSummary,
}

impl From<AppOperationalView> for AgentOperationalView {
    fn from(value: AppOperationalView) -> Self {
        Self {
            id: async_graphql::ID(value.id.to_string()),
            slug: value.slug,
            display_name: value.display_name,
            lifecycle_status: value.lifecycle_status,
            draft_validation: AgentDraftValidationSummary {
                status: value.draft_validation.status,
                error_count: value.draft_validation.error_count,
                warning_count: value.draft_validation.warning_count,
                validated_at: timestamp(value.draft_validation.validated_at),
            },
            latest_published_version: AgentPublishedVersionSummary {
                status: value.latest_published_version.status,
                version: value.latest_published_version.version,
                published_at: timestamp(value.latest_published_version.published_at),
            },
            alias_targets: AgentAliasTargetsSummary {
                total_count: value.alias_targets.total_count,
                active_count: value.alias_targets.active_count,
            },
            active_deployment: AgentDeploymentSummary {
                status: value.active_deployment.status,
                observed_at: timestamp(value.active_deployment.observed_at),
            },
            recent_evaluation: AgentEvaluationSummary {
                outcome: value.recent_evaluation.outcome,
                completed_at: timestamp(value.recent_evaluation.completed_at),
            },
            runtime_health: AgentRuntimeHealthSummary {
                status: value.runtime_health.status,
                observed_at: timestamp(value.runtime_health.observed_at),
                freshness: value.runtime_health.freshness,
            },
        }
    }
}

fn draft_service(
    ctx: &Context<'_>,
) -> async_graphql::Result<AgentDraftEditorService<PgAgentDraftRepository>> {
    let repository = PgAgentDraftRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
    Ok(AgentDraftEditorService::new(repository))
}

fn operational_view_service(
    ctx: &Context<'_>,
) -> async_graphql::Result<AgentOperationalViewQueryService<PgAgentOperationalViewRepository>> {
    let repository = PgAgentOperationalViewRepository::new(ctx.data::<sqlx::PgPool>()?.clone());
    Ok(AgentOperationalViewQueryService::new(repository))
}

fn principal(ctx: &Context<'_>) -> async_graphql::Result<Uuid> {
    Ok(ctx.data::<RequestPrincipal>()?.0)
}

pub struct AgentQueries;

#[Object]
impl AgentQueries {
    /// Ports `AgentDraftResolver.resolveDraft`.
    async fn agent_draft(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        agent_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<AgentDraft>> {
        let draft = draft_service(ctx)?
            .find_draft(principal(ctx)?, project_id.as_str(), agent_id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(draft.map(AgentDraft::from))
    }

    /// Ports `AgentDraftResolver.resolveReview`.
    async fn agent_draft_review(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        agent_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<AgentDraftReview>> {
        let review = draft_service(ctx)?
            .review_draft(principal(ctx)?, project_id.as_str(), agent_id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(review.map(AgentDraftReview::from))
    }

    /// Ports `AgentDraftResolver.resolveVersions`.
    async fn agent_versions(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        agent_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<Vec<AgentVersion>>> {
        let versions = draft_service(ctx)?
            .versions(principal(ctx)?, project_id.as_str(), agent_id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(versions.map(|values| values.into_iter().map(AgentVersion::from).collect()))
    }

    /// Ports `AgentDraftResolver.resolveVersion`.
    async fn agent_version(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        agent_id: async_graphql::ID,
        version_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<AgentVersion>> {
        let version = draft_service(ctx)?
            .version(
                principal(ctx)?,
                project_id.as_str(),
                agent_id.as_str(),
                version_id.as_str(),
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(version.map(AgentVersion::from))
    }

    /// Ports `AgentDraftResolver.resolveComparison`.
    async fn compare_agent_versions(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        agent_id: async_graphql::ID,
        from_version_id: async_graphql::ID,
        to_version_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<AgentVersionComparison>> {
        let comparison = draft_service(ctx)?
            .compare_versions(
                principal(ctx)?,
                project_id.as_str(),
                agent_id.as_str(),
                from_version_id.as_str(),
                to_version_id.as_str(),
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        // `AgentVersionComparison::from` as a bare path resolves to the
        // async-graphql-generated resolver for its `from` field (2 args: self,
        // ctx), not `<Self as From<AppAgentVersionComparison>>::from` (1 arg) —
        // the field name collides with the trait method name, so this needs the
        // fully qualified form to pick the trait impl.
        Ok(comparison.map(<AgentVersionComparison as From<AppAgentVersionComparison>>::from))
    }

    /// Ports `AgentOperationalViewResolver.resolve`.
    async fn agent_operational_view(
        &self,
        ctx: &Context<'_>,
        project_id: async_graphql::ID,
        agent_id: async_graphql::ID,
    ) -> async_graphql::Result<Option<AgentOperationalView>> {
        let overview = operational_view_service(ctx)?
            .find_overview(principal(ctx)?, project_id.as_str(), agent_id.as_str())
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(overview.map(AgentOperationalView::from))
    }
}

pub struct AgentMutations;

#[Object]
impl AgentMutations {
    /// Ports `AgentDraftResolver.createDraft`.
    async fn create_agent_draft(
        &self,
        ctx: &Context<'_>,
        input: CreateAgentDraftInput,
    ) -> async_graphql::Result<AgentDraftMutationPayload> {
        let result = draft_service(ctx)?
            .create_draft(
                principal(ctx)?,
                input.project_id.as_str(),
                input.display_name,
                input.slug,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AgentDraftMutationPayload::from(result))
    }

    /// Ports `AgentDraftResolver.updateDraft`.
    async fn update_agent_draft(
        &self,
        ctx: &Context<'_>,
        input: UpdateAgentDraftInput,
    ) -> async_graphql::Result<AgentDraftMutationPayload> {
        let document = serde_json::to_string(&input.document.0)
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        let result = draft_service(ctx)?
            .update_draft(
                principal(ctx)?,
                input.project_id.as_str(),
                input.agent_id.as_str(),
                input.expected_revision as i64,
                document,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AgentDraftMutationPayload::from(result))
    }

    /// Ports `AgentDraftResolver.validateDraft`.
    async fn validate_agent_draft(
        &self,
        ctx: &Context<'_>,
        input: ValidateAgentDraftInput,
    ) -> async_graphql::Result<AgentDraftMutationPayload> {
        let result = draft_service(ctx)?
            .validate_draft(
                principal(ctx)?,
                input.project_id.as_str(),
                input.agent_id.as_str(),
                input.expected_revision as i64,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AgentDraftMutationPayload::from(result))
    }

    /// Ports `AgentDraftResolver.publishDraft`.
    async fn publish_agent_draft(
        &self,
        ctx: &Context<'_>,
        input: PublishAgentDraftInput,
    ) -> async_graphql::Result<AgentDraftMutationPayload> {
        let result = draft_service(ctx)?
            .publish_draft(
                principal(ctx)?,
                input.project_id.as_str(),
                input.agent_id.as_str(),
                input.expected_revision as i64,
                input.warnings_acknowledged,
            )
            .await
            .map_err(|error| async_graphql::Error::new(error.to_string()))?;
        Ok(AgentDraftMutationPayload::from(result))
    }
}
