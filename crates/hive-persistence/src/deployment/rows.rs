//! Row-mapping helpers shared by `queries`/`mutations`/`worker`: the batched
//! `Deployment` loader and every other `ResultSet -> struct` mapper Java's
//! `PostgresDeploymentRepository` defines as a private static method.

use crate::sql::parse_string_array;
use chrono::{DateTime, Utc};
use hive_application::deployment::{
    Deployment, DeploymentAttempt, DeploymentEnvironment, DeploymentEvidence, DeploymentPlan,
    DeploymentPlanReview, DeploymentPolicy, DeploymentRollbackTarget, DeploymentRuntimeHealth,
    VersionSource,
};
use hive_domain::deployment::{ApprovalRequirementStatus, DeploymentLifecycleStatus};
use sea_orm::sea_query::ArrayType;
use sea_orm::{ConnectionTrait, DbErr, QueryResult, Statement, Value};
use uuid::Uuid;

pub struct RawRequirement {
    pub id: Uuid,
    pub deployment_id: Uuid,
    pub project_id: Uuid,
    pub requester_id: Uuid,
    pub requested_at: DateTime<Utc>,
    pub revision: i64,
    pub status: ApprovalRequirementStatus,
    pub expires_at: Option<DateTime<Utc>>,
    pub satisfied_at: Option<DateTime<Utc>>,
    pub rejected_at: Option<DateTime<Utc>>,
    pub invalidated_at: Option<DateTime<Utc>>,
    pub invalidation_code: Option<String>,
    pub satisfied_participants: Vec<Uuid>,
    pub required_approvers: i32,
}

pub async fn raw_requirement(
    db: &impl ConnectionTrait,
    requirement_id: Uuid,
    lock: bool,
) -> Result<Option<RawRequirement>, DbErr> {
    let suffix = if lock {
        " FOR UPDATE OF requirement, deployment"
    } else {
        ""
    };
    let sql = format!(
        "SELECT requirement.id, requirement.deployment_id, deployment.project_id, deployment.requested_by, deployment.requested_at, \
             requirement.revision, requirement.status, requirement.expires_at, requirement.satisfied_at, requirement.rejected_at, \
             requirement.invalidated_at, requirement.invalidation_code, requirement.satisfied_participants::text, requirement.required_approvers \
         FROM deployment_approval_requirements requirement JOIN deployments deployment ON deployment.id = requirement.deployment_id \
         JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id \
         WHERE requirement.id = $1{suffix}"
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [requirement_id.into()]);
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(raw_requirement_row(&row)?)),
        None => Ok(None),
    }
}

/// Loads a requirement by deployment only for commands that already hold the deployment row lock.
pub async fn raw_requirement_by_deployment(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    lock: bool,
) -> Result<Option<RawRequirement>, DbErr> {
    let suffix = if lock {
        " FOR UPDATE OF requirement, deployment"
    } else {
        ""
    };
    let sql = format!(
        "SELECT requirement.id, requirement.deployment_id, deployment.project_id, deployment.requested_by, deployment.requested_at, \
             requirement.revision, requirement.status, requirement.expires_at, requirement.satisfied_at, requirement.rejected_at, \
             requirement.invalidated_at, requirement.invalidation_code, requirement.satisfied_participants::text, requirement.required_approvers \
         FROM deployment_approval_requirements requirement JOIN deployments deployment ON deployment.id = requirement.deployment_id \
         JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id \
         WHERE requirement.deployment_id = $1{suffix}"
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [deployment_id.into()]);
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(raw_requirement_row(&row)?)),
        None => Ok(None),
    }
}

/// Projects elapsed pending expiry for an inbox without contending on every healthy aggregate.
pub async fn raw_requirements(
    db: &impl ConnectionTrait,
    requirement_ids: &[Uuid],
    project_elapsed_expiry: bool,
) -> Result<std::collections::HashMap<Uuid, RawRequirement>, DbErr> {
    if requirement_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let status_expression = if project_elapsed_expiry {
        "CASE WHEN requirement.status = 'PENDING' AND requirement.expires_at <= clock_timestamp() THEN 'EXPIRED' ELSE requirement.status END AS status"
    } else {
        "requirement.status"
    };
    let sql = format!(
        "SELECT requirement.id, requirement.deployment_id, deployment.project_id, deployment.requested_by, deployment.requested_at, \
             requirement.revision, {status_expression}, requirement.expires_at, requirement.satisfied_at, requirement.rejected_at, \
             requirement.invalidated_at, requirement.invalidation_code, requirement.satisfied_participants::text, requirement.required_approvers \
         FROM deployment_approval_requirements requirement JOIN deployments deployment ON deployment.id = requirement.deployment_id \
         JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id \
         WHERE requirement.id = ANY($1)"
    );
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        &sql,
        [uuid_array(requirement_ids)],
    );
    let rows = db.query_all_raw(statement).await?;
    let mut values = std::collections::HashMap::new();
    for row in &rows {
        let raw = raw_requirement_row(row)?;
        values.insert(raw.id, raw);
    }
    Ok(values)
}

