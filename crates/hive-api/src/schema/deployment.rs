//! What is left of the hand-built deployment tier: `deploymentPreview`, the 5 deployment
//! mutations, and (RTP-APPROVAL) the approval inbox/decision surface: `approvalInbox`,
//! `approvalRequirement` (Java's resolver method is named `approvalDetail`, but the GraphQL field
//! itself is `approvalRequirement`), the nested `ApprovalRequirement.decisions` field, and
//! `decideDeploymentApproval`.
//!
//! The three deployment *reads* (`deployments`, `deploymentProjection`, the deprecated
//! `deploymentTimeline`, and `deploymentEnvironmentDefinitionVersions`) are deleted: they are
//! generated entity queries over `deployments` and `environment_definition_versions` now
//! (`docs/idiomatic-seaography-plan.md`, A2), with the nested structures answered by relations and
//! by the computed fields in `hive_persistence::deployment::computed`. Every payload that carried
//! a deployment — the five mutations, the approval inbox item and the approval decision — returns
//! the generated `Deployments` object itself (A5), so one console fragment covers all of them.
//!
//! Seventh and eighth interfaces this port builds — the last two — `DeploymentProblem` (7
//! implementors) and `DeploymentApprovalProblem` (3 implementors), same pattern as every prior
//! interface (`agent.rs`'s doc comment has the full trail on variant naming and
//! `#[allow(clippy::enum_variant_names)]`).
//!
//! 10 enums, the most of any file — same `scalars::wire_enum!` pattern `evaluation.rs` established,
//! variants spelled in full SCREAMING_SNAKE_CASE (`GSR-WIRE-CASE`).
//!
//! `Required` (`GSR-REQUIRED`) makes its first and only appearance: `packageReference`,
//! `observedAt`, `expiresAt` (on `ApprovalRequirement`/`ApprovalInboxItem`), and `message` are all
//! schema-non-null fields backed by an optional application value.
//!
//! Every connection here shares `{ edges, pageInfo: DeploymentPageInfo! }` (a *nested* page-info
//! object, unlike `evaluation.rs`'s flat `hasNextPage`/`endCursor` fields) and the application
//! layer represents a page as parallel `nodes`/`cursors` vectors rather than a single `edges` list
//! of `(cursor, node)` pairs — so a `deployment_connection_type!`/`from_app_deployment_connection!`
//! macro pair (distinct from `evaluation.rs`'s `connection_type!`/`from_app_connection!`) mirrors
//! the static tier's own two macros of the same names.
//!
//! `ApprovalRequirement.decisions`'s in-memory-preview optimization (`Resolver::approvalDecisions`
//! pre-fetching the first decisions page inside `approvalInbox` to avoid an N+1 round trip) is not
//! ported: it is a pure performance optimization with no wire-visible effect (the fallback path —
//! a direct `approval_decisions` call — always produces the identical result), and porting it would
//! require hand-building the entire ~17-field `ApprovalRequirement` object by hand (to carry a
//! schema-invisible preview field the derive macro cannot express, the same class of problem
//! `administration.rs`'s `FixedApprovalPolicyMatrixInput` hit, but for an *output* object with far
//! more fields). `decisions` always takes the direct-query path; `approvalInbox` always passes
//! `include_decision_preview: false`. Recorded in the design doc as a known, deliberate scope
//! reduction, not an oversight.

use crate::schema::scalars;
use crate::schema::scalars::{wire_enum, Id, Long, Required, StringList};
use crate::schema::{RequestCorrelationId, RequestPrincipal};
use async_graphql::dynamic::{Field, FieldFuture, InputValue, TypeRef};
use hive_application::deployment::{
    ApprovalDecision as AppApprovalDecision,
    ApprovalDecisionMutationResult as AppDecisionMutationResult,
    ApprovalDecisionProblem as AppDecisionProblem, ApprovalInboxItem as AppInboxItem,
    ApprovalPrincipal as AppApprovalPrincipal, ApprovalRequirement as AppApprovalRequirement,
    ApprovalRule as AppApprovalRule, ApprovalSnapshot as AppApprovalSnapshot,
    ApprovalTarget as AppApprovalTarget, DeploymentEnvironment as AppDeploymentEnvironment,
    DeploymentEvidence as AppDeploymentEvidence, DeploymentMutationResult as AppMutationResult,
    DeploymentOutcome as AppOutcome, DeploymentPreview as AppDeploymentPreview,
    DeploymentProblem as AppProblem, DeploymentProblemKind as AppProblemKind, DeploymentService,
    PreviewCurrentTarget as AppPreviewCurrentTarget,
};
use hive_persistence::deployment::PgDeploymentRepository;
use hive_persistence::entity::deployments;
use sea_orm::EntityTrait;
use seaography::{
    BuilderContext, CustomFields, CustomInputType, CustomOutputObject, CustomOutputType,
};
use uuid::Uuid;

pub const DEPLOYMENT_PROBLEM_INTERFACE: &str = "DeploymentProblem";
pub const DEPLOYMENT_APPROVAL_PROBLEM_INTERFACE: &str = "DeploymentApprovalProblem";

fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    hive_domain::java_offset_date_time_string(value)
}

fn optional_timestamp(value: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    value.map(hive_domain::java_offset_date_time_string)
}

