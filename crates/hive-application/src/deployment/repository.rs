//! The persistence boundary for tenant-scoped deployment reads, atomic local deployment
//! commands, and the durable outbox worker's single unit of work.
//!
//! There is no degraded no-database implementation: the `hive` binary fails fast on a connection
//! error at startup, so nothing ever constructs a `DeploymentRepository` without a live pool.

use super::compiler::CompiledRequest;
use super::models::{
    ApprovalDecisionMutationResult, DeploymentCompilationContext, DeploymentMutationResult,
    DeploymentRecoveryCompilationContext,
};
use super::policy::{ApprovalDecisionCommand, ApprovalDecisionPlanner};
use crate::RepositoryError;
use async_trait::async_trait;
use uuid::Uuid;

/// Persistence boundary for tenant-scoped deployment reads and atomic local deployment commands.
#[async_trait]
pub trait DeploymentRepository: Send + Sync {
    async fn compilation_context(
        &self,
        principal_id: Uuid,
        agent_version_id: Uuid,
        environment_definition_version_id: Uuid,
    ) -> Result<Option<DeploymentCompilationContext>, RepositoryError>;

    async fn retry_compilation_context(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<Option<DeploymentRecoveryCompilationContext>, RepositoryError>;

    /// `None` means "roll back to the most recent prior active deployment regardless of agent
    /// version" — not the same outcome as an invalid or unparseable id.
    async fn rollback_compilation_context(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        target_agent_version_id: Option<&str>,
    ) -> Result<Option<DeploymentRecoveryCompilationContext>, RepositoryError>;

    async fn deploy(
        &self,
        principal_id: Uuid,
        request: &CompiledRequest,
        idempotency_key: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError>;

    async fn cancel(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        expected_revision: i64,
        reason: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError>;

    async fn retry(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
        request: Option<&CompiledRequest>,
    ) -> Result<DeploymentMutationResult, RepositoryError>;

    async fn promote(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError>;

    /// `target_agent_version_id`/`production_confirmation` are genuinely nullable, not empty-string
    /// sentinels: `None` for `target_agent_version_id` means "roll back to the most recent prior
    /// active deployment regardless of agent version", a materially different outcome from an
    /// invalid or unparseable id, which resolves no target at all.
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
    ) -> Result<DeploymentMutationResult, RepositoryError>;

    /// Persists one decision selected by the application-owned, fixed P-05 planner from locked facts.
    async fn record_approval_decision(
        &self,
        command: ApprovalDecisionCommand,
        planner: ApprovalDecisionPlanner,
    ) -> Result<ApprovalDecisionMutationResult, RepositoryError>;
}

/// Durable local-delivery port used by the deterministic worker process.
#[async_trait]
pub trait DeploymentOutboxDelivery: Send + Sync {
    async fn deliver_next(&self, worker_id: &str) -> Result<bool, RepositoryError>;
}
