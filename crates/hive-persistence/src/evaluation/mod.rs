//! Ports `PostgresEvaluationRepository`/`PostgresEvaluationWorkStore`: the
//! full `EvaluationRepository`/`EvaluationWorkStore` surface — 12 reads, 8
//! mutations, and the local outbox worker's claim/commit/heartbeat cycle.

mod cursors;
mod mutations;
mod queries;
mod rows;
mod worker;

use async_trait::async_trait;
use hive_application::evaluation::EvaluationRunStatus;
use hive_application::evaluation::{
    Connection as AppConnection, EvaluationArtifactMetadata, EvaluationAuditEvent,
    EvaluationCaseRun, EvaluationDefinition, EvaluationDefinitionConnection,
    EvaluationDefinitionVersion, EvaluationDefinitionVersionConnection,
    EvaluationExecutionDecision, EvaluationMetricResult, EvaluationMutationResult,
    EvaluationRepository, EvaluationRun, EvaluationRunConnection, EvaluationTarget,
    EvaluationWorkDecision, EvaluationWorkItem, EvaluationWorkStore, RepositoryError, WorkerHealth,
};
use sqlx::PgPool;
use uuid::Uuid;

fn other(error: sqlx::Error) -> RepositoryError {
    RepositoryError::Other(error.into())
}

pub struct PgEvaluationRepository {
    pool: PgPool,
}

impl PgEvaluationRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl EvaluationRepository for PgEvaluationRepository {
    async fn definitions(
        &self,
        principal: Uuid,
        project: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationDefinitionConnection>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::definitions(&mut conn, principal, project, after, first)
            .await
            .map_err(other)
    }

    async fn definition(
        &self,
        principal: Uuid,
        definition: Uuid,
    ) -> Result<Option<EvaluationDefinition>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::definition(&mut conn, principal, definition, false)
            .await
            .map_err(other)
    }

    async fn definition_version(
        &self,
        principal: Uuid,
        version: Uuid,
    ) -> Result<Option<EvaluationDefinitionVersion>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::definition_version(&mut conn, principal, version)
            .await
            .map_err(other)
    }

    async fn definition_versions(
        &self,
        principal: Uuid,
        definition: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationDefinitionVersionConnection>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::definition_versions(&mut conn, principal, definition, after, first)
            .await
            .map_err(other)
    }

    async fn definition_version_usage(
        &self,
        principal: Uuid,
        version: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationRunConnection>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::definition_version_usage(&mut conn, principal, version, after, first)
            .await
            .map_err(other)
    }

    async fn runs(
        &self,
        principal: Uuid,
        project: Uuid,
        status: Option<EvaluationRunStatus>,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<EvaluationRunConnection>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::runs(&mut conn, principal, project, status, after, first)
            .await
            .map_err(other)
    }

    async fn run(
        &self,
        principal: Uuid,
        run: Uuid,
    ) -> Result<Option<EvaluationRun>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::run(&mut conn, principal, run, false)
            .await
            .map_err(other)
    }

    async fn targets(
        &self,
        principal: Uuid,
        project: Uuid,
        definition_version: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<AppConnection<EvaluationTarget>>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::targets(
            &mut conn,
            principal,
            project,
            definition_version,
            after,
            first,
        )
        .await
        .map_err(other)
    }

    async fn cases(
        &self,
        principal: Uuid,
        run: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<AppConnection<EvaluationCaseRun>>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::cases(&mut conn, principal, run, after, first)
            .await
            .map_err(other)
    }

    async fn metrics(
        &self,
        principal: Uuid,
        run: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<AppConnection<EvaluationMetricResult>>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::metrics(&mut conn, principal, run, after, first)
            .await
            .map_err(other)
    }

    async fn artifacts(
        &self,
        principal: Uuid,
        run: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<AppConnection<EvaluationArtifactMetadata>>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::artifacts(&mut conn, principal, run, after, first)
            .await
            .map_err(other)
    }

    async fn audit(
        &self,
        principal: Uuid,
        run: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<AppConnection<EvaluationAuditEvent>>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::audit(&mut conn, principal, run, after, first)
            .await
            .map_err(other)
    }

    async fn create_definition(
        &self,
        principal: Uuid,
        project: Uuid,
        slug: &str,
        document: Option<&str>,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        mutations::create_definition(
            &self.pool,
            principal,
            project,
            slug,
            document,
            idempotency_key,
        )
        .await
        .map_err(other)
    }

    async fn update_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        document: &str,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        mutations::update_draft(
            &self.pool,
            principal,
            definition,
            expected_revision,
            document,
            idempotency_key,
        )
        .await
        .map_err(other)
    }

    async fn validate_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        mutations::validate_draft(
            &self.pool,
            principal,
            definition,
            expected_revision,
            idempotency_key,
        )
        .await
        .map_err(other)
    }

    async fn duplicate_version(
        &self,
        principal: Uuid,
        version: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        mutations::duplicate_version(
            &self.pool,
            principal,
            version,
            expected_revision,
            idempotency_key,
        )
        .await
        .map_err(other)
    }

    async fn publish_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        mutations::publish_draft(
            &self.pool,
            principal,
            definition,
            expected_revision,
            idempotency_key,
        )
        .await
        .map_err(other)
    }

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
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        mutations::run_evaluation(
            &self.pool,
            principal,
            project,
            definition_version,
            target_kind,
            target_id,
            environment_definition_version,
            idempotency_key,
        )
        .await
        .map_err(other)
    }

    async fn cancel(
        &self,
        principal: Uuid,
        run: Uuid,
        expected_generation: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        mutations::cancel(
            &self.pool,
            principal,
            run,
            expected_generation,
            idempotency_key,
        )
        .await
        .map_err(other)
    }

    async fn rerun(
        &self,
        principal: Uuid,
        source_run: Uuid,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        mutations::rerun(&self.pool, principal, source_run, idempotency_key)
            .await
            .map_err(other)
    }

    async fn worker_health(&self) -> Result<WorkerHealth, RepositoryError> {
        let health = crate::worker_health::evaluation_worker_health(&self.pool).await;
        Ok(WorkerHealth {
            status: health.status.to_string(),
            pending_events: health.pending_events,
            failure_code: health.failure_code,
        })
    }
}

