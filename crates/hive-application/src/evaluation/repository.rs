//! The authoritative evaluation boundary for authorization, immutable facts, and local worker
//! transitions.

use async_trait::async_trait;
use uuid::Uuid;

use super::models::{
    EvaluationMutationResult, EvaluationWorkDecision, EvaluationWorkItem, WorkerHealth,
};

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Every evaluation *read* is a generated Seaography entity query, so this boundary carries the
/// commands only. A command answers with the stored row itself, which the GraphQL payload exposes
/// as the same generated type the reads use.
#[async_trait]
pub trait EvaluationRepository: Send + Sync {
    /// The stored `evaluation_definitions` row.
    type Definition: Send;
    /// The stored `evaluation_definition_versions` row.
    type Version: Send;
    /// The stored `evaluation_runs` row.
    type Run: Send;

    async fn create_definition(
        &self,
        principal: Uuid,
        project: Uuid,
        slug: &str,
        document: Option<&str>,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult<Self::Definition, Self::Version, Self::Run>, RepositoryError>;

    async fn update_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        document: &str,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult<Self::Definition, Self::Version, Self::Run>, RepositoryError>;

    async fn validate_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult<Self::Definition, Self::Version, Self::Run>, RepositoryError>;

    async fn duplicate_version(
        &self,
        principal: Uuid,
        version: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult<Self::Definition, Self::Version, Self::Run>, RepositoryError>;

    async fn publish_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult<Self::Definition, Self::Version, Self::Run>, RepositoryError>;

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
    ) -> Result<EvaluationMutationResult<Self::Definition, Self::Version, Self::Run>, RepositoryError>;

    async fn cancel(
        &self,
        principal: Uuid,
        run: Uuid,
        expected_generation: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult<Self::Definition, Self::Version, Self::Run>, RepositoryError>;

    async fn rerun(
        &self,
        principal: Uuid,
        source_run: Uuid,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult<Self::Definition, Self::Version, Self::Run>, RepositoryError>;

    async fn worker_health(&self) -> Result<WorkerHealth, RepositoryError>;
}

/// Durable claim, commit, retry, heartbeat, and recovery mechanics for
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