pub fn raw_requirement_row(row: &QueryResult) -> Result<RawRequirement, DbErr> {
    let participants_json: String = row.try_get_by("satisfied_participants")?;
    Ok(RawRequirement {
        id: row.try_get_by("id")?,
        deployment_id: row.try_get_by("deployment_id")?,
        project_id: row.try_get_by("project_id")?,
        requester_id: row.try_get_by("requested_by")?,
        requested_at: row.try_get_by("requested_at")?,
        revision: row.try_get_by("revision")?,
        status: requirement_status(row.try_get_by("status")?),
        expires_at: row.try_get_by("expires_at")?,
        satisfied_at: row.try_get_by("satisfied_at")?,
        rejected_at: row.try_get_by("rejected_at")?,
        invalidated_at: row.try_get_by("invalidated_at")?,
        invalidation_code: row.try_get_by("invalidation_code")?,
        satisfied_participants: parse_string_array(&participants_json)
            .into_iter()
            .filter_map(|value| Uuid::parse_str(&value).ok())
            .collect(),
        required_approvers: row.try_get_by("required_approvers")?,
    })
}

pub struct ApprovalDecisionRow {
    pub id: Uuid,
    pub requirement_id: Uuid,
    pub actor_principal_id: Uuid,
    pub value: String,
    pub comment: Option<String>,
    pub rejection_reason: Option<String>,
    pub eligibility_checked_at: DateTime<Utc>,
    pub decided_at: DateTime<Utc>,
}

const REVIEW_CODES: [&str; 4] = [
    "REVIEWED_CHANGE_SCOPE",
    "AUTHORIZATION_GRANTED",
    "UNACCEPTABLE_CHANGE_SCOPE",
    "CHANGE_SCOPE_NOT_APPROVED",
];

/// Projects only M14's finite review-code vocabulary from immutable predecessor rows.
pub fn review_text(value: Option<String>) -> Option<String> {
    let value = value?;
    let normalized = value.trim().to_string();
    REVIEW_CODES
        .contains(&normalized.as_str())
        .then_some(normalized)
}

pub fn decision_row(row: &QueryResult) -> Result<ApprovalDecisionRow, DbErr> {
    Ok(ApprovalDecisionRow {
        id: row.try_get_by("id")?,
        requirement_id: row.try_get_by("approval_requirement_id")?,
        actor_principal_id: row.try_get_by("actor_principal_id")?,
        value: row.try_get_by("decision")?,
        comment: review_text(row.try_get_by("comment")?),
        rejection_reason: review_text(row.try_get_by("rejection_reason")?),
        eligibility_checked_at: row.try_get_by("eligibility_checked_at")?,
        decided_at: row.try_get_by("decided_at")?,
    })
}

pub fn compiler_environment_row(
    row: &QueryResult,
) -> Result<hive_application::deployment::EnvironmentDefinition, DbErr> {
    Ok(hive_application::deployment::EnvironmentDefinition {
        id: row.try_get_by("id")?,
        stable_definition_id: row.try_get_by("stable_definition_id")?,
        version: row.try_get_by("version")?,
        display_name: row.try_get_by("display_name")?,
        logical_environment_class: row.try_get_by("logical_environment_class")?,
        catalog_release_id: row.try_get_by("catalog_release_id")?,
        catalog_release_digest: row.try_get_by("catalog_release_digest")?,
        content_digest: row.try_get_by("content_digest")?,
    })
}

pub fn version_source_row(row: &QueryResult) -> Result<VersionSource, DbErr> {
    Ok(VersionSource {
        id: row.try_get_by("version_id")?,
        project_id: row.try_get_by("project_id")?,
        agent_id: row.try_get_by("agent_id")?,
        agent_display_name: row.try_get_by("display_name")?,
        version_number: row.try_get_by("version_number")?,
        content_digest: row.try_get_by("content_digest")?,
        catalog_release_id: row.try_get_by("catalog_release_id")?,
        catalog_release_digest: row.try_get_by("catalog_release_digest")?,
        organization_id: row.try_get_by("organization_id")?,
        canonical_document: row.try_get_by("canonical_document")?,
    })
}

