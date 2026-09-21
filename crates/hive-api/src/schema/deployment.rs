//! What is left of the hand-built deployment tier: the 5 deployment mutations and
//! `decideDeploymentApproval`. Every read is generated.
//!
//! `deploymentPreview` is gone too: the frozen-inputs preview is a computation over the agent
//! version, the environment definition version and the project's approval policy, so it is the
//! computed `AgentVersions.deploymentPreview(environmentDefinitionVersionId, strategy)` field
//! (`hive_persistence::deployment::computed`, A4). It sits on the agent version because the
//! deleted query gated itself on `DEPLOYMENT.VIEW` at the *version's* project, where an
//! environment definition version is an ownerless catalog row visible far more widely.
//!
//! The deployment *reads* and the approval *reads* are deleted: `deployments`,
//! `deploymentProjection`, the deprecated `deploymentTimeline`,
//! `deploymentEnvironmentDefinitionVersions`, `approvalInbox`, `approvalRequirement` and the
//! nested `ApprovalRequirement.decisions` are generated entity queries over `deployments`,
//! `environment_definition_versions`, `deployment_approval_requirements` and
//! `deployment_approval_decisions` now (`docs/idiomatic-seaography-plan.md`, A2), with the nested
//! structures answered by relations and by the computed fields in
//! `hive_persistence::deployment::computed`. Every payload that carried a deployment, a
//! requirement or a decision — the five mutations and `decideDeploymentApproval` — returns the
//! generated object itself (A5), so one console fragment covers all of them.
//!
//! The commands list their refusals with the shared `Problem` type (`schema/problem.rs`), whose
//! `code` is the stable, machine-readable reason; the `DeploymentProblem` and
//! `DeploymentApprovalProblem` interfaces and their ten concrete types are gone.
//!
//! 10 enums, the most of any file — same `scalars::wire_enum!` pattern `evaluation.rs` established,
//! variants spelled in full SCREAMING_SNAKE_CASE. Five of them
//! (`DeploymentLifecycleStatus`, `DeploymentAttemptStatus`, `DeploymentRuntimeHealthStatus`,
//! `ApprovalEvidenceState`, `ApprovalRequirementStatus`) — and now `DeploymentRiskLevel`,
//! `ApprovalEvidenceKind` and `LogicalEnvironmentClass`, which the deleted preview type was the
//! last field of — have no field of their own any more: the
//! columns behind them are `TEXT` with a `CHECK`, so the generated objects expose them as `String`
//! (the plan's "text enums stay strings" finding). They stay registered as the wire vocabulary the
//! console's `cynic::Enum`s are checked against, exactly as `EvaluationRunStatus` does, so a value
//! the server adds or removes fails the console build.

use crate::schema::problem::Problem;
use crate::schema::scalars::{wire_enum, Id, Long};
use crate::schema::{RequestCorrelationId, RequestPrincipal};
use hive_application::deployment::{
    ApprovalDecisionMutationResult as AppDecisionMutationResult,
    ApprovalDecisionProblem as AppDecisionProblem, DeploymentMutationResult as AppMutationResult,
    DeploymentOutcome as AppOutcome, DeploymentProblem as AppProblem,
    DeploymentProblemKind as AppProblemKind, DeploymentService,
};
use hive_persistence::deployment::PgDeploymentRepository;
use hive_persistence::entity::{
    deployment_approval_decisions, deployment_approval_requirements, deployments,
};
use sea_orm::EntityTrait;
use seaography::{CustomFields, CustomInputType, CustomOutputType};
use uuid::Uuid;

fn deployment_service(
    ctx: &async_graphql::Context<'_>,
) -> async_graphql::Result<DeploymentService<PgDeploymentRepository>> {
    let repository =
        PgDeploymentRepository::new(ctx.data::<sea_orm::DatabaseConnection>()?.clone());
    Ok(DeploymentService::new(repository))
}

fn principal(ctx: &async_graphql::Context<'_>) -> async_graphql::Result<Uuid> {
    Ok(ctx.data::<RequestPrincipal>()?.0)
}

/// The generated `Deployments` row a command or an approval fact names. Every payload here returns
/// the entity itself, so the console reads one fragment and the object's own relations and
/// computed fields answer the nested plan, policy, attempt, health and rollback facts.
async fn deployment_row(
    ctx: &async_graphql::Context<'_>,
    id: Uuid,
) -> async_graphql::Result<Option<deployments::Model>> {
    let db = ctx.data::<sea_orm::DatabaseConnection>()?;
    deployments::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(map_error)
}

