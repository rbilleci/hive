//! Ports `EvaluationService`: canonicalizes untrusted identifiers and page
//! bounds before the repository rechecks current authority.

use uuid::Uuid;

use super::models::{
    Connection, EvaluationArtifactMetadata, EvaluationAuditEvent, EvaluationCaseRun,
    EvaluationDefinition, EvaluationDefinitionConnection, EvaluationDefinitionVersion,
    EvaluationDefinitionVersionConnection, EvaluationMetricResult, EvaluationMutationResult,
    EvaluationProblem, EvaluationRun, EvaluationRunConnection, EvaluationTarget, WorkerHealth,
};
use super::repository::{EvaluationRepository, RepositoryError};
use super::state_machine::EvaluationRunStatus;

const LIST_BOUND: i32 = 50;
pub const DETAIL_BOUND: i32 = 100;

fn valid(first: i32, maximum: i32) -> bool {
    (1..=maximum).contains(&first)
}

fn id(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value).ok()
}

pub struct EvaluationService<R: EvaluationRepository> {
    repository: R,
}

impl<R: EvaluationRepository> EvaluationService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub async fn definitions(
        &self,
        principal: Uuid,
        project: &str,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationDefinitionConnection>, RepositoryError> {
        let (Some(project), true) = (id(project), valid(first, LIST_BOUND)) else {
            return Ok(None);
        };
        self.repository
            .definitions(principal, project, after, first)
            .await
    }

    pub async fn definition(
        &self,
        principal: Uuid,
        definition: &str,
    ) -> Result<Option<EvaluationDefinition>, RepositoryError> {
        let Some(definition) = id(definition) else {
            return Ok(None);
        };
        self.repository.definition(principal, definition).await
    }

    pub async fn definition_version(
        &self,
        principal: Uuid,
        version: &str,
    ) -> Result<Option<EvaluationDefinitionVersion>, RepositoryError> {
        let Some(version) = id(version) else {
            return Ok(None);
        };
        self.repository.definition_version(principal, version).await
    }

    pub async fn definition_versions(
        &self,
        principal: Uuid,
        definition: &str,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationDefinitionVersionConnection>, RepositoryError> {
        let (Some(definition), true) = (id(definition), valid(first, LIST_BOUND)) else {
            return Ok(None);
        };
        self.repository
            .definition_versions(principal, definition, after, first)
            .await
    }

    pub async fn definition_version_usage(
        &self,
        principal: Uuid,
        version: &str,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationRunConnection>, RepositoryError> {
        let (Some(version), true) = (id(version), valid(first, LIST_BOUND)) else {
            return Ok(None);
        };
        self.repository
            .definition_version_usage(principal, version, after, first)
            .await
    }

    pub async fn runs(
        &self,
        principal: Uuid,
        project: &str,
        status: Option<EvaluationRunStatus>,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationRunConnection>, RepositoryError> {
        let (Some(project), true) = (id(project), valid(first, LIST_BOUND)) else {
            return Ok(None);
        };
        self.repository
            .runs(principal, project, status, after, first)
            .await
    }

    pub async fn run(
        &self,
        principal: Uuid,
        run: &str,
    ) -> Result<Option<EvaluationRun>, RepositoryError> {
        let Some(run) = id(run) else {
            return Ok(None);
        };
        self.repository.run(principal, run).await
    }

    pub async fn targets(
        &self,
        principal: Uuid,
        project: &str,
        definition_version: &str,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<Connection<EvaluationTarget>>, RepositoryError> {
        let (Some(project), Some(definition_version), true) = (
            id(project),
            id(definition_version),
            valid(first, LIST_BOUND),
        ) else {
            return Ok(None);
        };
        self.repository
            .targets(principal, project, definition_version, after, first)
            .await
    }

    pub async fn cases(
        &self,
        principal: Uuid,
        run: &str,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<Connection<EvaluationCaseRun>>, RepositoryError> {
        let (Some(run), true) = (id(run), valid(first, DETAIL_BOUND)) else {
            return Ok(None);
        };
        self.repository.cases(principal, run, after, first).await
    }

    pub async fn metrics(
        &self,
        principal: Uuid,
        run: &str,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<Connection<EvaluationMetricResult>>, RepositoryError> {
        let (Some(run), true) = (id(run), valid(first, DETAIL_BOUND)) else {
            return Ok(None);
        };
        self.repository.metrics(principal, run, after, first).await
    }

    pub async fn artifacts(
        &self,
        principal: Uuid,
        run: &str,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<Connection<EvaluationArtifactMetadata>>, RepositoryError> {
        let (Some(run), true) = (id(run), valid(first, DETAIL_BOUND)) else {
            return Ok(None);
        };
        self.repository
            .artifacts(principal, run, after, first)
            .await
    }

    pub async fn audit(
        &self,
        principal: Uuid,
        run: &str,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<Connection<EvaluationAuditEvent>>, RepositoryError> {
        let (Some(run), true) = (id(run), valid(first, DETAIL_BOUND)) else {
            return Ok(None);
        };
        self.repository.audit(principal, run, after, first).await
    }

    pub async fn create_definition(
        &self,
        principal: Uuid,
        project: &str,
        slug: &str,
        document: Option<&str>,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        let Some(project) = id(project) else {
            return Ok(EvaluationMutationResult::refused(
                EvaluationProblem::not_found(),
            ));
        };
        self.repository
            .create_definition(principal, project, slug, document, idempotency_key)
            .await
    }

    pub async fn update_draft(
        &self,
        principal: Uuid,
        definition: &str,
        revision: i64,
        document: &str,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        let Some(definition) = id(definition) else {
            return Ok(EvaluationMutationResult::refused(
                EvaluationProblem::not_found(),
            ));
        };
        self.repository
            .update_draft(principal, definition, revision, document, idempotency_key)
            .await
    }

    pub async fn validate_draft(
        &self,
        principal: Uuid,
        definition: &str,
        revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        let Some(definition) = id(definition) else {
            return Ok(EvaluationMutationResult::refused(
                EvaluationProblem::not_found(),
            ));
        };
        self.repository
            .validate_draft(principal, definition, revision, idempotency_key)
            .await
    }

    pub async fn duplicate_version(
        &self,
        principal: Uuid,
        version: &str,
        revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        let Some(version) = id(version) else {
            return Ok(EvaluationMutationResult::refused(
                EvaluationProblem::not_found(),
            ));
        };
        self.repository
            .duplicate_version(principal, version, revision, idempotency_key)
            .await
    }

    pub async fn publish_draft(
        &self,
        principal: Uuid,
        definition: &str,
        revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        let Some(definition) = id(definition) else {
            return Ok(EvaluationMutationResult::refused(
                EvaluationProblem::not_found(),
            ));
        };
        self.repository
            .publish_draft(principal, definition, revision, idempotency_key)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_evaluation(
        &self,
        principal: Uuid,
        project: &str,
        definition_version: &str,
        target_kind: &str,
        target: &str,
        environment: &str,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        let (Some(project), Some(definition_version), Some(target), Some(environment)) = (
            id(project),
            id(definition_version),
            id(target),
            id(environment),
        ) else {
            return Ok(EvaluationMutationResult::refused(
                EvaluationProblem::not_found(),
            ));
        };
        self.repository
            .run_evaluation(
                principal,
                project,
                definition_version,
                target_kind,
                target,
                environment,
                idempotency_key,
            )
            .await
    }

    pub async fn cancel(
        &self,
        principal: Uuid,
        run: &str,
        generation: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        let Some(run) = id(run) else {
            return Ok(EvaluationMutationResult::refused(
                EvaluationProblem::not_found(),
            ));
        };
        self.repository
            .cancel(principal, run, generation, idempotency_key)
            .await
    }

    pub async fn rerun(
        &self,
        principal: Uuid,
        run: &str,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        let Some(run) = id(run) else {
            return Ok(EvaluationMutationResult::refused(
                EvaluationProblem::not_found(),
            ));
        };
        self.repository.rerun(principal, run, idempotency_key).await
    }

    pub async fn worker_health(&self) -> Result<WorkerHealth, RepositoryError> {
        self.repository.worker_health().await
    }
}