fn evidence_from_json(json: &str) -> Vec<DeploymentEvidence> {
    let values: Vec<serde_json::Value> =
        serde_json::from_str(json).expect("deployment evidence projection is always a JSON array");
    values
        .into_iter()
        .map(|value| DeploymentEvidence {
            kind: value["kind"]
                .as_str()
                .expect("evidence kind is always present")
                .to_string(),
            digest: value["digest"].as_str().map(str::to_string),
            binding_digest: value["bindingDigest"].as_str().map(str::to_string),
            expires_at: value["expiresAt"]
                .as_str()
                .and_then(|text| DateTime::parse_from_rfc3339(&text.replace(' ', "T")).ok())
                .map(|value| value.with_timezone(&Utc)),
            state: value["state"]
                .as_str()
                .expect("evidence state is always present")
                .to_string(),
        })
        .collect()
}

const DEPLOYMENT_COLUMNS: &str = "deployment.id, deployment.project_id, deployment.agent_id, agent.display_name, deployment.agent_version_id, version.version_number, \
      deployment.strategy, deployment.lifecycle_status, deployment.revision, deployment.projection_revision, deployment.requested_by, deployment.requested_at, \
      environment.id AS environment_definition_version_id, environment.stable_definition_id, environment.version AS environment_version, \
      environment.display_name AS environment_display_name, environment.logical_environment_class, environment.catalog_release_id AS environment_catalog_release_id, \
      environment.catalog_release_digest AS environment_catalog_release_digest, environment.content_digest AS environment_content_digest, \
      plan.agent_content_digest, plan.target_digest, plan.plan_digest, plan.package_digest, plan.package_reference, plan.compiler_version, \
      plan.catalog_release_id, plan.catalog_release_digest, %CANONICAL_PLAN% AS canonical_plan, \
      review.active_agent_version_number AS review_active_agent_version_number, \
      COALESCE(review.change_summary, 'Retained plan review facts are unavailable.') AS review_change_summary, \
      COALESCE(review.added_dependency_versions, '[]'::jsonb)::text AS review_added_dependency_versions, \
      COALESCE(review.removed_dependency_versions, '[]'::jsonb)::text AS review_removed_dependency_versions, \
      policy.policy_digest, policy.policy_revision, \
      policy.logical_environment_class AS policy_environment_class, policy.risk, policy.binding_digest, policy.required_evidence::text AS required_evidence, \
      policy.required_approvers, policy.evaluation_requirement_expires_at, health.status AS health_status, health.summary AS health_summary, health.observed_at, health.generation AS health_generation, \
      attempt.id AS attempt_id, attempt.attempt_number, attempt.status AS attempt_status, attempt.generation AS attempt_generation, \
      attempt.started_at, attempt.completed_at, attempt.failure_code, attempt.failure_summary, \
      rollback_target.deployment_id AS rollback_deployment_id, rollback_target.agent_version_id AS rollback_agent_version_id, \
      rollback_target.agent_version_number AS rollback_agent_version_number, rollback_target.target_digest AS rollback_target_digest, \
      rollback_target.health_status AS rollback_health_status, rollback_target.health_summary AS rollback_health_summary, \
      rollback_target.observed_at AS rollback_observed_at, rollback_target.health_generation AS rollback_health_generation, \
      COALESCE((SELECT jsonb_agg(jsonb_build_object('kind', evidence.evidence_kind, 'digest', evidence.evidence_digest, \
        'bindingDigest', evidence.binding_digest, 'expiresAt', evidence.expires_at::text, 'state', CASE \
          WHEN EXISTS (SELECT 1 FROM deployment_evidence_invalidations invalidation \
                       WHERE invalidation.evidence_snapshot_id = evidence.id AND invalidation.kind = 'FAILED') THEN 'FAILED' \
          WHEN EXISTS (SELECT 1 FROM deployment_evidence_invalidations invalidation \
                       WHERE invalidation.evidence_snapshot_id = evidence.id AND invalidation.kind = 'REVOKED') THEN 'REVOKED' \
          WHEN evidence.expires_at <= clock_timestamp() THEN 'EXPIRED' \
          WHEN evidence.binding_digest = policy.binding_digest AND evidence.agent_version_id = policy.agent_version_id \
            AND evidence.environment_definition_version_id = policy.environment_definition_version_id \
            AND evidence.target_digest = policy.target_digest AND evidence.plan_digest = policy.plan_digest \
            AND evidence.package_digest = policy.package_digest THEN 'VALID' \
          ELSE 'MISMATCH' END) ORDER BY evidence.evidence_kind)::text \
        FROM deployment_evidence_snapshots evidence WHERE evidence.deployment_id = deployment.id), '[]') AS evidence_json";

