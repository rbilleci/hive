//! The evaluation mutations and the local outbox worker's claim/commit/heartbeat cycle. Every
//! evaluation read the API exposes is a generated Seaography entity query; the computed fields
//! those entity objects carry are in `computed`. `queries` holds the few reads that stay
//! repository methods, and `rows` the row types, parsers and mappers the commands, the worker and
//! the computed fields share.

pub mod computed;
mod mutations;
mod queries;
mod rows;
mod worker;

use async_trait::async_trait;
use hive_application::evaluation::{
    EvaluationExecutionDecision, EvaluationMutationResult, EvaluationRepository,
    EvaluationWorkDecision, EvaluationWorkItem, EvaluationWorkStore, WorkerHealth,
};
use hive_application::RepositoryError;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::entity::{evaluation_definition_versions, evaluation_definitions, evaluation_runs};
use crate::error::repository_error;
use mutations::MutationResult;

/// A storage failure inside an evaluation mutation refuses the command with `UNAVAILABLE` in the
/// payload, so the console renders a problem the way it does for every other refusal instead of a
/// transport-level GraphQL error.
fn refuse_on_storage_failure(
    result: Result<MutationResult, sea_orm::DbErr>,
) -> Result<MutationResult, RepositoryError> {
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
    type Definition = evaluation_definitions::Model;
    type Version = evaluation_definition_versions::Model;
    type Run = evaluation_runs::Model;

    async fn create_definition(
        &self,
        principal: Uuid,
        project: Uuid,
        slug: &str,
        document: Option<&str>,
        idempotency_key: &str,
    ) -> Result<MutationResult, RepositoryError> {
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
    ) -> Result<MutationResult, RepositoryError> {
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
    ) -> Result<MutationResult, RepositoryError> {
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
    ) -> Result<MutationResult, RepositoryError> {
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
    ) -> Result<MutationResult, RepositoryError> {
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
    ) -> Result<MutationResult, RepositoryError> {
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
    ) -> Result<MutationResult, RepositoryError> {
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
    ) -> Result<MutationResult, RepositoryError> {
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
        worker::claim_next(&self.db, worker_id)
            .await
            .map_err(repository_error)
    }

    async fn commit(
        &self,
        worker_id: &str,
        work: &EvaluationWorkItem,
        decision: EvaluationWorkDecision,
    ) -> Result<(), RepositoryError> {
        worker::commit(&self.db, worker_id, work, decision)
            .await
            .map_err(repository_error)
    }

    async fn idle(&self, worker_id: &str) -> Result<(), RepositoryError> {
        worker::idle(&self.db, worker_id)
            .await
            .map_err(repository_error)
    }

    async fn delivered(&self, worker_id: &str) -> Result<(), RepositoryError> {
        worker::delivered(&self.db, worker_id)
            .await
            .map_err(repository_error)
    }

    async fn failed(
        &self,
        worker_id: &str,
        work: &EvaluationWorkItem,
        decision: EvaluationExecutionDecision,
    ) -> Result<(), RepositoryError> {
        worker::failed(&self.db, worker_id, work, &decision)
            .await
            .map_err(repository_error)
    }

    async fn claim_failed(&self, worker_id: &str) -> Result<(), RepositoryError> {
        worker::claim_failed(&self.db, worker_id)
            .await
            .map_err(repository_error)
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
