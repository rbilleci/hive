//! Canonicalizes untrusted identifiers before the repository applies tenant visibility and current
//! authority. Approval-decision validation lives in `DeploymentService::decide_approval` rather
//! than a separate struct, since it needs only the repository `DeploymentService` already owns.

use super::compiler::{CompiledRequest, DeploymentCompiler};
use super::models::{
    ApprovalDecisionMutationResult, ApprovalDecisionProblem, DeploymentMutationResult,
    DeploymentPreview, DeploymentProblem, DeploymentRecoveryCompilationContext,
    PreviewCurrentTarget, PreviewEnvironment,
};
use super::policy::ApprovalDecisionPlanner;
use super::repository::DeploymentRepository;
use crate::RepositoryError;
use chrono::Utc;
use hive_domain::deployment::ApprovalDecisionCommand;
use std::collections::HashSet;
use std::sync::LazyLock;
use uuid::Uuid;

/// M14 persists only cataloged review codes so an immutable fact cannot carry arbitrary secret or personal-data text.
static REVIEW_CODES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "REVIEWED_CHANGE_SCOPE",
        "AUTHORIZATION_GRANTED",
        "UNACCEPTABLE_CHANGE_SCOPE",
        "CHANGE_SCOPE_NOT_APPROVED",
    ])
});

fn blank(value: Option<&str>) -> bool {
    value.map(str::trim).unwrap_or("").is_empty()
}

fn bounded_review_text(value: Option<&str>) -> bool {
    blank(value) || REVIEW_CODES.contains(value.unwrap().trim())
}

fn review_text_combination(
    decision: &str,
    comment: Option<&str>,
    rejection_reason: Option<&str>,
) -> bool {
    !(decision == "APPROVE" && !blank(rejection_reason))
        && !(decision == "REJECT" && !blank(comment))
}

pub struct DeploymentService<R: DeploymentRepository> {
    repository: R,
    compiler: DeploymentCompiler,
}

impl<R: DeploymentRepository> DeploymentService<R> {
    pub fn new(repository: R) -> Self {
        Self {
            repository,
            compiler: DeploymentCompiler::new(),
        }
    }

