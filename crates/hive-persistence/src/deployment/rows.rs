//! Row-mapping helpers shared by `queries`/`mutations`/`worker`: the batched
//! `Deployment` loader and every other `ResultSet -> struct` mapper Java's
//! `PostgresDeploymentRepository` defines as a private static method.

use crate::sql::parse_string_array;
use chrono::{DateTime, Utc};
use hive_application::deployment::{
    Deployment, DeploymentAttempt, DeploymentEnvironment, DeploymentEvidence, DeploymentPlan,
    DeploymentPlanReview, DeploymentPolicy, DeploymentRollbackTarget, DeploymentRuntimeHealth,
    DeploymentTimelineEvent, EnvironmentVersion, VersionSource,
};
use hive_domain::deployment::{ApprovalRequirementStatus, DeploymentLifecycleStatus};
use sqlx::{PgConnection, Row};
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
    conn: &mut PgConnection,
    requirement_id: Uuid,
    lock: bool,
) -> Result<Option<RawRequirement>, sqlx::Error> {
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
    let row = sqlx::query(&sql)
        .bind(requirement_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.as_ref().map(raw_requirement_row))
}

/// Loads a requirement by deployment only for commands that already hold the deployment row lock.
pub async fn raw_requirement_by_deployment(
    conn: &mut PgConnection,
    deployment_id: Uuid,
    lock: bool,
) -> Result<Option<RawRequirement>, sqlx::Error> {
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
    let row = sqlx::query(&sql)
        .bind(deployment_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.as_ref().map(raw_requirement_row))
}

/// Projects elapsed pending expiry for an inbox without contending on every healthy aggregate.
pub async fn raw_requirements(
    conn: &mut PgConnection,
    requirement_ids: &[Uuid],
    project_elapsed_expiry: bool,
) -> Result<std::collections::HashMap<Uuid, RawRequirement>, sqlx::Error> {
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
    let rows = sqlx::query(&sql)
        .bind(requirement_ids)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows
        .iter()
        .map(raw_requirement_row)
        .map(|raw| (raw.id, raw))
        .collect())
}