fn after_argument() -> InputValue {
    InputValue::new("after", TypeRef::named(TypeRef::STRING))
}

fn first_argument_20() -> InputValue {
    InputValue::new("first", TypeRef::named_nn(TypeRef::INT)).default_value(20i32)
}

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
        fn parse(value: &str) -> Self {
            match value {
                "REPLACE" => Self::REPLACE,
                "ROLLING" => Self::ROLLING,
                "BLUE_GREEN" => Self::BLUE_GREEN,
                "CANARY" => Self::CANARY,
                other => panic!("unrecognized deployment strategy `{other}`"),
            }
        }
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
    impl LogicalEnvironmentClass {
        fn parse(value: &str) -> Self {
            match value {
                "DEVELOPMENT" => Self::DEVELOPMENT,
                "STAGING" => Self::STAGING,
                "PRODUCTION" => Self::PRODUCTION,
                other => panic!("unrecognized logical environment class `{other}`"),
            }
        }
    }

    screaming_enum!(DeploymentRiskLevel { LOW, MEDIUM, HIGH });
    impl DeploymentRiskLevel {
        fn parse(value: &str) -> Self {
            match value {
                "LOW" => Self::LOW,
                "MEDIUM" => Self::MEDIUM,
                "HIGH" => Self::HIGH,
                other => panic!("unrecognized deployment risk level `{other}`"),
            }
        }
    }

    screaming_enum!(ApprovalEvidenceKind {
        PLAN_VALIDATED,
        CHANGE_SUMMARY_READY,
        EVALUATION_PASSED
    });
    impl ApprovalEvidenceKind {
        fn parse(value: &str) -> Self {
            match value {
                "PLAN_VALIDATED" => Self::PLAN_VALIDATED,
                "CHANGE_SUMMARY_READY" => Self::CHANGE_SUMMARY_READY,
                "EVALUATION_PASSED" => Self::EVALUATION_PASSED,
                other => panic!("unrecognized approval evidence kind `{other}`"),
            }
        }
    }

    screaming_enum!(ApprovalEvidenceState {
        VALID,
        MISSING,
        EXPIRED,
        REVOKED,
        FAILED,
        MISMATCH
    });
    impl ApprovalEvidenceState {
        fn parse(value: &str) -> Self {
            match value {
                "VALID" => Self::VALID,
                "MISSING" => Self::MISSING,
                "EXPIRED" => Self::EXPIRED,
                "REVOKED" => Self::REVOKED,
                "FAILED" => Self::FAILED,
                "MISMATCH" => Self::MISMATCH,
                other => panic!("unrecognized approval evidence state `{other}`"),
            }
        }
    }

    screaming_enum!(ApprovalRequirementStatus {
        PENDING,
        SATISFIED,
        REJECTED,
        EXPIRED,
        INVALIDATED
    });
    impl From<hive_domain::deployment::ApprovalRequirementStatus> for ApprovalRequirementStatus {
        fn from(value: hive_domain::deployment::ApprovalRequirementStatus) -> Self {
            use hive_domain::deployment::ApprovalRequirementStatus as Domain;
            match value {
                Domain::Pending => Self::PENDING,
                Domain::Satisfied => Self::SATISFIED,
                Domain::Rejected => Self::REJECTED,
                Domain::Expired => Self::EXPIRED,
                Domain::Invalidated => Self::INVALIDATED,
            }
        }
    }

    screaming_enum!(ApprovalDecisionValue { APPROVE, REJECT });
    impl ApprovalDecisionValue {
        fn value(self) -> &'static str {
            match self {
                Self::APPROVE => "APPROVE",
                Self::REJECT => "REJECT",
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentEnvironmentDefinitionVersion {
        pub id: Id,
        pub stableDefinitionId: String,
        pub version: String,
        pub displayName: String,
        pub logicalEnvironmentClass: LogicalEnvironmentClass,
        pub catalogReleaseId: String,
        pub catalogReleaseDigest: String,
        pub contentDigest: String,
    }

    impl From<&AppDeploymentEnvironment> for DeploymentEnvironmentDefinitionVersion {
        fn from(value: &AppDeploymentEnvironment) -> Self {
            Self {
                id: value.id.to_string().into(),
                stableDefinitionId: value.stable_definition_id.clone(),
                version: value.version.clone(),
                displayName: value.display_name.clone(),
                logicalEnvironmentClass: LogicalEnvironmentClass::parse(
                    &value.logical_environment_class,
                ),
                catalogReleaseId: value.catalog_release_id.clone(),
                catalogReleaseDigest: value.catalog_release_digest.clone(),
                contentDigest: value.content_digest.clone(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentEvidenceSnapshot {
        pub kind: ApprovalEvidenceKind,
        pub digest: Option<String>,
        pub bindingDigest: Option<String>,
        pub expiresAt: Option<String>,
        pub state: ApprovalEvidenceState,
    }

    impl From<&AppDeploymentEvidence> for DeploymentEvidenceSnapshot {
        fn from(value: &AppDeploymentEvidence) -> Self {
            Self {
                kind: ApprovalEvidenceKind::parse(&value.kind),
                digest: value.digest.clone(),
                bindingDigest: value.binding_digest.clone(),
                expiresAt: optional_timestamp(value.expires_at),
                state: ApprovalEvidenceState::parse(&value.state),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentCurrentTarget {
        pub aliasName: Option<String>,
        pub deploymentId: Id,
        pub agentVersionId: Id,
        pub agentVersionNumber: Long,
        pub targetDigest: String,
        pub requestedAt: String,
    }

    impl From<&AppPreviewCurrentTarget> for DeploymentCurrentTarget {
        fn from(value: &AppPreviewCurrentTarget) -> Self {
            Self {
                aliasName: Some(value.alias_name.clone()),
                deploymentId: value.deployment_id.to_string().into(),
                agentVersionId: value.agent_version_id.to_string().into(),
                agentVersionNumber: Long(value.agent_version_number),
                targetDigest: value.target_digest.clone(),
                requestedAt: timestamp(value.requested_at),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentPreview {
        pub environmentDefinitionVersion: DeploymentEnvironmentDefinitionVersion,
        pub strategy: DeploymentStrategy,
        pub risk: DeploymentRiskLevel,
        pub policyDigest: String,
        pub policyRevision: Long,
        pub requiredEvidence: Vec<ApprovalEvidenceKind>,
        pub requiredApprovers: i32,
        pub planDigest: String,
        pub packageDigest: String,
        pub catalogReleaseId: String,
        pub catalogReleaseDigest: String,
        pub agentContentDigest: String,
        pub targetDigest: String,
        pub bindingDigest: String,
        pub currentTarget: Option<DeploymentCurrentTarget>,
        pub requirementExpiresAt: Option<String>,
        pub warnings: StringList,
        pub compatibility: String,
    }

    impl From<AppDeploymentPreview> for DeploymentPreview {
        fn from(value: AppDeploymentPreview) -> Self {
            Self {
                environmentDefinitionVersion: DeploymentEnvironmentDefinitionVersion::from(
                    &AppDeploymentEnvironment {
                        id: value.environment.id,
                        stable_definition_id: value.environment.stable_definition_id.clone(),
                        version: value.environment.version.clone(),
                        display_name: value.environment.display_name.clone(),
                        logical_environment_class: value
                            .environment
                            .logical_environment_class
                            .clone(),
                        catalog_release_id: value.environment.catalog_release_id.clone(),
                        catalog_release_digest: value.environment.catalog_release_digest.clone(),
                        content_digest: value.environment.content_digest.clone(),
                    },
                ),
                strategy: DeploymentStrategy::parse(&value.strategy),
                risk: DeploymentRiskLevel::parse(&value.risk),
                policyDigest: value.policy_digest,
                policyRevision: Long(value.policy_revision),
                requiredEvidence: value
                    .required_evidence
                    .iter()
                    .map(|kind| ApprovalEvidenceKind::parse(kind))
                    .collect(),
                requiredApprovers: value.required_approvers,
                planDigest: value.plan_digest,
                packageDigest: value.package_digest,
                catalogReleaseId: value.catalog_release_id,
                catalogReleaseDigest: value.catalog_release_digest,
                agentContentDigest: value.agent_content_digest,
                targetDigest: value.target_digest,
                bindingDigest: value.binding_digest,
                currentTarget: value
                    .current_target
                    .as_ref()
                    .map(DeploymentCurrentTarget::from),
                requirementExpiresAt: Some(timestamp(value.requirement_expires_at)),
                warnings: value.warnings.into(),
                compatibility: value.compatibility,
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentPageInfo {
        pub hasNextPage: bool,
        pub endCursor: Option<String>,
    }

    /// Ports the repeated `XxxEdge { cursor, node }` / `XxxConnection { edges, pageInfo:
    /// DeploymentPageInfo }` shape every connection in this file shares (this file's own doc
    /// comment has the trail on why it's a distinct pair from `evaluation.rs`'s `connection_type!`).
    macro_rules! deployment_connection_type {
        ($connection_name:ident, $edge_name:ident, $node:ty) => {
            #[derive(CustomOutputType, Clone)]
            pub struct $edge_name {
                pub cursor: String,
                pub node: $node,
            }

            #[derive(CustomOutputType, Clone)]
            pub struct $connection_name {
                pub edges: Vec<$edge_name>,
                pub pageInfo: DeploymentPageInfo,
            }
        };
    }

    macro_rules! from_app_deployment_connection {
        ($app_type:ty, $connection_name:ident, $edge_name:ident, $node_from:path) => {
            impl From<$app_type> for $connection_name {
                fn from(value: $app_type) -> Self {
                    let edges = value
                        .nodes
                        .iter()
                        .zip(value.cursors.iter())
                        .map(|(node, cursor)| $edge_name {
                            cursor: cursor.clone(),
                            node: $node_from(node),
                        })
                        .collect();
                    Self {
                        edges,
                        pageInfo: DeploymentPageInfo {
                            hasNextPage: value.has_next_page,
                            endCursor: value.end_cursor,
                        },
                    }
                }
            }
        };
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentNotFoundProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentAuthorizationProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentValidationProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentLifecycleProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentIdempotencyProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentRateLimitedProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentRevisionConflict {
        pub code: String,
        pub message: String,
        pub resourceId: Id,
        pub expectedRevision: Long,
        pub actualRevision: Long,
    }

    // clippy::enum_variant_names is a false positive here — see `agent.rs`'s `AgentDraftProblem`
    // for the full rationale.
    #[derive(CustomOutputType, Clone)]
    #[allow(clippy::enum_variant_names)]
    pub enum DeploymentProblem {
        DeploymentNotFoundProblem(DeploymentNotFoundProblem),
        DeploymentAuthorizationProblem(DeploymentAuthorizationProblem),
        DeploymentValidationProblem(DeploymentValidationProblem),
        DeploymentLifecycleProblem(DeploymentLifecycleProblem),
        DeploymentIdempotencyProblem(DeploymentIdempotencyProblem),
        DeploymentRateLimitedProblem(DeploymentRateLimitedProblem),
        DeploymentRevisionConflict(DeploymentRevisionConflict),
    }

    impl From<AppProblem> for DeploymentProblem {
        fn from(problem: AppProblem) -> Self {
            match problem.kind {
                AppProblemKind::NotFound => DeploymentProblem::DeploymentNotFoundProblem(DeploymentNotFoundProblem {
                    code: "NOT_FOUND".to_string(),
                    message: "This deployment is unavailable.".to_string(),
                }),
                AppProblemKind::Forbidden => DeploymentProblem::DeploymentAuthorizationProblem(DeploymentAuthorizationProblem {
                    code: "FORBIDDEN".to_string(),
                    message: "You do not have permission to perform this deployment action.".to_string(),
                }),
                AppProblemKind::InvalidInput => DeploymentProblem::DeploymentValidationProblem(DeploymentValidationProblem {
                    code: "INVALID_INPUT".to_string(),
                    message: "Choose immutable deployment facts and a valid idempotency key.".to_string(),
                }),
                AppProblemKind::ReasonRequired => DeploymentProblem::DeploymentValidationProblem(DeploymentValidationProblem {
                    code: "REASON_REQUIRED".to_string(),
                    message: "Enter a rollback reason.".to_string(),
                }),
                AppProblemKind::ConfirmationRequired => DeploymentProblem::DeploymentValidationProblem(DeploymentValidationProblem {
                    code: "CONFIRMATION_REQUIRED".to_string(),
                    message: "Type the production environment identifier to confirm rollback.".to_string(),
                }),
                AppProblemKind::ConfirmationMismatch => DeploymentProblem::DeploymentValidationProblem(DeploymentValidationProblem {
                    code: "CONFIRMATION_MISMATCH".to_string(),
                    message: "The production environment confirmation does not match.".to_string(),
                }),
                AppProblemKind::RevisionConflict => DeploymentProblem::DeploymentRevisionConflict(DeploymentRevisionConflict {
                    code: "REVISION_CONFLICT".to_string(),
                    message: "This deployment changed before the action was recorded.".to_string(),
                    resourceId: problem.resource_id.map(|id| id.to_string()).unwrap_or_default().into(),
                    expectedRevision: Long(problem.expected_revision),
                    actualRevision: Long(problem.actual_revision),
                }),
                AppProblemKind::LifecycleConflict => DeploymentProblem::DeploymentLifecycleProblem(DeploymentLifecycleProblem {
                    code: "LIFECYCLE_CONFLICT".to_string(),
                    message: "This deployment cannot make that transition.".to_string(),
                }),
                AppProblemKind::IdempotencyConflict => DeploymentProblem::DeploymentIdempotencyProblem(DeploymentIdempotencyProblem {
                    code: "IDEMPOTENCY_CONFLICT".to_string(),
                    message: "This idempotency key belongs to a different deployment action.".to_string(),
                }),
                AppProblemKind::RateLimited => DeploymentProblem::DeploymentRateLimitedProblem(DeploymentRateLimitedProblem {
                    code: "RATE_LIMITED".to_string(),
                    message: "The local deployment request limit is reached. Wait for an active request to finish.".to_string(),
                }),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentMutationPayload {
        pub deployment: Option<deployments::Model>,
        pub problems: Vec<DeploymentProblem>,
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
            problems: result
                .problem
                .into_iter()
                .map(DeploymentProblem::from)
                .collect(),
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

    #[derive(CustomOutputType, Clone)]
    pub struct ProjectApprovalPolicyRule {
        pub requiredEvidence: Vec<ApprovalEvidenceKind>,
        pub requiredDistinctApproverCount: i32,
    }

    impl From<&AppApprovalRule> for ProjectApprovalPolicyRule {
        fn from(value: &AppApprovalRule) -> Self {
            Self {
                requiredEvidence: value
                    .required_evidence
                    .iter()
                    .map(|kind| ApprovalEvidenceKind::parse(kind))
                    .collect(),
                requiredDistinctApproverCount: value.required_distinct_approver_count,
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ApprovalTargetSnapshot {
        pub agentVersionId: Id,
        pub agentVersionDigest: String,
        pub environmentDefinitionVersionId: Id,
        pub environmentDefinitionDigest: String,
        pub targetDigest: String,
        pub deploymentPlanDigest: String,
        pub artifactDigest: String,
    }

    impl From<&AppApprovalTarget> for ApprovalTargetSnapshot {
        fn from(value: &AppApprovalTarget) -> Self {
            Self {
                agentVersionId: value.agent_version_id.to_string().into(),
                agentVersionDigest: value.agent_version_digest.clone(),
                environmentDefinitionVersionId: value
                    .environment_definition_version_id
                    .to_string()
                    .into(),
                environmentDefinitionDigest: value.environment_definition_digest.clone(),
                targetDigest: value.target_digest.clone(),
                deploymentPlanDigest: value.deployment_plan_digest.clone(),
                artifactDigest: value.artifact_digest.clone(),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DeploymentApprovalSnapshot {
        pub policyDigest: String,
        pub policyRevision: Long,
        pub environmentClass: LogicalEnvironmentClass,
        pub risk: DeploymentRiskLevel,
        pub riskLevel: DeploymentRiskLevel,
        pub rule: ProjectApprovalPolicyRule,
        pub target: ApprovalTargetSnapshot,
        pub evidence: Vec<DeploymentEvidenceSnapshot>,
        pub expiresAt: String,
    }

    impl From<&AppApprovalSnapshot> for DeploymentApprovalSnapshot {
        fn from(value: &AppApprovalSnapshot) -> Self {
            let risk = DeploymentRiskLevel::parse(&value.risk);
            Self {
                policyDigest: value.policy_digest.clone(),
                policyRevision: Long(value.policy_revision),
                environmentClass: LogicalEnvironmentClass::parse(&value.environment_class),
                risk,
                riskLevel: risk,
                rule: ProjectApprovalPolicyRule::from(&value.rule),
                target: ApprovalTargetSnapshot::from(&value.target),
                evidence: value
                    .evidence
                    .iter()
                    .map(DeploymentEvidenceSnapshot::from)
                    .collect(),
                expiresAt: timestamp(value.expires_at),
            }
        }
    }

    fn approval_principal(
        id: Uuid,
        principal: Option<&AppApprovalPrincipal>,
    ) -> crate::schema::principal::Principal {
        match principal {
            Some(principal) => crate::schema::principal::Principal {
                id: principal.id.to_string().into(),
                subject: principal.subject.clone(),
            },
            None => crate::schema::principal::Principal {
                id: id.to_string().into(),
                subject: id.to_string(),
            },
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ApprovalDecision {
        pub id: Id,
        pub requirementId: Id,
        pub actorPrincipalId: Id,
        pub decision: ApprovalDecisionValue,
        pub comment: Option<String>,
        pub rejectionReason: Option<String>,
        pub eligibilityCheckedAt: String,
        pub decidedAt: String,
    }

    impl From<&AppApprovalDecision> for ApprovalDecision {
        fn from(value: &AppApprovalDecision) -> Self {
            Self {
                id: value.id.to_string().into(),
                requirementId: value.requirement_id.to_string().into(),
                actorPrincipalId: value.actor_principal_id.to_string().into(),
                decision: match value.value.as_str() {
                    "APPROVE" => ApprovalDecisionValue::APPROVE,
                    "REJECT" => ApprovalDecisionValue::REJECT,
                    other => panic!("unrecognized approval decision value `{other}`"),
                },
                comment: value.comment.clone(),
                rejectionReason: value.rejection_reason.clone(),
                eligibilityCheckedAt: timestamp(value.eligibility_checked_at),
                decidedAt: timestamp(value.decided_at),
            }
        }
    }

    deployment_connection_type!(
        ApprovalDecisionConnection,
        ApprovalDecisionEdge,
        ApprovalDecision
    );
    from_app_deployment_connection!(
        hive_application::deployment::ApprovalDecisionConnection,
        ApprovalDecisionConnection,
        ApprovalDecisionEdge,
        ApprovalDecision::from
    );

    #[derive(CustomOutputType, Clone)]
    pub struct ApprovalRequirement {
        pub id: Id,
        pub deploymentId: Id,
        pub projectId: Id,
        pub revision: Long,
        pub revisionNumber: Long,
        pub status: ApprovalRequirementStatus,
        pub expiresAt: Required,
        pub satisfiedAt: Option<String>,
        pub rejectedAt: Option<String>,
        pub invalidatedAt: Option<String>,
        pub requesterId: Id,
        pub requester: crate::schema::principal::Principal,
        pub requiredDistinctApproverCount: i32,
        pub qualifyingApprovalCount: i32,
        pub satisfiedParticipantIds: Vec<Id>,
        pub satisfiedParticipants: Vec<crate::schema::principal::Principal>,
        pub approvalSnapshot: DeploymentApprovalSnapshot,
    }

    impl From<&AppApprovalRequirement> for ApprovalRequirement {
        fn from(value: &AppApprovalRequirement) -> Self {
            let revision = Long(value.revision);
            Self {
                id: value.id.to_string().into(),
                deploymentId: value.deployment_id.to_string().into(),
                projectId: value.project_id.to_string().into(),
                revision,
                revisionNumber: revision,
                status: value.status.into(),
                expiresAt: Required(optional_timestamp(value.expires_at)),
                satisfiedAt: optional_timestamp(value.satisfied_at),
                rejectedAt: optional_timestamp(value.rejected_at),
                invalidatedAt: optional_timestamp(value.invalidated_at),
                requesterId: value.requester_id.to_string().into(),
                requester: approval_principal(value.requester_id, value.requester.as_ref()),
                requiredDistinctApproverCount: value.required_distinct_approver_count,
                qualifyingApprovalCount: value.qualifying_approval_count,
                satisfiedParticipantIds: value
                    .satisfied_participants
                    .iter()
                    .map(|id| id.to_string().into())
                    .collect(),
                satisfiedParticipants: value
                    .satisfied_participant_details
                    .iter()
                    .map(|principal| approval_principal(principal.id, Some(principal)))
                    .collect(),
                approvalSnapshot: DeploymentApprovalSnapshot::from(&value.approval_snapshot),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ApprovalInboxItem {
        pub requirement: ApprovalRequirement,
        pub deployment: deployments::Model,
        pub decisionAvailable: bool,
        pub eligible: bool,
        pub status: ApprovalRequirementStatus,
        pub riskLevel: DeploymentRiskLevel,
        pub expiresAt: Required,
    }

    /// One inbox item, with the deployment it names re-read as the generated entity.
    pub(super) async fn approval_inbox_item(
        ctx: &async_graphql::Context<'_>,
        value: &AppInboxItem,
    ) -> async_graphql::Result<ApprovalInboxItem> {
        Ok(ApprovalInboxItem {
            requirement: ApprovalRequirement::from(&value.requirement),
            deployment: super::deployment_row(ctx, value.deployment.id)
                .await?
                .ok_or_else(|| async_graphql::Error::new("This deployment is unavailable."))?,
            decisionAvailable: value.decision_available,
            eligible: value.eligible,
            status: value.requirement.status.into(),
            riskLevel: DeploymentRiskLevel::parse(&value.requirement.approval_snapshot.risk),
            expiresAt: Required(optional_timestamp(value.requirement.expires_at)),
        })
    }

    deployment_connection_type!(
        ApprovalInboxConnection,
        ApprovalInboxEdge,
        ApprovalInboxItem
    );

    /// The inbox page, item by item; the plain `From` shape every other connection uses cannot
    /// reach the database the entity re-read needs.
    pub(super) async fn approval_inbox_connection(
        ctx: &async_graphql::Context<'_>,
        value: hive_application::deployment::ApprovalInboxConnection,
    ) -> async_graphql::Result<ApprovalInboxConnection> {
        let mut edges = Vec::with_capacity(value.nodes.len());
        for (node, cursor) in value.nodes.iter().zip(value.cursors.iter()) {
            edges.push(ApprovalInboxEdge {
                cursor: cursor.clone(),
                node: approval_inbox_item(ctx, node).await?,
            });
        }
        Ok(ApprovalInboxConnection {
            edges,
            pageInfo: DeploymentPageInfo {
                hasNextPage: value.has_next_page,
                endCursor: value.end_cursor,
            },
        })
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ApprovalPolicyProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ApprovalIdempotencyProblem {
        pub code: String,
        pub message: String,
    }

    #[derive(CustomOutputType, Clone)]
    pub struct ApprovalRequirementRevisionConflict {
        pub code: String,
        pub message: String,
        pub resourceId: Id,
        pub expectedRevision: Long,
        pub actualRevision: Long,
    }

    #[derive(CustomOutputType, Clone)]
    #[allow(clippy::enum_variant_names)]
    pub enum DeploymentApprovalProblem {
        ApprovalPolicyProblem(ApprovalPolicyProblem),
        ApprovalIdempotencyProblem(ApprovalIdempotencyProblem),
        ApprovalRequirementRevisionConflict(ApprovalRequirementRevisionConflict),
    }

    impl From<AppDecisionProblem> for DeploymentApprovalProblem {
        fn from(problem: AppDecisionProblem) -> Self {
            match problem.code.as_str() {
                "NOT_FOUND" => DeploymentApprovalProblem::ApprovalPolicyProblem(ApprovalPolicyProblem {
                    code: problem.code,
                    message: "This approval requirement is unavailable.".to_string(),
                }),
                "REVISION_CONFLICT" => DeploymentApprovalProblem::ApprovalRequirementRevisionConflict(ApprovalRequirementRevisionConflict {
                    code: problem.code,
                    message: "This approval requirement changed before the decision was recorded.".to_string(),
                    resourceId: problem.resource_id.map(|id| id.to_string()).unwrap_or_default().into(),
                    expectedRevision: Long(problem.expected_revision),
                    actualRevision: Long(problem.actual_revision),
                }),
                "IDEMPOTENCY_CONFLICT" => DeploymentApprovalProblem::ApprovalIdempotencyProblem(ApprovalIdempotencyProblem {
                    code: problem.code,
                    message: "This idempotency key belongs to a different approval decision.".to_string(),
                }),
                "REJECTION_REASON_REQUIRED" => DeploymentApprovalProblem::ApprovalPolicyProblem(ApprovalPolicyProblem {
                    code: problem.code,
                    message: "Enter a rejection reason before recording that decision.".to_string(),
                }),
                _ => DeploymentApprovalProblem::ApprovalPolicyProblem(ApprovalPolicyProblem {
                    code: problem.code,
                    message: "This approval decision is unavailable.".to_string(),
                }),
            }
        }
    }

    #[derive(CustomOutputType, Clone)]
    pub struct DecideDeploymentApprovalPayload {
        pub decision: Option<ApprovalDecision>,
        pub requirement: Option<ApprovalRequirement>,
        pub deployment: Option<deployments::Model>,
        pub problems: Vec<DeploymentApprovalProblem>,
    }

    /// The decision's own answer, with the deployment it names re-read as the generated entity.
    pub(super) async fn decide_payload(
        ctx: &async_graphql::Context<'_>,
        result: AppDecisionMutationResult,
    ) -> async_graphql::Result<DecideDeploymentApprovalPayload> {
        let deployment = match result.deployment.as_ref() {
            Some(deployment) => super::deployment_row(ctx, deployment.id).await?,
            None => None,
        };
        Ok(DecideDeploymentApprovalPayload {
            decision: result.decision.as_ref().map(ApprovalDecision::from),
            requirement: result.requirement.as_ref().map(ApprovalRequirement::from),
            deployment,
            problems: result
                .problem
                .into_iter()
                .map(DeploymentApprovalProblem::from)
                .collect(),
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

    pub struct DeploymentQueries;

    #[CustomFields]
    impl DeploymentQueries {
        // Ports `DeploymentGraphql.Resolver.preview`.
        async fn deploymentPreview(
            ctx: &async_graphql::Context<'_>,
            agentVersionId: Id,
            environmentDefinitionVersionId: Id,
            strategy: DeploymentStrategy,
        ) -> async_graphql::Result<Option<DeploymentPreview>> {
            let preview = deployment_service(ctx)?
                .preview(
                    principal(ctx)?,
                    &agentVersionId.0,
                    &environmentDefinitionVersionId.0,
                    strategy.value(),
                )
                .await
                .map_err(map_error)?;
            Ok(preview.map(DeploymentPreview::from))
        }

        // Ports `DeploymentGraphql.Resolver.approvalDetail`. Java names the GraphQL field
        // `approvalRequirement`, but it resolves through `approvalDetail` and returns the same
        // `ApprovalInboxItem` wrapper `approvalInbox` does.
        async fn approvalRequirement(
            ctx: &async_graphql::Context<'_>,
            approvalRequirementId: Id,
        ) -> async_graphql::Result<Option<ApprovalInboxItem>> {
            let item = deployment_service(ctx)?
                .approval_detail(principal(ctx)?, &approvalRequirementId.0)
                .await
                .map_err(map_error)?;
            match item.as_ref() {
                Some(item) => Ok(Some(approval_inbox_item(ctx, item).await?)),
                None => Ok(None),
            }
        }
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
    ApprovalDecision, ApprovalDecisionConnection, ApprovalDecisionEdge, ApprovalDecisionValue,
    ApprovalEvidenceKind, ApprovalEvidenceState, ApprovalIdempotencyProblem,
    ApprovalInboxConnection, ApprovalInboxEdge, ApprovalInboxItem, ApprovalPolicyProblem,
    ApprovalRequirement, ApprovalRequirementRevisionConflict, ApprovalRequirementStatus,
    ApprovalTargetSnapshot, CancelDeploymentInput, DecideDeploymentApprovalInput,
    DecideDeploymentApprovalPayload, DeployAgentVersionInput, DeploymentApprovalSnapshot,
    DeploymentAttemptStatus, DeploymentAuthorizationProblem, DeploymentCurrentTarget,
    DeploymentEnvironmentDefinitionVersion, DeploymentEvidenceSnapshot,
    DeploymentIdempotencyProblem, DeploymentLifecycleProblem, DeploymentLifecycleStatus,
    DeploymentMutationPayload, DeploymentMutations, DeploymentNotFoundProblem, DeploymentPageInfo,
    DeploymentPreview, DeploymentQueries, DeploymentRateLimitedProblem, DeploymentRevisionConflict,
    DeploymentRiskLevel, DeploymentRuntimeHealthStatus, DeploymentStrategy,
    DeploymentValidationProblem, LogicalEnvironmentClass, ProjectApprovalPolicyRule,
    PromoteDeploymentInput, RetryDeploymentInput, RollbackDeploymentInput,
};

fn context() -> &'static BuilderContext {
    crate::schema::context()
}

/// This module's two `Interface`s, registered directly on the `SchemaBuilder` in `mod.rs::build()`
/// (`Builder` itself has no interface vector to push onto — same as every other interface module).
pub fn interfaces() -> Vec<async_graphql::dynamic::Interface> {
    use async_graphql::dynamic::{Interface, InterfaceField};
    vec![
        Interface::new(DEPLOYMENT_PROBLEM_INTERFACE)
            .field(InterfaceField::new(
                "code",
                TypeRef::named_nn(TypeRef::STRING),
            ))
            .field(InterfaceField::new(
                "message",
                TypeRef::named_nn(TypeRef::STRING),
            )),
        Interface::new(DEPLOYMENT_APPROVAL_PROBLEM_INTERFACE)
            .field(InterfaceField::new(
                "code",
                TypeRef::named_nn(TypeRef::STRING),
            ))
            .field(InterfaceField::new(
                "message",
                TypeRef::named_nn(TypeRef::STRING),
            )),
    ]
}

/// `ApprovalRequirement.decisions(after, first: Int! = 20)` (`GSR-DEFAULTS`). Always takes the
/// direct-query path — see this file's own doc comment on the dropped in-memory-preview
/// optimization.
fn decisions_field() -> Field {
    Field::new(
        "decisions",
        TypeRef::named_nn("ApprovalDecisionConnection"),
        |ctx| {
            FieldFuture::new(async move {
                let requirement = ctx.parent_value.try_downcast_ref::<ApprovalRequirement>()?;
                let after = scalars::optional_string(ctx.args.get("after"))?;
                let first = ctx.args.try_get("first")?.i64()? as i32;
                let principal_id = principal(ctx.ctx)?;
                let empty = || ApprovalDecisionConnection {
                    edges: Vec::new(),
                    pageInfo: DeploymentPageInfo {
                        hasNextPage: false,
                        endCursor: None,
                    },
                };
                let connection = deployment_service(ctx.ctx)?
                    .approval_decisions(principal_id, &requirement.id.0, after.as_deref(), first)
                    .await
                    .map_err(map_error)?;
                let connection = connection.map_or_else(empty, ApprovalDecisionConnection::from);
                Ok(connection.gql_field_value(context()))
            })
        },
    )
    .argument(after_argument())
    .argument(first_argument_20())
}

pub fn register(builder: &mut seaography::Builder) {
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

    builder.register_custom_query::<DeploymentQueries>();
    builder.register_custom_mutation::<DeploymentMutations>();

    // The two return types of the generated objects' computed fields
    // (`hive_persistence::deployment::computed`). `#[CustomFields]` only builds the field; the
    // object a field returns still needs its own registration.
    builder
        .register_custom_output::<hive_persistence::deployment::computed::DeploymentTimelineEvent>(
        );
    builder
        .register_custom_output::<hive_persistence::deployment::computed::DeploymentPlanReview>();

    builder.register_custom_output::<DeploymentEnvironmentDefinitionVersion>();
    builder.register_custom_output::<DeploymentEvidenceSnapshot>();
    builder.register_custom_output::<DeploymentCurrentTarget>();
    builder.register_custom_output::<DeploymentPreview>();
    builder.register_custom_output::<DeploymentPageInfo>();

    builder.outputs.push(
        DeploymentNotFoundProblem::basic_object(context()).implement(DEPLOYMENT_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        DeploymentAuthorizationProblem::basic_object(context())
            .implement(DEPLOYMENT_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        DeploymentValidationProblem::basic_object(context())
            .implement(DEPLOYMENT_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        DeploymentLifecycleProblem::basic_object(context()).implement(DEPLOYMENT_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        DeploymentIdempotencyProblem::basic_object(context())
            .implement(DEPLOYMENT_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        DeploymentRateLimitedProblem::basic_object(context())
            .implement(DEPLOYMENT_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        DeploymentRevisionConflict::basic_object(context()).implement(DEPLOYMENT_PROBLEM_INTERFACE),
    );
    builder.register_custom_output::<DeploymentMutationPayload>();

    builder.register_custom_input::<DeployAgentVersionInput>();
    builder.register_custom_input::<CancelDeploymentInput>();
    builder.register_custom_input::<RetryDeploymentInput>();
    builder.register_custom_input::<PromoteDeploymentInput>();
    builder.register_custom_input::<RollbackDeploymentInput>();

    builder.register_custom_output::<ProjectApprovalPolicyRule>();
    builder.register_custom_output::<ApprovalTargetSnapshot>();
    builder.register_custom_output::<DeploymentApprovalSnapshot>();
    builder.register_custom_output::<ApprovalDecision>();
    builder.register_custom_output::<ApprovalDecisionEdge>();
    builder.register_custom_output::<ApprovalDecisionConnection>();
    builder
        .outputs
        .push(ApprovalRequirement::basic_object(context()).field(decisions_field()));
    builder.register_custom_output::<ApprovalInboxItem>();
    builder.register_custom_output::<ApprovalInboxEdge>();
    builder.register_custom_output::<ApprovalInboxConnection>();

    builder.outputs.push(
        ApprovalPolicyProblem::basic_object(context())
            .implement(DEPLOYMENT_APPROVAL_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        ApprovalIdempotencyProblem::basic_object(context())
            .implement(DEPLOYMENT_APPROVAL_PROBLEM_INTERFACE),
    );
    builder.outputs.push(
        ApprovalRequirementRevisionConflict::basic_object(context())
            .implement(DEPLOYMENT_APPROVAL_PROBLEM_INTERFACE),
    );
    builder.register_custom_output::<DecideDeploymentApprovalPayload>();
    builder.register_custom_input::<DecideDeploymentApprovalInput>();

    builder.queries.push(
        Field::new(
            "approvalInbox",
            TypeRef::named("ApprovalInboxConnection"),
            |ctx| {
                FieldFuture::new(async move {
                    let organization_id = scalars::optional_string(ctx.args.get("organizationId"))?;
                    let project_id = scalars::optional_string(ctx.args.get("projectId"))?;
                    let after = scalars::optional_string(ctx.args.get("after"))?;
                    let first = ctx.args.try_get("first")?.i64()? as i32;
                    let connection = deployment_service(ctx.ctx)?
                        .approval_inbox(
                            principal(ctx.ctx)?,
                            organization_id.as_deref(),
                            project_id.as_deref(),
                            after.as_deref(),
                            first,
                            false,
                        )
                        .await
                        .map_err(map_error)?;
                    let connection = match connection {
                        Some(connection) => {
                            Some(wire::approval_inbox_connection(ctx.ctx, connection).await?)
                        }
                        None => None,
                    };
                    Ok(connection.and_then(|connection| connection.gql_field_value(context())))
                })
            },
        )
        .argument(InputValue::new(
            "organizationId",
            TypeRef::named(TypeRef::ID),
        ))
        .argument(InputValue::new("projectId", TypeRef::named(TypeRef::ID)))
        .argument(after_argument())
        .argument(first_argument_20()),
    );
}