    pub async fn preview(
        &self,
        principal: Uuid,
        version: &str,
        environment_definition_version: &str,
        strategy: &str,
    ) -> Result<Option<DeploymentPreview>, RepositoryError> {
        let (Ok(version_id), Ok(environment_id)) = (
            Uuid::parse_str(version),
            Uuid::parse_str(environment_definition_version),
        ) else {
            return Ok(None);
        };
        let Some(request) = self
            .compile(principal, version_id, environment_id, strategy)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(preview(request)))
    }

    pub async fn deploy(
        &self,
        principal: Uuid,
        version: &str,
        environment_definition_version: &str,
        strategy: &str,
        key: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError> {
        let (Ok(version_id), Ok(environment_id)) = (
            Uuid::parse_str(version),
            Uuid::parse_str(environment_definition_version),
        ) else {
            return Ok(DeploymentMutationResult::refused(
                DeploymentProblem::not_found(),
            ));
        };
        match self
            .compile(principal, version_id, environment_id, strategy)
            .await?
        {
            Some(request) => self.repository.deploy(principal, &request, key).await,
            None => Ok(DeploymentMutationResult::refused(
                DeploymentProblem::invalid(),
            )),
        }
    }

    pub async fn cancel(
        &self,
        principal: Uuid,
        deployment: &str,
        revision: i64,
        reason: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError> {
        let Ok(deployment_id) = Uuid::parse_str(deployment) else {
            return Ok(DeploymentMutationResult::refused(
                DeploymentProblem::not_found(),
            ));
        };
        self.repository
            .cancel(principal, deployment_id, revision, reason)
            .await
    }

    pub async fn retry(
        &self,
        principal: Uuid,
        deployment: &str,
        revision: i64,
        key: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError> {
        let Ok(deployment_id) = Uuid::parse_str(deployment) else {
            return Ok(DeploymentMutationResult::refused(
                DeploymentProblem::not_found(),
            ));
        };
        let context = self
            .repository
            .retry_compilation_context(principal, deployment_id)
            .await?;
        let request = match context {
            Some(context) => self.compile_recovery(&context).await,
            None => None,
        };
        self.repository
            .retry(principal, deployment_id, revision, key, request.as_ref())
            .await
    }

    pub async fn promote(
        &self,
        principal: Uuid,
        deployment: &str,
        revision: i64,
        key: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError> {
        let Ok(deployment_id) = Uuid::parse_str(deployment) else {
            return Ok(DeploymentMutationResult::refused(
                DeploymentProblem::not_found(),
            ));
        };
        self.repository
            .promote(principal, deployment_id, revision, key)
            .await
    }

    /// `target_version`/`confirmation` are genuinely nullable, not empty-string sentinels — see
    /// `DeploymentRepository::rollback`'s doc comment for why that distinction matters.
    #[allow(clippy::too_many_arguments)]
    pub async fn rollback(
        &self,
        principal: Uuid,
        deployment: &str,
        target_version: Option<&str>,
        revision: i64,
        reason: &str,
        confirmation: Option<&str>,
        key: &str,
    ) -> Result<DeploymentMutationResult, RepositoryError> {
        let Ok(deployment_id) = Uuid::parse_str(deployment) else {
            return Ok(DeploymentMutationResult::refused(
                DeploymentProblem::not_found(),
            ));
        };
        let context = self
            .repository
            .rollback_compilation_context(principal, deployment_id, target_version)
            .await?;
        let request = match context {
            Some(context) => self.compile_recovery(&context).await,
            None => None,
        };
        self.repository
            .rollback(
                principal,
                deployment_id,
                target_version,
                revision,
                reason,
                confirmation,
                key,
                request.as_ref(),
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn decide_approval(
        &self,
        principal: Uuid,
        requirement: &str,
        revision: i64,
        decision: &str,
        comment: Option<&str>,
        rejection_reason: Option<&str>,
        request_id: &str,
        correlation_id: &str,
    ) -> Result<ApprovalDecisionMutationResult, RepositoryError> {
        if !review_text_combination(decision, comment, rejection_reason)
            || !bounded_review_text(comment)
            || !bounded_review_text(rejection_reason)
        {
            return Ok(ApprovalDecisionMutationResult::refused(
                ApprovalDecisionProblem::of("INVALID_APPROVAL_DECISION"),
            ));
        }
        let (Ok(requirement_id), Ok(decision_request_id), Ok(decision_correlation_id)) = (
            Uuid::parse_str(requirement),
            Uuid::parse_str(request_id),
            Uuid::parse_str(correlation_id),
        ) else {
            return Ok(ApprovalDecisionMutationResult::refused(
                ApprovalDecisionProblem::unavailable(),
            ));
        };
        let command = ApprovalDecisionCommand {
            principal_id: principal,
            requirement_id,
            expected_revision: revision,
            value: decision.to_string(),
            comment: (!blank(comment)).then(|| comment.unwrap().trim().to_string()),
            rejection_reason: (!blank(rejection_reason))
                .then(|| rejection_reason.unwrap().trim().to_string()),
            request_id: decision_request_id,
            correlation_id: decision_correlation_id,
        };
        self.repository
            .record_approval_decision(command, ApprovalDecisionPlanner)
            .await
    }

    async fn compile(
        &self,
        principal: Uuid,
        version_id: Uuid,
        environment_id: Uuid,
        strategy: &str,
    ) -> Result<Option<CompiledRequest>, RepositoryError> {
        let Some(context) = self
            .repository
            .compilation_context(principal, version_id, environment_id)
            .await?
        else {
            return Ok(None);
        };
        Ok(self.compiler.compile(
            &context.version,
            &context.environment,
            &context.policy,
            context.current_target.as_ref(),
            strategy,
            Utc::now(),
        ))
    }

    async fn compile_recovery(
        &self,
        context: &DeploymentRecoveryCompilationContext,
    ) -> Option<CompiledRequest> {
        self.compiler.compile(
            &context.version,
            &context.environment,
            &context.policy,
            context.current_target.as_ref(),
            &context.strategy,
            Utc::now(),
        )
    }
}

/// Takes `value` by value rather than `&CompiledRequest`: its one caller (`preview()` above) never
/// reuses `request` afterward, so every field here moves into the returned `DeploymentPreview`
/// instead of cloning. The two fields read twice (`environment.stable_definition_id` for both
/// `PreviewCurrentTarget.alias_name` and `PreviewEnvironment.stable_definition_id`; `rule.evidence`
/// for both the `warnings` check and `required_evidence`) still need no clone: their first read is a
/// borrow (`format!`'s `Display` argument, `.iter()`), which can run before the second read moves
/// the field.
fn preview(value: CompiledRequest) -> DeploymentPreview {
    let CompiledRequest {
        version,
        environment,
        policy,
        rule,
        strategy,
        risk,
        target_digest,
        plan_digest,
        package_digest,
        binding_digest,
        current_target,
        requirement_expires_at,
        ..
    } = value;
    let current_target = current_target.map(|current| PreviewCurrentTarget {
        alias_name: format!("local:{}", environment.stable_definition_id),
        deployment_id: current.deployment_id,
        agent_version_id: current.agent_version_id,
        agent_version_number: current.agent_version_number,
        target_digest: current.target_digest,
        requested_at: current.requested_at,
    });
    let warnings = if rule.evidence.iter().any(|kind| kind == "EVALUATION_PASSED") {
        vec!["M13 freezes the evaluation requirement expiry. M16 owns evaluation evidence authoring.".to_string()]
    } else {
        vec!["The local plan and change-summary evidence have no expiry timestamp.".to_string()]
    };
    DeploymentPreview {
        environment: PreviewEnvironment {
            id: environment.id,
            stable_definition_id: environment.stable_definition_id,
            version: environment.version,
            display_name: environment.display_name,
            logical_environment_class: environment.logical_environment_class,
            catalog_release_id: environment.catalog_release_id,
            catalog_release_digest: environment.catalog_release_digest,
            content_digest: environment.content_digest,
        },
        strategy,
        risk,
        policy_digest: policy.digest,
        policy_revision: policy.revision,
        required_evidence: rule.evidence,
        required_approvers: rule.approvers,
        plan_digest,
        package_digest,
        catalog_release_id: version.catalog_release_id,
        catalog_release_digest: version.catalog_release_digest,
        agent_content_digest: version.content_digest,
        target_digest,
        binding_digest,
        current_target,
        requirement_expires_at,
        warnings,
        compatibility: "The immutable version and environment definition resolve against the same catalog release.".to_string(),
    }
}