pub fn raw_requirement_row(row: &sqlx::postgres::PgRow) -> RawRequirement {
    let participants_json: String = row.get(12);
    RawRequirement {
        id: row.get(0),
        deployment_id: row.get(1),
        project_id: row.get(2),
        requester_id: row.get(3),
        requested_at: row.get(4),
        revision: row.get(5),
        status: requirement_status(row.get(6)),
        expires_at: row.get(7),
        satisfied_at: row.get(8),
        rejected_at: row.get(9),
        invalidated_at: row.get(10),
        invalidation_code: row.get(11),
        satisfied_participants: parse_string_array(&participants_json)
            .into_iter()
            .filter_map(|value| Uuid::parse_str(&value).ok())
            .collect(),
        required_approvers: row.get(13),
    }
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

pub fn decision_row(row: &sqlx::postgres::PgRow) -> ApprovalDecisionRow {
    ApprovalDecisionRow {
        id: row.get("id"),
        requirement_id: row.get("approval_requirement_id"),
        actor_principal_id: row.get("actor_principal_id"),
        value: row.get("decision"),
        comment: review_text(row.get("comment")),
        rejection_reason: review_text(row.get("rejection_reason")),
        eligibility_checked_at: row.get("eligibility_checked_at"),
        decided_at: row.get("decided_at"),
    }
}

pub fn timeline_row(row: &sqlx::postgres::PgRow) -> DeploymentTimelineEvent {
    DeploymentTimelineEvent {
        id: row.get("id"),
        attempt_id: row.get("attempt_id"),
        attempt_number: row.get("attempt_number"),
        sequence: row.get("sequence_number"),
        stage: row.get("stage"),
        status: row.get("status"),
        message: row.get("message"),
        source: row.get("source"),
        occurred_at: row.get("occurred_at"),
    }
}

pub fn environment_row(row: &sqlx::postgres::PgRow) -> EnvironmentVersion {
    let id: Uuid = row.get(0);
    EnvironmentVersion {
        id: id.to_string(),
        stable_definition_id: row.get(1),
        version: row.get(2),
        display_name: row.get(3),
        logical_environment_class: row.get(4),
        catalog_release_id: row.get(5),
        catalog_release_digest: row.get(6),
        content_digest: row.get(7),
    }
}

pub fn compiler_environment_row(
    row: &sqlx::postgres::PgRow,
) -> hive_application::deployment::EnvironmentDefinition {
    hive_application::deployment::EnvironmentDefinition {
        id: row.get(0),
        stable_definition_id: row.get(1),
        version: row.get(2),
        display_name: row.get(3),
        logical_environment_class: row.get(4),
        catalog_release_id: row.get(5),
        catalog_release_digest: row.get(6),
        content_digest: row.get(7),
    }
}

pub fn version_source_row(row: &sqlx::postgres::PgRow) -> VersionSource {
    VersionSource {
        id: row.get(0),
        project_id: row.get(1),
        agent_id: row.get(2),
        agent_display_name: row.get(3),
        version_number: row.get(4),
        content_digest: row.get(5),
        catalog_release_id: row.get(6),
        catalog_release_digest: row.get(7),
        organization_id: row.get(8),
        canonical_document: row.get(9),
    }
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

pub async fn deployments(
    conn: &mut PgConnection,
    ids: &[Uuid],
    include_canonical_plan: bool,
) -> Result<Vec<Deployment>, sqlx::Error> {
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
    let rows = sqlx::query(&sql).bind(ids).fetch_all(&mut *conn).await?;
    Ok(rows.iter().map(deployment_row).collect())
}

fn deployment_row(row: &sqlx::postgres::PgRow) -> Deployment {
    let required_evidence_json: String = row.get("required_evidence");
    let environment = DeploymentEnvironment {
        id: row.get("environment_definition_version_id"),
        stable_definition_id: row.get("stable_definition_id"),
        version: row.get("environment_version"),
        display_name: row.get("environment_display_name"),
        logical_environment_class: row.get("logical_environment_class"),
        catalog_release_id: row.get("environment_catalog_release_id"),
        catalog_release_digest: row.get("environment_catalog_release_digest"),
        content_digest: row.get("environment_content_digest"),
    };
    let added_dependency_versions_json: String = row.get("review_added_dependency_versions");
    let removed_dependency_versions_json: String = row.get("review_removed_dependency_versions");
    let plan = DeploymentPlan {
        agent_version_id: row.get("agent_version_id"),
        agent_content_digest: row.get("agent_content_digest"),
        environment_definition_version_id: row.get("environment_definition_version_id"),
        target_digest: row.get("target_digest"),
        plan_digest: row.get("plan_digest"),
        package_digest: row.get("package_digest"),
        package_reference: row.get("package_reference"),
        compiler_version: row.get("compiler_version"),
        catalog_release_id: row.get("catalog_release_id"),
        catalog_release_digest: row.get("catalog_release_digest"),
        canonical_plan: row.get("canonical_plan"),
        review: DeploymentPlanReview {
            active_agent_version_number: row.get("review_active_agent_version_number"),
            change_summary: row.get("review_change_summary"),
            added_dependency_versions: parse_string_array(&added_dependency_versions_json),
            removed_dependency_versions: parse_string_array(&removed_dependency_versions_json),
        },
    };
    let evidence_json: String = row.get("evidence_json");
    let policy = DeploymentPolicy {
        policy_digest: row.get("policy_digest"),
        policy_revision: row.get("policy_revision"),
        logical_environment_class: row.get("policy_environment_class"),
        risk: row.get("risk"),
        binding_digest: row.get("binding_digest"),
        required_evidence: parse_string_array(&required_evidence_json),
        required_approvers: row.get("required_approvers"),
        evaluation_requirement_expires_at: row.get("evaluation_requirement_expires_at"),
        evidence: evidence_from_json(&evidence_json),
    };
    let attempt_id: Option<Uuid> = row.get("attempt_id");
    let current_attempt = attempt_id.map(|id| DeploymentAttempt {
        id,
        number: row.get("attempt_number"),
        status: row.get("attempt_status"),
        generation: row.get("attempt_generation"),
        started_at: row.get("started_at"),
        completed_at: row.get("completed_at"),
        failure_code: row.get("failure_code"),
        failure_summary: row.get("failure_summary"),
    });
    let rollback_deployment_id: Option<Uuid> = row.get("rollback_deployment_id");
    let rollback_target = rollback_deployment_id.map(|deployment_id| DeploymentRollbackTarget {
        deployment_id,
        agent_version_id: row.get("rollback_agent_version_id"),
        agent_version_number: row.get("rollback_agent_version_number"),
        target_digest: row.get("rollback_target_digest"),
        runtime_health: DeploymentRuntimeHealth {
            status: row.get("rollback_health_status"),
            summary: row.get("rollback_health_summary"),
            observed_at: row.get("rollback_observed_at"),
            generation: row.get("rollback_health_generation"),
        },
    });
    Deployment {
        id: row.get("id"),
        project_id: row.get("project_id"),
        agent_id: row.get("agent_id"),
        agent_display_name: row.get("display_name"),
        agent_version_id: row.get("agent_version_id"),
        agent_version_number: row.get("version_number"),
        environment,
        strategy: row.get("strategy"),
        lifecycle_status: lifecycle_status(row.get("lifecycle_status")),
        revision: row.get("revision"),
        projection_revision: row.get("projection_revision"),
        requested_by: row.get("requested_by"),
        requested_at: row.get("requested_at"),
        plan,
        policy,
        current_attempt,
        runtime_health: DeploymentRuntimeHealth {
            status: row.get("health_status"),
            summary: row.get("health_summary"),
            observed_at: row.get("observed_at"),
            generation: row.get("health_generation"),
        },
        rollback_target,
    }
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
