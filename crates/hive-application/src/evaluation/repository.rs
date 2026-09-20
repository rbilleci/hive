//! Ports `EvaluationRepository`/`EvaluationWorkStore`: the authoritative
//! evaluation boundary for authorization, immutable facts, and local worker
//! transitions.

use async_trait::async_trait;
use uuid::Uuid;

use super::models::{
    Connection, EvaluationArtifactMetadata, EvaluationAuditEvent, EvaluationCaseRun,
    EvaluationDefinition, EvaluationDefinitionConnection, EvaluationDefinitionVersion,
    EvaluationDefinitionVersionConnection, EvaluationMetricResult, EvaluationMutationResult,
    EvaluationRunConnection, EvaluationTarget, EvaluationWorkDecision, EvaluationWorkItem,
    WorkerHealth,
};
use super::state_machine::EvaluationRunStatus;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[async_trait]
pub trait EvaluationRepository: Send + Sync {
    async fn definitions(
        &self,
        principal: Uuid,
        project: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationDefinitionConnection>, RepositoryError>;

    async fn definition(
        &self,
        principal: Uuid,
        definition: Uuid,
    ) -> Result<Option<EvaluationDefinition>, RepositoryError>;

    async fn definition_version(
        &self,
        principal: Uuid,
        version: Uuid,
    ) -> Result<Option<EvaluationDefinitionVersion>, RepositoryError>;

    async fn definition_versions(
        &self,
        principal: Uuid,
        definition: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationDefinitionVersionConnection>, RepositoryError>;

    async fn definition_version_usage(
        &self,
        principal: Uuid,
        version: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationRunConnection>, RepositoryError>;

    async fn runs(
        &self,
        principal: Uuid,
        project: Uuid,
        status: Option<EvaluationRunStatus>,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationRunConnection>, RepositoryError>;

    async fn run(
        &self,
        principal: Uuid,
        run: Uuid,
    ) -> Result<Option<super::models::EvaluationRun>, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn targets(
        &self,
        principal: Uuid,
        project: Uuid,
        definition_version: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<Connection<EvaluationTarget>>, RepositoryError>;

    async fn cases(
        &self,
        principal: Uuid,
        run: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<Connection<EvaluationCaseRun>>, RepositoryError>;

    async fn metrics(
        &self,
        principal: Uuid,
        run: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<Connection<EvaluationMetricResult>>, RepositoryError>;

    async fn artifacts(
        &self,
        principal: Uuid,
        run: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<Connection<EvaluationArtifactMetadata>>, RepositoryError>;

    async fn audit(
        &self,
        principal: Uuid,
        run: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<Connection<EvaluationAuditEvent>>, RepositoryError>;

    async fn create_definition(
        &self,
        principal: Uuid,
        project: Uuid,
        slug: &str,
        document: Option<&str>,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError>;

    async fn update_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        document: &str,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError>;

    async fn validate_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError>;

    async fn duplicate_version(
        &self,
        principal: Uuid,
        version: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError>;

    async fn publish_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn run_evaluation(
        &self,
        principal: Uuid,
        project: Uuid,
        definition_version: Uuid,
        target_kind: &str,
        target_id: Uuid,
        environment_definition_version: Uuid,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError>;

    async fn cancel(
        &self,
        principal: Uuid,
        run: Uuid,
        expected_generation: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError>;

    async fn rerun(
        &self,
        principal: Uuid,
        source_run: Uuid,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError>;

    async fn worker_health(&self) -> Result<WorkerHealth, RepositoryError>;
}

/// Ports `EvaluationWorkStore`: durable claim, commit, retry, heartbeat, and recovery mechanics for
/// local evaluation work.
#[async_trait]
pub trait EvaluationWorkStore: Send + Sync {
    async fn claim_next(
        &self,
        worker_id: &str,
    ) -> Result<Option<EvaluationWorkItem>, RepositoryError>;

    async fn commit(
        &self,
        worker_id: &str,
        work: &EvaluationWorkItem,
        decision: EvaluationWorkDecision,
    ) -> Result<(), RepositoryError>;

    async fn idle(&self, worker_id: &str) -> Result<(), RepositoryError>;

    async fn delivered(&self, worker_id: &str) -> Result<(), RepositoryError>;

    async fn failed(
        &self,
        worker_id: &str,
        work: &EvaluationWorkItem,
        decision: super::models::EvaluationExecutionDecision,
    ) -> Result<(), RepositoryError>;

    async fn claim_failed(&self, worker_id: &str) -> Result<(), RepositoryError>;

    async fn worker_health(&self) -> Result<WorkerHealth, RepositoryError>;
}