pub struct PgEvaluationWorkStore {
    pool: PgPool,
}

impl PgEvaluationWorkStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl EvaluationWorkStore for PgEvaluationWorkStore {
    async fn claim_next(
        &self,
        worker_id: &str,
    ) -> Result<Option<EvaluationWorkItem>, RepositoryError> {
        worker::claim_next(&self.pool, worker_id)
            .await
            .map_err(other)
    }

    async fn commit(
        &self,
        worker_id: &str,
        work: &EvaluationWorkItem,
        decision: EvaluationWorkDecision,
    ) -> Result<(), RepositoryError> {
        worker::commit(&self.pool, worker_id, work, decision)
            .await
            .map_err(other)
    }

    async fn idle(&self, worker_id: &str) -> Result<(), RepositoryError> {
        worker::idle(&self.pool, worker_id).await.map_err(other)
    }

    async fn delivered(&self, worker_id: &str) -> Result<(), RepositoryError> {
        worker::delivered(&self.pool, worker_id)
            .await
            .map_err(other)
    }

    async fn failed(
        &self,
        worker_id: &str,
        work: &EvaluationWorkItem,
        decision: EvaluationExecutionDecision,
    ) -> Result<(), RepositoryError> {
        worker::failed(&self.pool, worker_id, work, &decision)
            .await
            .map_err(other)
    }

    async fn claim_failed(&self, worker_id: &str) -> Result<(), RepositoryError> {
        worker::claim_failed(&self.pool, worker_id)
            .await
            .map_err(other)
    }

    async fn worker_health(&self) -> Result<WorkerHealth, RepositoryError> {
        let health = crate::worker_health::evaluation_worker_health(&self.pool).await;
        Ok(WorkerHealth {
            status: health.status.to_string(),
            pending_events: health.pending_events,
            failure_code: health.failure_code,
        })
    }
}
