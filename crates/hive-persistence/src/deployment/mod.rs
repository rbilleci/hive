//! Ports `PostgresDeploymentRepository`/`PostgresDeploymentApprovalEvidenceIssue`/
//! `PostgresEvaluationTargetProjection.projectDeploymentTarget` — the full
//! `DeploymentRepository`/`DeploymentOutboxDelivery` surface: compile-context
//! reads, list/find/timeline/detail/environments, the five deployment
//! mutations (deploy/cancel/retry/promote/rollback), the approval inbox/
//! decision/requirement surface, and the outbox worker's delivery engine.
//!
//! Also ports (RTP-APPROVAL) the scheduled reconciliation entry points
//! `reconcile_approval_expiry`/`reconcile_approval_upgrade` (re-exported below,
//! called by the `serve`-subcommand's 1-second maintenance task) and
//! `record_worker_heartbeat`'s opportunistic maintenance branch (the `if
//! (ready && approvalMaintenanceDue())` block).

mod approval;
mod cursors;
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
    ApprovalDecisionConnection, ApprovalDecisionMutationResult, ApprovalDecisionPlanner,
    ApprovalInboxConnection, ApprovalInboxItem, CompiledRequest, Deployment,
    DeploymentCompilationContext, DeploymentConnection, DeploymentDetailProjection,
    DeploymentEnvironmentConnection, DeploymentFilter, DeploymentMutationResult,
    DeploymentOutboxDelivery, DeploymentRecoveryCompilationContext, DeploymentRepository,
    DeploymentRepositoryError as RepositoryError, DeploymentTimelineConnection,
};
use hive_domain::deployment::ApprovalDecisionCommand;
use sqlx::PgPool;
use uuid::Uuid;

pub struct PgDeploymentRepository {
    pool: PgPool,
    next_approval_maintenance_at: std::sync::atomic::AtomicI64,
}

impl PgDeploymentRepository {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            next_approval_maintenance_at: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// Ports `recordWorkerHeartbeat`. The `deployment-worker` subcommand calls this directly for
    /// its pre/post-batch heartbeats; `self.next_approval_maintenance_at` is this repository
    /// instance's own rate gate, mirroring Java's per-`PostgresDeploymentRepository`-instance
    /// `AtomicLong` (the standalone worker process and the `serve` process each own one).
    pub async fn record_worker_heartbeat(
        &self,
        worker: &str,
        delivered: i32,
        ready: bool,
        failure_code: Option<&str>,
    ) -> Result<(), RepositoryError> {
        worker::record_worker_heartbeat(
            &self.pool,
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

fn other(error: sqlx::Error) -> RepositoryError {
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
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::compilation_context(
            &mut conn,
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
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::recovery_compilation_context(&mut conn, principal_id, deployment_id, None, true)
            .await
            .map_err(other)
    }

    async fn rollback_compilation_context(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        target_agent_version_id: Option<&str>,
    ) -> Result<Option<DeploymentRecoveryCompilationContext>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::recovery_compilation_context(
            &mut conn,
            principal_id,
            deployment_id,
            target_agent_version_id,
            false,
        )
        .await
        .map_err(other)
    }

    async fn list(
        &self,
        principal_id: Uuid,
        filter: &DeploymentFilter,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<DeploymentConnection>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::list(&mut conn, principal_id, filter, after, first)
            .await
            .map_err(other)
    }

    async fn find(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<Option<Deployment>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::find(&mut conn, principal_id, deployment_id)
            .await
            .map_err(other)
    }

    async fn timeline(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<DeploymentTimelineConnection>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::timeline_page(&mut conn, principal_id, deployment_id, after, first)
            .await
            .map_err(other)
    }

    async fn detail(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<DeploymentDetailProjection>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::detail(&mut conn, principal_id, deployment_id, after, first)
            .await
            .map_err(other)
    }

    async fn approval_inbox(
        &self,
        principal_id: Uuid,
        organization_id: Option<Uuid>,
        project_id: Option<Uuid>,
        after: Option<&str>,
        first: i32,
        include_decision_preview: bool,
    ) -> Result<Option<ApprovalInboxConnection>, RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(other)?;
        let result = queries::approval_inbox(
            &mut tx,
            principal_id,
            organization_id,
            project_id,
            after,
            first,
            include_decision_preview,
        )
        .await;
        match result {
            Ok(value) => {
                tx.commit().await.map_err(other)?;
                Ok(value)
            }
            Err(error) => Err(other(error)),
        }
    }

    async fn approval_detail(
        &self,
        principal_id: Uuid,
        approval_requirement_id: Uuid,
    ) -> Result<Option<ApprovalInboxItem>, RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(other)?;
        let result = queries::approval_detail(&mut tx, principal_id, approval_requirement_id).await;
        match result {
            Ok(value) => {
                tx.commit().await.map_err(other)?;
                Ok(value)
            }
            Err(error) => Err(other(error)),
        }
    }

    async fn approval_decisions(
        &self,
        principal_id: Uuid,
        approval_requirement_id: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<ApprovalDecisionConnection>, RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(other)?;
        let result = queries::approval_decisions(
            &mut tx,
            principal_id,
            approval_requirement_id,
            after,
            first,
        )
        .await;
        match result {
            Ok(value) => {
                tx.commit().await.map_err(other)?;
                Ok(value)
            }
            Err(error) => Err(other(error)),
        }
    }

    async fn environments(
        &self,
        principal_id: Uuid,
        agent_version_id: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<DeploymentEnvironmentConnection>, RepositoryError> {
        let mut conn = self.pool.acquire().await.map_err(other)?;
        queries::environments(&mut conn, principal_id, agent_version_id, after, first)
            .await
            .map_err(other)
    }

    async fn deploy(
        &self,
        principal_id: Uuid,
        request: &CompiledRequest,
        idempotency_key: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError> {
        mutations::deploy(&self.pool, principal_id, request, idempotency_key)
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
            &self.pool,
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
            &self.pool,
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
            &self.pool,
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
            &self.pool,
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
        queries::record_approval_decision(&self.pool, command, planner)
            .await
            .map_err(other)
    }
}

#[async_trait]
impl DeploymentOutboxDelivery for PgDeploymentRepository {
    async fn deliver_next(&self, worker_id: &str) -> Result<bool, RepositoryError> {
        worker::deliver_next(&self.pool, worker_id)
            .await
            .map_err(other)
    }
}
