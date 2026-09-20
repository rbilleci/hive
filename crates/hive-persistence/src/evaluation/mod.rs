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
use sea_orm::DatabaseConnection;
use uuid::Uuid;

fn other(error: sea_orm::DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

/// Ports `PostgresEvaluationRepository.command()`: a storage failure inside an evaluation mutation
/// refuses the command with `UNAVAILABLE` in the payload, so the console renders a problem the way
/// it does for every other refusal instead of a transport-level GraphQL error.
fn refuse_on_storage_failure(
    result: Result<EvaluationMutationResult, sea_orm::DbErr>,
) -> Result<EvaluationMutationResult, RepositoryError> {
    Ok(result.unwrap_or_else(|error| {
        tracing::error!(%error, "evaluation mutation: storage failure");
        EvaluationMutationResult::refused(
            hive_application::evaluation::EvaluationProblem::unavailable(),
        )
    }))
}

pub struct PgEvaluationRepository {
    db: DatabaseConnection,
}

impl PgEvaluationRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
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
        queries::definitions(&self.db, principal, project, after, first)
            .await
            .map_err(other)
    }

    async fn definition(
        &self,
        principal: Uuid,
        definition: Uuid,
    ) -> Result<Option<EvaluationDefinition>, RepositoryError> {
        queries::definition(&self.db, principal, definition, false)
            .await
            .map_err(other)
    }

    async fn definition_version(
        &self,
        principal: Uuid,
        version: Uuid,
    ) -> Result<Option<EvaluationDefinitionVersion>, RepositoryError> {
        queries::definition_version(&self.db, principal, version)
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
        queries::definition_versions(&self.db, principal, definition, after, first)
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
        queries::definition_version_usage(&self.db, principal, version, after, first)
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
        queries::runs(&self.db, principal, project, status, after, first)
            .await
            .map_err(other)
    }

    async fn run(
        &self,
        principal: Uuid,
        run: Uuid,
    ) -> Result<Option<EvaluationRun>, RepositoryError> {
        queries::run(&self.db, principal, run, false)
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
        queries::targets(
            &self.db,
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
        queries::cases(&self.db, principal, run, after, first)
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
        queries::metrics(&self.db, principal, run, after, first)
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
        queries::artifacts(&self.db, principal, run, after, first)
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
        queries::audit(&self.db, principal, run, after, first)
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
        refuse_on_storage_failure(
            mutations::create_definition(
                &self.db,
                principal,
                project,
                slug,
                document,
                idempotency_key,
            )
            .await,
        )
    }

    async fn update_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        document: &str,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        refuse_on_storage_failure(
            mutations::update_draft(
                &self.db,
                principal,
                definition,
                expected_revision,
                document,
                idempotency_key,
            )
            .await,
        )
    }

    async fn validate_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        refuse_on_storage_failure(
            mutations::validate_draft(
                &self.db,
                principal,
                definition,
                expected_revision,
                idempotency_key,
            )
            .await,
        )
    }

    async fn duplicate_version(
        &self,
        principal: Uuid,
        version: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        refuse_on_storage_failure(
            mutations::duplicate_version(
                &self.db,
                principal,
                version,
                expected_revision,
                idempotency_key,
            )
            .await,
        )
    }

    async fn publish_draft(
        &self,
        principal: Uuid,
        definition: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        refuse_on_storage_failure(
            mutations::publish_draft(
                &self.db,
                principal,
                definition,
                expected_revision,
                idempotency_key,
            )
            .await,
        )
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
        refuse_on_storage_failure(
            mutations::run_evaluation(
                &self.db,
                principal,
                project,
                definition_version,
                target_kind,
                target_id,
                environment_definition_version,
                idempotency_key,
            )
            .await,
        )
    }

    async fn cancel(
        &self,
        principal: Uuid,
        run: Uuid,
        expected_generation: i64,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        refuse_on_storage_failure(
            mutations::cancel(
                &self.db,
                principal,
                run,
                expected_generation,
                idempotency_key,
            )
            .await,
        )
    }

    async fn rerun(
        &self,
        principal: Uuid,
        source_run: Uuid,
        idempotency_key: &str,
    ) -> Result<EvaluationMutationResult, RepositoryError> {
        refuse_on_storage_failure(
            mutations::rerun(&self.db, principal, source_run, idempotency_key).await,
        )
    }

    async fn worker_health(&self) -> Result<WorkerHealth, RepositoryError> {
        let health = crate::worker_health::evaluation_worker_health(&self.db).await;
        Ok(WorkerHealth {
            status: health.status.to_string(),
            pending_events: health.pending_events,
            failure_code: health.failure_code,
        })
    }
}

pub struct PgEvaluationWorkStore {
    db: DatabaseConnection,
}

impl PgEvaluationWorkStore {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl EvaluationWorkStore for PgEvaluationWorkStore {
    async fn claim_next(
        &self,
        worker_id: &str,
    ) -> Result<Option<EvaluationWorkItem>, RepositoryError> {
        worker::claim_next(&self.db, worker_id).await.map_err(other)
    }

    async fn commit(
        &self,
        worker_id: &str,
        work: &EvaluationWorkItem,
        decision: EvaluationWorkDecision,
    ) -> Result<(), RepositoryError> {
        worker::commit(&self.db, worker_id, work, decision)
            .await
            .map_err(other)
    }

    async fn idle(&self, worker_id: &str) -> Result<(), RepositoryError> {
        worker::idle(&self.db, worker_id).await.map_err(other)
    }

    async fn delivered(&self, worker_id: &str) -> Result<(), RepositoryError> {
        worker::delivered(&self.db, worker_id).await.map_err(other)
    }

    async fn failed(
        &self,
        worker_id: &str,
        work: &EvaluationWorkItem,
        decision: EvaluationExecutionDecision,
    ) -> Result<(), RepositoryError> {
        worker::failed(&self.db, worker_id, work, &decision)
            .await
            .map_err(other)
    }

    async fn claim_failed(&self, worker_id: &str) -> Result<(), RepositoryError> {
        worker::claim_failed(&self.db, worker_id)
            .await
            .map_err(other)
    }

    async fn worker_health(&self) -> Result<WorkerHealth, RepositoryError> {
        let health = crate::worker_health::evaluation_worker_health(&self.db).await;
        Ok(WorkerHealth {
            status: health.status.to_string(),
            pending_events: health.pending_events,
            failure_code: health.failure_code,
        })
    }
}