const DEPLOYMENT_FROM: &str = "FROM deployments deployment JOIN agents agent ON agent.id = deployment.agent_id JOIN agent_versions version ON version.id = deployment.agent_version_id \
      JOIN environment_definition_versions environment ON environment.id = deployment.environment_definition_version_id \
      JOIN deployment_plan_versions plan ON plan.deployment_id = deployment.id AND plan.version_number = 1 \
      %REVIEW_JOIN% \
      JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id \
      JOIN deployment_runtime_health health ON health.deployment_id = deployment.id \
      LEFT JOIN LATERAL (SELECT id, attempt_number, status, generation, started_at, completed_at, failure_code, failure_summary \
        FROM deployment_attempts WHERE deployment_id = deployment.id ORDER BY attempt_number DESC LIMIT 1) attempt ON true \
      LEFT JOIN LATERAL ( \
        SELECT candidate.id AS deployment_id, candidate.agent_version_id, candidate_version.version_number AS agent_version_number, \
          candidate_plan.target_digest, candidate_health.status AS health_status, candidate_health.summary AS health_summary, \
          candidate_health.observed_at, candidate_health.generation AS health_generation \
        FROM deployments candidate JOIN agent_versions candidate_version ON candidate_version.id = candidate.agent_version_id \
          JOIN deployment_plan_versions candidate_plan ON candidate_plan.deployment_id = candidate.id AND candidate_plan.version_number = 1 \
          JOIN deployment_runtime_health candidate_health ON candidate_health.deployment_id = candidate.id \
        WHERE candidate.project_id = deployment.project_id AND candidate.agent_id = deployment.agent_id \
          AND candidate.environment_definition_version_id = deployment.environment_definition_version_id \
          AND candidate.lifecycle_status = 'ACTIVE' AND candidate.id <> deployment.id \
          AND (candidate.requested_at, candidate.id) < (deployment.requested_at, deployment.id) \
        ORDER BY candidate.requested_at DESC, candidate.id DESC LIMIT 1 \
      ) rollback_target ON true";

pub(crate) fn uuid_array(ids: &[Uuid]) -> Value {
    Value::Array(
        ArrayType::Uuid,
        Some(Box::new(ids.iter().map(|id| Value::from(*id)).collect())),
    )
}

pub async fn deployments(
    db: &impl ConnectionTrait,
    ids: &[Uuid],
    include_canonical_plan: bool,
) -> Result<Vec<Deployment>, DbErr> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let canonical_plan = if include_canonical_plan {
        "plan.canonical_plan::text"
    } else {
        "NULL::text"
    };
    let review_join = if include_canonical_plan {
        "LEFT JOIN deployment_plan_review_facts review ON review.plan_id = plan.id"
    } else {
        "JOIN deployment_plan_review_facts review ON review.plan_id = plan.id"
    };
    let sql = format!(
        "SELECT {} {} WHERE deployment.id = ANY($1) ORDER BY deployment.requested_at DESC, deployment.id DESC",
        DEPLOYMENT_COLUMNS.replace("%CANONICAL_PLAN%", canonical_plan),
        DEPLOYMENT_FROM.replace("%REVIEW_JOIN%", review_join),
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [uuid_array(ids)]);
    let rows = db.query_all_raw(statement).await?;
    rows.iter().map(deployment_row).collect()
}