/// The generated `DeploymentApprovalRequirements` row a decision names.
async fn requirement_row(
    ctx: &async_graphql::Context<'_>,
    id: Uuid,
) -> async_graphql::Result<Option<deployment_approval_requirements::Model>> {
    let db = ctx.data::<sea_orm::DatabaseConnection>()?;
    deployment_approval_requirements::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(map_error)
}

/// The generated `DeploymentApprovalDecisions` row a decision names.
async fn decision_row(
    ctx: &async_graphql::Context<'_>,
    id: Uuid,
) -> async_graphql::Result<Option<deployment_approval_decisions::Model>> {
    let db = ctx.data::<sea_orm::DatabaseConnection>()?;
    deployment_approval_decisions::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(map_error)
}

// Every deployment service error is a storage failure (`RepositoryError` has no other variant), so
// each one carries the marker that makes the handler answer `503`.
fn map_error(error: impl std::fmt::Display) -> async_graphql::Error {
    async_graphql::Error::new_with_source(crate::schema::DependencyUnavailable(error.to_string()))
}

const INVALID_ID: &str = "INVALID_ID";
const NONE: &str = "NONE";

/// Ports `identifier(Object)`: only a well-formed UUID is echoed into an operational log line.
fn log_identifier(value: &str) -> String {
    Uuid::parse_str(value).map_or_else(|_| INVALID_ID.to_string(), |id| id.to_string())
}

/// Ports `permitInvalidSourceLog`: at most one line per second per action for a malformed source
/// identifier, so a caller cannot flood the log with rejected input.
fn permit_invalid_source_log(action: &str) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;
    use std::time::Instant;
    static STARTED: OnceLock<Instant> = OnceLock::new();
    static NEXT_RETRY: AtomicU64 = AtomicU64::new(0);
    static NEXT_PROMOTE: AtomicU64 = AtomicU64::new(0);
    static NEXT_ROLLBACK: AtomicU64 = AtomicU64::new(0);
    let next = match action {
        "RETRY" => &NEXT_RETRY,
        "PROMOTE" => &NEXT_PROMOTE,
        _ => &NEXT_ROLLBACK,
    };
    let now = STARTED.get_or_init(Instant::now).elapsed().as_nanos() as u64;
    let mut recorded = next.load(Ordering::Relaxed);
    loop {
        if now < recorded {
            return false;
        }
        match next.compare_exchange_weak(
            recorded,
            now + 1_000_000_000,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(actual) => recorded = actual,
        }
    }
}

/// Ports `recoveryPayload`/`logRecovery`: one `event=deployment_recovery` line per retry,
/// promotion, and rollback, naming the outcome, the refusal, and both deployment identifiers.
fn log_recovery<E>(
    action: &str,
    ctx: &async_graphql::Context<'_>,
    source_deployment_id: &str,
    result: &Result<AppMutationResult, E>,
) {
    let source = log_identifier(source_deployment_id);
    if source == INVALID_ID && !permit_invalid_source_log(action) {
        return;
    }
    let correlation = ctx
        .data::<RequestCorrelationId>()
        .map_or_else(|_| INVALID_ID.to_string(), |id| id.0.to_string());
    match result {
        Ok(result) => {
            let outcome = match result.outcome {
                AppOutcome::Accepted => "ACCEPTED",
                AppOutcome::Replayed => "REPLAYED",
                AppOutcome::Refused => "REFUSED",
            };
            let problem_code = result
                .problem
                .as_ref()
                .map_or(NONE, |problem| problem_kind_name(&problem.kind));
            let result_id = result
                .deployment
                .as_ref()
                .map_or_else(|| NONE.to_string(), |deployment| deployment.id.to_string());
            tracing::info!(
                "event=deployment_recovery action={action} outcome={outcome} problemCode={problem_code} failureClass={NONE} requestCorrelationId={correlation} sourceDeploymentId={source} resultDeploymentId={result_id}"
            );
        }
        Err(_) => tracing::warn!(
            "event=deployment_recovery action={action} outcome=UNAVAILABLE problemCode={NONE} failureClass=DEPENDENCY_UNAVAILABLE requestCorrelationId={correlation} sourceDeploymentId={source} resultDeploymentId={NONE}"
        ),
    }
}

