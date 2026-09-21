//! The whole `DeploymentRepository`/`DeploymentOutboxDelivery` surface: compile-context reads,
//! the five deployment mutations (deploy, cancel, retry, promote, rollback), the approval
//! inbox/decision/requirement surface, and the outbox worker's delivery engine.
//!
//! The scheduled reconciliation entry points `reconcile_approval_expiry` and
//! `reconcile_approval_upgrade` are re-exported below for the `serve` subcommand's 1-second
//! maintenance task; `record_worker_heartbeat` runs the same maintenance opportunistically when a
//! ready worker finds it due.

mod approval;
pub mod computed;
mod mutations;
mod queries;
mod rows;
mod worker;
mod writes;

pub use approval::{
    automatic_approval_handoff, reconcile_approval_expiry, reconcile_approval_upgrade,
    waiting_for_evaluation,
};
pub use writes::touch_projection;

use async_trait::async_trait;
use hive_application::deployment::{
    ApprovalDecisionMutationResult, ApprovalDecisionPlanner, CompiledRequest,
    DeploymentCompilationContext, DeploymentMutationResult, DeploymentOutboxDelivery,
    DeploymentRecoveryCompilationContext, DeploymentRepository,
    DeploymentRepositoryError as RepositoryError,
};
use hive_domain::deployment::ApprovalDecisionCommand;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

pub struct PgDeploymentRepository {
    db: DatabaseConnection,
    next_approval_maintenance_at: std::sync::atomic::AtomicI64,
}

impl PgDeploymentRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self {
            db,
            next_approval_maintenance_at: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// The `deployment-worker` subcommand calls this directly for its pre- and post-batch
    /// heartbeats. `self.next_approval_maintenance_at` rate-gates the opportunistic maintenance
    /// branch per repository instance, so the standalone worker process and the `serve` process
    /// each hold their own gate.
    pub async fn record_worker_heartbeat(
        &self,
        worker: &str,
        delivered: i32,
        ready: bool,
        failure_code: Option<&str>,
    ) -> Result<(), RepositoryError> {
        worker::record_worker_heartbeat(
            &self.db,
            worker,
            delivered,
            ready,
            failure_code,
            &self.next_approval_maintenance_at,
        )
        .await
        .map_err(other)
    }
}

fn other(error: sea_orm::DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

pub const MAX_WORKER_DELIVERIES: i32 = 3;

#[async_trait]
impl DeploymentRepository for PgDeploymentRepository {
    async fn compilation_context(
        &self,
        principal_id: Uuid,
        agent_version_id: Uuid,
        environment_definition_version_id: Uuid,
    ) -> Result<Option<DeploymentCompilationContext>, RepositoryError> {
        queries::compilation_context(
            &self.db,
            principal_id,
            agent_version_id,
            environment_definition_version_id,
        )
        .await
        .map_err(other)
    }

    async fn retry_compilation_context(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<Option<DeploymentRecoveryCompilationContext>, RepositoryError> {
        queries::recovery_compilation_context(&self.db, principal_id, deployment_id, None, true)
            .await
            .map_err(other)
    }

    async fn rollback_compilation_context(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        target_agent_version_id: Option<&str>,
    ) -> Result<Option<DeploymentRecoveryCompilationContext>, RepositoryError> {
        queries::recovery_compilation_context(
            &self.db,
            principal_id,
            deployment_id,
            target_agent_version_id,
            false,
        )
        .await
        .map_err(other)
    }

    async fn deploy(
        &self,
        principal_id: Uuid,
        request: &CompiledRequest,
        idempotency_key: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError> {
        mutations::deploy(&self.db, principal_id, request, idempotency_key)
            .await
            .map_err(other)
    }

    async fn cancel(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        expected_revision: i64,
        reason: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError> {
        mutations::cancel(
            &self.db,
            principal_id,
            deployment_id,
            expected_revision,
            reason,
        )
        .await
        .map_err(other)
    }

    async fn retry(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
        request: Option<&CompiledRequest>,
    ) -> Result<DeploymentMutationResult, RepositoryError> {
        mutations::recovery(
            &self.db,
            principal_id,
            deployment_id,
            expected_revision,
            idempotency_key,
            mutations::RecoveryAction::Retry,
            None,
            None,
            None,
            request,
        )
        .await
        .map_err(other)
    }

    async fn promote(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError> {
        mutations::promote(
            &self.db,
            principal_id,
            deployment_id,
            expected_revision,
            idempotency_key,
        )
        .await
        .map_err(other)
    }

    #[allow(clippy::too_many_arguments)]
    async fn rollback(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        target_agent_version_id: Option<&str>,
        expected_revision: i64,
        reason: &str,
        production_confirmation: Option<&str>,
        idempotency_key: &str,
        request: Option<&CompiledRequest>,
    ) -> Result<DeploymentMutationResult, RepositoryError> {
        mutations::recovery(
            &self.db,
            principal_id,
            deployment_id,
            expected_revision,
            idempotency_key,
            mutations::RecoveryAction::Rollback,
            target_agent_version_id,
            Some(reason),
            production_confirmation,
            request,
        )
        .await
        .map_err(other)
    }

    async fn record_approval_decision(
        &self,
        command: ApprovalDecisionCommand,
        planner: ApprovalDecisionPlanner,
    ) -> Result<ApprovalDecisionMutationResult, RepositoryError> {
        queries::record_approval_decision(&self.db, command, planner)
            .await
            .map_err(other)
    }
}

#[async_trait]
impl DeploymentOutboxDelivery for PgDeploymentRepository {
    async fn deliver_next(&self, worker_id: &str) -> Result<bool, RepositoryError> {
        worker::deliver_next(&self.db, worker_id)
            .await
            .map_err(other)
    }
}