fn deployment_row(row: &QueryResult) -> Result<Deployment, DbErr> {
    let required_evidence_json: String = row.try_get_by("required_evidence")?;
    let environment = DeploymentEnvironment {
        id: row.try_get_by("environment_definition_version_id")?,
        stable_definition_id: row.try_get_by("stable_definition_id")?,
        version: row.try_get_by("environment_version")?,
        display_name: row.try_get_by("environment_display_name")?,
        logical_environment_class: row.try_get_by("logical_environment_class")?,
        catalog_release_id: row.try_get_by("environment_catalog_release_id")?,
        catalog_release_digest: row.try_get_by("environment_catalog_release_digest")?,
        content_digest: row.try_get_by("environment_content_digest")?,
    };
    let added_dependency_versions_json: String =
        row.try_get_by("review_added_dependency_versions")?;
    let removed_dependency_versions_json: String =
        row.try_get_by("review_removed_dependency_versions")?;
    let plan = DeploymentPlan {
        agent_version_id: row.try_get_by("agent_version_id")?,
        agent_content_digest: row.try_get_by("agent_content_digest")?,
        environment_definition_version_id: row.try_get_by("environment_definition_version_id")?,
        target_digest: row.try_get_by("target_digest")?,
        plan_digest: row.try_get_by("plan_digest")?,
        package_digest: row.try_get_by("package_digest")?,
        package_reference: row.try_get_by("package_reference")?,
        compiler_version: row.try_get_by("compiler_version")?,
        catalog_release_id: row.try_get_by("catalog_release_id")?,
        catalog_release_digest: row.try_get_by("catalog_release_digest")?,
        canonical_plan: row.try_get_by("canonical_plan")?,
        review: DeploymentPlanReview {
            active_agent_version_number: row.try_get_by("review_active_agent_version_number")?,
            change_summary: row.try_get_by("review_change_summary")?,
            added_dependency_versions: parse_string_array(&added_dependency_versions_json),
            removed_dependency_versions: parse_string_array(&removed_dependency_versions_json),
        },
    };
    let evidence_json: String = row.try_get_by("evidence_json")?;
    let policy = DeploymentPolicy {
        policy_digest: row.try_get_by("policy_digest")?,
        policy_revision: row.try_get_by("policy_revision")?,
        logical_environment_class: row.try_get_by("policy_environment_class")?,
        risk: row.try_get_by("risk")?,
        binding_digest: row.try_get_by("binding_digest")?,
        required_evidence: parse_string_array(&required_evidence_json),
        required_approvers: row.try_get_by("required_approvers")?,
        evaluation_requirement_expires_at: row.try_get_by("evaluation_requirement_expires_at")?,
        evidence: evidence_from_json(&evidence_json),
    };
    let attempt_id: Option<Uuid> = row.try_get_by("attempt_id")?;
    let current_attempt = match attempt_id {
        Some(id) => Some(DeploymentAttempt {
            id,
            number: row.try_get_by("attempt_number")?,
            status: row.try_get_by("attempt_status")?,
            generation: row.try_get_by("attempt_generation")?,
            started_at: row.try_get_by("started_at")?,
            completed_at: row.try_get_by("completed_at")?,
            failure_code: row.try_get_by("failure_code")?,
            failure_summary: row.try_get_by("failure_summary")?,
        }),
        None => None,
    };
    let rollback_deployment_id: Option<Uuid> = row.try_get_by("rollback_deployment_id")?;
    let rollback_target = match rollback_deployment_id {
        Some(deployment_id) => Some(DeploymentRollbackTarget {
            deployment_id,
            agent_version_id: row.try_get_by("rollback_agent_version_id")?,
            agent_version_number: row.try_get_by("rollback_agent_version_number")?,
            target_digest: row.try_get_by("rollback_target_digest")?,
            runtime_health: DeploymentRuntimeHealth {
                status: row.try_get_by("rollback_health_status")?,
                summary: row.try_get_by("rollback_health_summary")?,
                observed_at: row.try_get_by("rollback_observed_at")?,
                generation: row.try_get_by("rollback_health_generation")?,
            },
        }),
        None => None,
    };
    Ok(Deployment {
        id: row.try_get_by("id")?,
        project_id: row.try_get_by("project_id")?,
        agent_id: row.try_get_by("agent_id")?,
        agent_display_name: row.try_get_by("display_name")?,
        agent_version_id: row.try_get_by("agent_version_id")?,
        agent_version_number: row.try_get_by("version_number")?,
        environment,
        strategy: row.try_get_by("strategy")?,
        lifecycle_status: lifecycle_status(row.try_get_by("lifecycle_status")?),
        revision: row.try_get_by("revision")?,
        projection_revision: row.try_get_by("projection_revision")?,
        requested_by: row.try_get_by("requested_by")?,
        requested_at: row.try_get_by("requested_at")?,
        plan,
        policy,
        current_attempt,
        runtime_health: DeploymentRuntimeHealth {
            status: row.try_get_by("health_status")?,
            summary: row.try_get_by("health_summary")?,
            observed_at: row.try_get_by("observed_at")?,
            generation: row.try_get_by("health_generation")?,
        },
        rollback_target,
    })
}

/// Parses `deployments.lifecycle_status` once at the row boundary. The column's CHECK constraint
/// admits only the values `DeploymentLifecycleStatus` names, so an unrecognized value is schema
/// drift and panics, as the GraphQL-layer parse of the same string did before this type existed.
pub fn lifecycle_status(value: String) -> DeploymentLifecycleStatus {
    value.parse().unwrap_or_else(|error| panic!("{error}"))
}

/// Parses `deployment_approval_requirements.status` once at the row boundary; an unrecognized
/// value is schema drift against the column's CHECK constraint and panics.
pub fn requirement_status(value: String) -> ApprovalRequirementStatus {
    value.parse().unwrap_or_else(|error| panic!("{error}"))
}