fn problem_kind_name(kind: &AppProblemKind) -> &'static str {
    match kind {
        AppProblemKind::NotFound => "NOT_FOUND",
        AppProblemKind::Forbidden => "FORBIDDEN",
        AppProblemKind::InvalidInput => "INVALID_INPUT",
        AppProblemKind::ReasonRequired => "REASON_REQUIRED",
        AppProblemKind::ConfirmationRequired => "CONFIRMATION_REQUIRED",
        AppProblemKind::ConfirmationMismatch => "CONFIRMATION_MISMATCH",
        AppProblemKind::RevisionConflict => "REVISION_CONFLICT",
        AppProblemKind::LifecycleConflict => "LIFECYCLE_CONFLICT",
        AppProblemKind::IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
        AppProblemKind::RateLimited => "RATE_LIMITED",
    }
}

#[allow(non_snake_case)]
mod wire {
    use super::*;

    macro_rules! screaming_enum {
        ($name:ident { $($variant:ident),+ $(,)? }) => {
            // Both allows exist for the same reason across every enum in this file — see
            // `evaluation.rs`'s equivalent comment for the full rationale.
            #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
            #[derive(seaography::CustomEnum, Clone, Copy, Eq, PartialEq)]
            pub enum $name {
                $($variant),+
            }
            wire_enum!($name { $($variant),+ });
        };
    }

