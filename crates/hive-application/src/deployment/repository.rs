//! Ports `DeploymentRepository`/`DeploymentOutboxDelivery`: the persistence
//! boundary for tenant-scoped deployment reads, atomic local deployment
//! commands, and the durable outbox worker's single unit of work.
//!
//! `UnavailableDeploymentRepository` (a null-object Java falls back to when
//! Postgres is unreachable at boot) is not ported: the Rust `hive` binary
//! fails fast on a connection error at startup (`ConnectionFactory::connect`
//! propagates via `?`) rather than serving in a degraded no-database mode, so
//! nothing ever constructs a `DeploymentRepository` without a live pool.

use super::compiler::CompiledRequest;
use super::models::{
    ApprovalDecisionConnection, ApprovalDecisionMutationResult, ApprovalInboxConnection,
    ApprovalInboxItem, DeploymentCompilationContext, DeploymentMutationResult,
    DeploymentRecoveryCompilationContext,
};
use super::policy::ApprovalDecisionPlanner;
use async_trait::async_trait;
use hive_domain::deployment::ApprovalDecisionCommand;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

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
    /// version" (Java's own `null` semantics) — not the same outcome as an invalid/unparseable id.
    async fn rollback_compilation_context(
        &self,
        principal_id: Uuid,
        deployment_id: Uuid,
        target_agent_version_id: Option<&str>,
    ) -> Result<Option<DeploymentRecoveryCompilationContext>, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn approval_inbox(
        &self,
        principal_id: Uuid,
        organization_id: Option<Uuid>,
        project_id: Option<Uuid>,
        after: Option<&str>,
        first: i32,
        include_decision_preview: bool,
    ) -> Result<Option<ApprovalInboxConnection>, RepositoryError>;

    async fn approval_detail(
        &self,
        principal_id: Uuid,
        approval_requirement_id: Uuid,
    ) -> Result<Option<ApprovalInboxItem>, RepositoryError>;

    async fn approval_decisions(
        &self,
        principal_id: Uuid,
        approval_requirement_id: Uuid,
        after: Option<&str>,
        first: i32,
    ) -> Result<Option<ApprovalDecisionConnection>, RepositoryError>;

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
    /// active deployment regardless of agent version" (Java's own `null` semantics), a materially
    /// different outcome from an invalid/unparseable id (which resolves no target at all).
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
