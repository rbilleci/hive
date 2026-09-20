//! Ports `EvaluationService`: canonicalizes untrusted identifiers before the repository rechecks
//! current authority. Reads are generated Seaography entity queries and no longer pass here.

use uuid::Uuid;

use super::models::{EvaluationMutationResult, EvaluationProblem, WorkerHealth};
use super::repository::{EvaluationRepository, RepositoryError};

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