    screaming_enum!(DeploymentStrategy {
        REPLACE,
        ROLLING,
        BLUE_GREEN,
        CANARY
    });
    impl DeploymentStrategy {
        fn value(self) -> &'static str {
            match self {
                Self::REPLACE => "REPLACE",
                Self::ROLLING => "ROLLING",
                Self::BLUE_GREEN => "BLUE_GREEN",
                Self::CANARY => "CANARY",
            }
        }
    }

    screaming_enum!(DeploymentLifecycleStatus {
        REQUESTED,
        AWAITING_APPROVAL,
        APPROVED,
        IN_PROGRESS,
        ACTIVE,
        FAILED,
        CANCELED,
        ROLLED_BACK,
    });

    // `DeploymentAttemptStatus`, `DeploymentLifecycleStatus` and `DeploymentRuntimeHealthStatus`
    // have no field of their own any more: the columns behind them are `TEXT` with a `CHECK`, so
    // the generated objects expose them as `String` (the plan's "text enums stay strings" finding).
    // They stay registered as the wire vocabulary the console's `cynic::Enum`s are checked
    // against, exactly as `EvaluationRunStatus` does, so a value the server adds or removes fails
    // the console build.
    screaming_enum!(DeploymentAttemptStatus {
        QUEUED,
        RUNNING,
        SUCCEEDED,
        FAILED,
        CANCELED
    });

    screaming_enum!(DeploymentRuntimeHealthStatus {
        NOT_OBSERVED,
        STARTING,
        HEALTHY,
        UNHEALTHY,
        CANCELED
    });

    screaming_enum!(LogicalEnvironmentClass {
        DEVELOPMENT,
        STAGING,
        PRODUCTION
    });
    screaming_enum!(DeploymentRiskLevel { LOW, MEDIUM, HIGH });
    screaming_enum!(ApprovalEvidenceKind {
        PLAN_VALIDATED,
        CHANGE_SUMMARY_READY,
        EVALUATION_PASSED
    });
    screaming_enum!(ApprovalEvidenceState {
        VALID,
        MISSING,
        EXPIRED,
        REVOKED,
        FAILED,
        MISMATCH
    });
    screaming_enum!(ApprovalRequirementStatus {
        PENDING,
        SATISFIED,
        REJECTED,
        EXPIRED,
        INVALIDATED
    });
    screaming_enum!(ApprovalDecisionValue { APPROVE, REJECT });
    impl ApprovalDecisionValue {
        fn value(self) -> &'static str {
            match self {
                Self::APPROVE => "APPROVE",
                Self::REJECT => "REJECT",
            }
        }
    }

    impl From<AppProblem> for Problem {
        fn from(problem: AppProblem) -> Self {
            let code = problem_kind_name(&problem.kind);
            let message = match problem.kind {
                AppProblemKind::NotFound => "This deployment is unavailable.",
                AppProblemKind::Forbidden => {
                    "You do not have permission to perform this deployment action."
                }
                AppProblemKind::InvalidInput => {
                    "Choose immutable deployment facts and a valid idempotency key."
                }
                AppProblemKind::ReasonRequired => "Enter a rollback reason.",
                AppProblemKind::ConfirmationRequired => {
                    "Type the production environment identifier to confirm rollback."
                }
                AppProblemKind::ConfirmationMismatch => {
                    "The production environment confirmation does not match."
                }
                AppProblemKind::RevisionConflict => {
                    "This deployment changed before the action was recorded."
                }
                AppProblemKind::LifecycleConflict => "This deployment cannot make that transition.",
                AppProblemKind::IdempotencyConflict => {
                    "This idempotency key belongs to a different deployment action."
                }
                AppProblemKind::RateLimited => {
                    "The local deployment request limit is reached. Wait for an active request to finish."
                }
            };
            if problem.kind == AppProblemKind::RevisionConflict {
                return Problem::revision_conflict(
                    message,
                    problem.resource_id.map(|id| id.to_string()),
                    problem.expected_revision,
                    problem.actual_revision,
                );
            }
            Problem::new(code, message)
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentMutationPayload {
        pub deployment: Option<deployments::Model>,
        pub problems: Vec<Problem>,
    }

    /// The command's own answer, with the deployment it names re-read as the generated entity.
    pub(super) async fn mutation_payload(
        ctx: &async_graphql::Context<'_>,
        result: AppMutationResult,
    ) -> async_graphql::Result<DeploymentMutationPayload> {
        let deployment = match result.deployment.as_ref() {
            Some(deployment) => super::deployment_row(ctx, deployment.id).await?,
            None => None,
        };
        Ok(DeploymentMutationPayload {
            deployment,
            problems: result.problem.into_iter().map(Problem::from).collect(),
        })
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "DeployAgentVersionInput")]
    pub struct DeployAgentVersionInput {
        pub agentVersionId: Id,
        pub environmentDefinitionVersionId: Id,
        pub strategy: DeploymentStrategy,
        pub idempotencyKey: Option<String>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "CancelDeploymentInput")]
    pub struct CancelDeploymentInput {
        pub deploymentId: Id,
        pub expectedRevision: Long,
        pub reason: Option<String>,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "RetryDeploymentInput")]
    pub struct RetryDeploymentInput {
        pub deploymentId: Id,
        pub expectedRevision: Long,
        pub idempotencyKey: String,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "PromoteDeploymentInput")]
    pub struct PromoteDeploymentInput {
        pub deploymentId: Id,
        pub expectedRevision: Long,
        pub idempotencyKey: String,
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "RollbackDeploymentInput")]
    pub struct RollbackDeploymentInput {
        pub deploymentId: Id,
        pub targetAgentVersionId: Option<Id>,
        pub expectedRevision: Long,
        pub reason: String,
        pub productionConfirmation: Option<String>,
        pub idempotencyKey: String,
    }

    /// The shared `Problem` (`schema/problem.rs`) replaces the `DeploymentApprovalProblem`
    /// interface and its three concrete types. The codes and the messages are unchanged; a
    /// `REVISION_CONFLICT` still carries the resource and both revisions.
    pub(super) fn approval_problem(problem: AppDecisionProblem) -> Problem {
        match problem.code.as_str() {
            "NOT_FOUND" => Problem::new(&problem.code, "This approval requirement is unavailable."),
            "REVISION_CONFLICT" => Problem::revision_conflict(
                "This approval requirement changed before the decision was recorded.",
                problem.resource_id.map(|id| id.to_string()),
                problem.expected_revision,
                problem.actual_revision,
            ),
            "IDEMPOTENCY_CONFLICT" => Problem::new(
                &problem.code,
                "This idempotency key belongs to a different approval decision.",
            ),
            "REJECTION_REASON_REQUIRED" => Problem::new(
                &problem.code,
                "Enter a rejection reason before recording that decision.",
            ),
            _ => Problem::new(&problem.code, "This approval decision is unavailable."),
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DecideDeploymentApprovalPayload {
        pub decision: Option<deployment_approval_decisions::Model>,
        pub requirement: Option<deployment_approval_requirements::Model>,
        pub deployment: Option<deployments::Model>,
        pub problems: Vec<Problem>,
    }

    /// The decision's own answer, with the decision, the requirement and the deployment it names
    /// re-read as the generated entities, so one console fragment covers the read and the write.
    pub(super) async fn decide_payload(
        ctx: &async_graphql::Context<'_>,
        result: AppDecisionMutationResult,
    ) -> async_graphql::Result<DecideDeploymentApprovalPayload> {
        let deployment = match result.deployment.as_ref() {
            Some(deployment) => super::deployment_row(ctx, deployment.id).await?,
            None => None,
        };
        let requirement = match result.requirement.as_ref() {
            Some(requirement) => super::requirement_row(ctx, requirement.id).await?,
            None => None,
        };
        let decision = match result.decision.as_ref() {
            Some(decision) => super::decision_row(ctx, decision.id).await?,
            None => None,
        };
        Ok(DecideDeploymentApprovalPayload {
            decision,
            requirement,
            deployment,
            problems: result.problem.into_iter().map(approval_problem).collect(),
        })
    }

    #[derive(CustomInputType)]
    #[seaography(input_type_name = "DecideDeploymentApprovalInput")]
    pub struct DecideDeploymentApprovalInput {
        pub approvalRequirementId: Id,
        pub expectedRevision: Long,
        pub decision: ApprovalDecisionValue,
        pub idempotencyKey: Id,
        pub comment: Option<String>,
        pub rejectionReason: Option<String>,
    }

    pub struct DeploymentMutations;

    #[CustomFields]
    impl DeploymentMutations {
        // Ports `DeploymentGraphql.Resolver.deploy`.
        async fn deployAgentVersion(
            ctx: &async_graphql::Context<'_>,
            input: DeployAgentVersionInput,
        ) -> async_graphql::Result<DeploymentMutationPayload> {
            let result = deployment_service(ctx)?
                .deploy(
                    principal(ctx)?,
                    &input.agentVersionId.0,
                    &input.environmentDefinitionVersionId.0,
                    input.strategy.value(),
                    input.idempotencyKey.as_deref().unwrap_or(""),
                )
                .await
                .map_err(map_error)?;
            mutation_payload(ctx, result).await
        }

        // Ports `DeploymentGraphql.Resolver.cancel`.
        async fn cancelDeployment(
            ctx: &async_graphql::Context<'_>,
            input: CancelDeploymentInput,
        ) -> async_graphql::Result<DeploymentMutationPayload> {
            let result = deployment_service(ctx)?
                .cancel(
                    principal(ctx)?,
                    &input.deploymentId.0,
                    input.expectedRevision.0,
                    input.reason.as_deref().unwrap_or(""),
                )
                .await
                .map_err(map_error)?;
            mutation_payload(ctx, result).await
        }

        // Ports `DeploymentGraphql.Resolver.retry`.
        async fn retryDeployment(
            ctx: &async_graphql::Context<'_>,
            input: RetryDeploymentInput,
        ) -> async_graphql::Result<DeploymentMutationPayload> {
            let result = deployment_service(ctx)?
                .retry(
                    principal(ctx)?,
                    &input.deploymentId.0,
                    input.expectedRevision.0,
                    &input.idempotencyKey,
                )
                .await;
            log_recovery("RETRY", ctx, &input.deploymentId.0, &result);
            mutation_payload(ctx, result.map_err(map_error)?).await
        }

        // Ports `DeploymentGraphql.Resolver.promote`.
        async fn promoteDeployment(
            ctx: &async_graphql::Context<'_>,
            input: PromoteDeploymentInput,
        ) -> async_graphql::Result<DeploymentMutationPayload> {
            let result = deployment_service(ctx)?
                .promote(
                    principal(ctx)?,
                    &input.deploymentId.0,
                    input.expectedRevision.0,
                    &input.idempotencyKey,
                )
                .await;
            log_recovery("PROMOTE", ctx, &input.deploymentId.0, &result);
            mutation_payload(ctx, result.map_err(map_error)?).await
        }

        // Ports `DeploymentGraphql.Resolver.rollback`.
        async fn rollbackDeployment(
            ctx: &async_graphql::Context<'_>,
            input: RollbackDeploymentInput,
        ) -> async_graphql::Result<DeploymentMutationPayload> {
            let result = deployment_service(ctx)?
                .rollback(
                    principal(ctx)?,
                    &input.deploymentId.0,
                    input.targetAgentVersionId.as_ref().map(|id| id.0.as_str()),
                    input.expectedRevision.0,
                    &input.reason,
                    input.productionConfirmation.as_deref(),
                    &input.idempotencyKey,
                )
                .await;
            log_recovery("ROLLBACK", ctx, &input.deploymentId.0, &result);
            mutation_payload(ctx, result.map_err(map_error)?).await
        }

        // Ports `DeploymentGraphql.Resolver.decide`. `idempotencyKey` doubles as
        // `decide_approval`'s `request_id`; `correlation_id` comes from the ambient
        // per-HTTP-request id `graphql::graphql` inserts.
        async fn decideDeploymentApproval(
            ctx: &async_graphql::Context<'_>,
            input: DecideDeploymentApprovalInput,
        ) -> async_graphql::Result<DecideDeploymentApprovalPayload> {
            let correlation_id = ctx.data::<RequestCorrelationId>()?.0.to_string();
            let result = deployment_service(ctx)?
                .decide_approval(
                    principal(ctx)?,
                    &input.approvalRequirementId.0,
                    input.expectedRevision.0,
                    input.decision.value(),
                    input.comment.as_deref(),
                    input.rejectionReason.as_deref(),
                    input.idempotencyKey.0.as_str(),
                    &correlation_id,
                )
                .await
                .map_err(map_error)?;
            decide_payload(ctx, result).await
        }
    }
}

pub use wire::{
    ApprovalDecisionValue, ApprovalEvidenceKind, ApprovalEvidenceState, ApprovalRequirementStatus,
    CancelDeploymentInput, DecideDeploymentApprovalInput, DecideDeploymentApprovalPayload,
    DeployAgentVersionInput, DeploymentAttemptStatus, DeploymentLifecycleStatus,
    DeploymentMutationPayload, DeploymentMutations, DeploymentRiskLevel,
    DeploymentRuntimeHealthStatus, DeploymentStrategy, LogicalEnvironmentClass,
    PromoteDeploymentInput, RetryDeploymentInput, RollbackDeploymentInput,
};

pub fn register(builder: &mut seaography::Builder) {
    // `DeploymentAttemptStatus`, `DeploymentLifecycleStatus`, `DeploymentRuntimeHealthStatus`,
    // `ApprovalEvidenceState` and `ApprovalRequirementStatus` have no field of their own: the
    // columns behind them are `TEXT` with a `CHECK`, so the generated objects expose them as
    // `String`. They stay registered as the wire vocabulary the console's `cynic::Enum`s are
    // checked against, exactly as `EvaluationRunStatus` does.
    builder.register_custom_enum::<DeploymentStrategy>();
    builder.register_custom_enum::<DeploymentLifecycleStatus>();
    builder.register_custom_enum::<DeploymentAttemptStatus>();
    builder.register_custom_enum::<DeploymentRuntimeHealthStatus>();
    builder.register_custom_enum::<LogicalEnvironmentClass>();
    builder.register_custom_enum::<DeploymentRiskLevel>();
    builder.register_custom_enum::<ApprovalEvidenceKind>();
    builder.register_custom_enum::<ApprovalEvidenceState>();
    builder.register_custom_enum::<ApprovalRequirementStatus>();
    builder.register_custom_enum::<ApprovalDecisionValue>();

    builder.register_custom_mutation::<DeploymentMutations>();

    // The return types of the generated objects' computed fields
    // (`hive_persistence::deployment::computed`). `#[CustomFields]` only builds the field; the
    // object a field returns still needs its own registration.
    use hive_persistence::deployment::computed;
    builder.register_custom_output::<computed::DeploymentTimelineEvent>();
    builder.register_custom_output::<computed::DeploymentPlanReview>();
    builder.register_custom_output::<computed::ProjectApprovalPolicyRule>();
    builder.register_custom_output::<computed::ApprovalTargetSnapshot>();
    builder.register_custom_output::<computed::DeploymentEvidenceSnapshot>();
    builder.register_custom_output::<computed::DeploymentApprovalSnapshot>();
    // `AgentVersions.deploymentPreview` (`hive_persistence::deployment::computed`) and the target
    // it reports; the field itself rides on the `AgentVersions` block `mod.rs` attaches.
    builder.register_custom_output::<computed::DeploymentPreviewTarget>();
    builder.register_custom_output::<computed::DeploymentPreview>();

    builder.register_custom_output::<DeploymentMutationPayload>();

    builder.register_custom_input::<DeployAgentVersionInput>();
    builder.register_custom_input::<CancelDeploymentInput>();
    builder.register_custom_input::<RetryDeploymentInput>();
    builder.register_custom_input::<PromoteDeploymentInput>();
    builder.register_custom_input::<RollbackDeploymentInput>();

    builder.register_custom_output::<DecideDeploymentApprovalPayload>();
    builder.register_custom_input::<DecideDeploymentApprovalInput>();
}
