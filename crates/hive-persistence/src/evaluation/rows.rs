//! Row-mapping helpers shared by `queries`/`mutations`/`worker`.

use chrono::{DateTime, Utc};
use hive_application::evaluation::document::EvaluationDiagnostic;
use hive_application::evaluation::EvaluationRunStatus;
use hive_application::evaluation::{
    EvaluationArtifactMetadata, EvaluationAuditEvent, EvaluationCaseRun, EvaluationDefinitionDraft,
    EvaluationDefinitionVersion, EvaluationMetricResult, EvaluationRun, EvaluationTarget,
    EvaluationTargetSnapshot,
};
use sea_orm::{DbErr, QueryResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct RawDiagnostic {
    code: String,
    severity: String,
    message: String,
    path: Vec<String>,
}

pub fn parse_diagnostics(json: &str) -> Vec<EvaluationDiagnostic> {
    let raw: Vec<RawDiagnostic> =
        serde_json::from_str(json).expect("stored evaluation diagnostics are always valid JSON");
    raw.into_iter()
        .map(|value| EvaluationDiagnostic {
            code: value.code,
            severity: value.severity,
            message: value.message,
            path: value.path,
        })
        .collect()
}

pub fn diagnostics_json(diagnostics: &[EvaluationDiagnostic]) -> String {
    let raw: Vec<RawDiagnostic> = diagnostics
        .iter()
        .map(|value| RawDiagnostic {
            code: value.code.clone(),
            severity: value.severity.clone(),
            message: value.message.clone(),
            path: value.path.clone(),
        })
        .collect();
    serde_json::to_string(&raw).expect("diagnostics always serialize")
}

/// Ports the `draft`/`redacted(EvaluationDefinitionDraft)` shape: `document`/`diagnostics` are
/// fetched in full by every query here and redacted afterward in Rust (not via Java's `CASE WHEN ?`
/// bind trick), since both approaches keep the real content equally server-side-only.
#[allow(clippy::too_many_arguments)]
pub fn draft_row(
    definition_id: Uuid,
    document: String,
    revision: i64,
    validation_status: String,
    diagnostics_text: &str,
    based_on_version_id: Option<Uuid>,
    updated_at: Option<DateTime<Utc>>,
) -> EvaluationDefinitionDraft {
    EvaluationDefinitionDraft {
        definition_id,
        canonical_document: document,
        revision,
        validation_status,
        diagnostics: parse_diagnostics(diagnostics_text),
        based_on_version_id,
        updated_at,
    }
}

pub fn redacted_draft(value: &EvaluationDefinitionDraft) -> EvaluationDefinitionDraft {
    EvaluationDefinitionDraft {
        definition_id: value.definition_id,
        canonical_document: String::new(),
        revision: value.revision,
        validation_status: value.validation_status.clone(),
        diagnostics: Vec::new(),
        based_on_version_id: value.based_on_version_id,
        updated_at: value.updated_at,
    }
}

pub fn redacted_version(value: &EvaluationDefinitionVersion) -> EvaluationDefinitionVersion {
    EvaluationDefinitionVersion {
        id: value.id,
        definition_id: value.definition_id,
        number: value.number,
        canonical_document: String::new(),
        content_digest: value.content_digest.clone(),
        based_on_version_id: value.based_on_version_id,
        published_by: value.published_by,
        published_at: value.published_at,
    }
}

pub fn version_row(row: &QueryResult, prefix: &str) -> Result<EvaluationDefinitionVersion, DbErr> {
    Ok(EvaluationDefinitionVersion {
        id: row.try_get_by(format!("{prefix}id").as_str())?,
        definition_id: row.try_get_by(format!("{prefix}definition_id").as_str())?,
        number: row.try_get_by(format!("{prefix}version_number").as_str())?,
        canonical_document: row.try_get_by(format!("{prefix}canonical_document").as_str())?,
        content_digest: row.try_get_by(format!("{prefix}content_digest").as_str())?,
        based_on_version_id: row.try_get_by(format!("{prefix}based_on_version_id").as_str())?,
        published_by: row.try_get_by(format!("{prefix}published_by").as_str())?,
        published_at: row.try_get_by(format!("{prefix}published_at").as_str())?,
    })
}

pub fn case_row(row: &QueryResult) -> Result<EvaluationCaseRun, DbErr> {
    Ok(EvaluationCaseRun {
        id: row.try_get_by("id")?,
        key: row.try_get_by("case_key")?,
        ordinal: row.try_get_by("ordinal")?,
        lifecycle_status: row.try_get_by("lifecycle_status")?,
        passed: row.try_get_by("passed")?,
        failure_code: row.try_get_by("failure_code")?,
        completed_at: row.try_get_by("completed_at")?,
    })
}

/// The query binds `value`/`threshold` cast to `float8` (not `numeric`) so this can decode them as
/// plain `f64`, avoiding a dependency on sqlx's `bigdecimal` feature — matching how
/// `EvaluationScoringPolicy`'s already-ported Rust port (`scoring.rs`) also computes rates as `f64`.
pub fn metric_row(row: &QueryResult) -> Result<EvaluationMetricResult, DbErr> {
    Ok(EvaluationMetricResult {
        id: row.try_get_by("id")?,
        code: row.try_get_by("metric_code")?,
        value: row.try_get_by("value")?,
        threshold: row.try_get_by("threshold")?,
        passed: row.try_get_by("passed")?,
    })
}

pub fn artifact_row(row: &QueryResult) -> Result<EvaluationArtifactMetadata, DbErr> {
    Ok(EvaluationArtifactMetadata {
        id: row.try_get_by("id")?,
        kind: row.try_get_by("artifact_kind")?,
        content_digest: row.try_get_by("content_digest")?,
        media_type: row.try_get_by("media_type")?,
        byte_length: row.try_get_by("byte_length")?,
    })
}

pub fn audit_row(row: &QueryResult) -> Result<EvaluationAuditEvent, DbErr> {
    Ok(EvaluationAuditEvent {
        id: row.try_get_by("id")?,
        action: row.try_get_by("action")?,
        occurred_at: row.try_get_by("occurred_at")?,
        summary: row
            .try_get_by::<Option<String>, _>("summary")?
            .unwrap_or_default(),
    })
}

pub fn target_row(row: &QueryResult) -> Result<EvaluationTarget, DbErr> {
    Ok(EvaluationTarget {
        kind: row.try_get_by("target_kind")?,
        id: row.try_get_by("target_id")?,
        agent_version_id: row.try_get_by("agent_version_id")?,
        environment_definition_version_id: row.try_get_by("environment_definition_version_id")?,
        logical_environment_class: row.try_get_by("logical_environment_class")?,
        display_name: row.try_get_by("display_name")?,
    })
}

/// Ports the private `Target` record: the resolved facts one of `target()`'s two branches (or
/// `targetFromSnapshot()`) supplies before `run()`/`rerun()` insert `evaluation_target_snapshots`.
/// `deployment`/`target_digest`/`plan_digest`/`package_digest`/`binding_digest` are `None` for an
/// `AGENT_VERSION` target — only a `DEPLOYMENT` target's policy snapshot supplies them (see
/// `queries::resolve_target`'s two branches).
pub struct Target {
    pub agent_version_id: Uuid,
    pub deployment_id: Option<Uuid>,
    pub environment_definition_version_id: Uuid,
    pub environment_class: String,
    pub agent_digest: String,
    pub target_digest: Option<String>,
    pub plan_digest: Option<String>,
    pub package_digest: Option<String>,
    pub binding_digest: Option<String>,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub environment_digest: String,
}

pub fn target_from_row(row: &QueryResult) -> Result<Target, DbErr> {
    Ok(Target {
        agent_version_id: row.try_get_by("agent_version_id")?,
        deployment_id: row.try_get_by("deployment_id")?,
        environment_definition_version_id: row.try_get_by("environment_definition_version_id")?,
        environment_class: row.try_get_by("environment_class")?,
        agent_digest: row.try_get_by("agent_digest")?,
        target_digest: row.try_get_by("target_digest")?,
        plan_digest: row.try_get_by("plan_digest")?,
        package_digest: row.try_get_by("package_digest")?,
        binding_digest: row.try_get_by("binding_digest")?,
        catalog_release_id: row.try_get_by("catalog_release_id")?,
        catalog_release_digest: row.try_get_by("catalog_release_digest")?,
        environment_digest: row.try_get_by("environment_digest")?,
    })
}

pub fn snapshot_row(row: &QueryResult) -> Result<EvaluationTargetSnapshot, DbErr> {
    Ok(EvaluationTargetSnapshot {
        agent_version_id: row.try_get_by("agent_version_id")?,
        deployment_id: row.try_get_by("deployment_id")?,
        environment_definition_version_id: row.try_get_by("environment_definition_version_id")?,
        logical_environment_class: row.try_get_by("logical_environment_class")?,
        agent_content_digest: row.try_get_by("agent_content_digest")?,
        target_digest: row.try_get_by("target_digest")?,
        plan_digest: row.try_get_by("plan_digest")?,
        package_digest: row.try_get_by("package_digest")?,
        binding_digest: row.try_get_by("binding_digest")?,
        catalog_release_id: row.try_get_by("catalog_release_id")?,
        catalog_release_digest: row.try_get_by("catalog_release_digest")?,
        environment_content_digest: row.try_get_by("environment_content_digest")?,
    })
}

/// Ports the private `RawRun` record.
#[derive(Clone)]
pub struct RawRun {
    pub id: Uuid,
    pub project_id: Uuid,
    pub definition_version_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub environment_id: Uuid,
    pub source_run_id: Option<Uuid>,
    pub status: EvaluationRunStatus,
    pub generation: i64,
    pub outcome_category: Option<String>,
    pub outcome_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

pub fn raw_run_row(row: &QueryResult) -> Result<RawRun, DbErr> {
    Ok(RawRun {
        id: row.try_get_by("id")?,
        project_id: row.try_get_by("project_id")?,
        definition_version_id: row.try_get_by("definition_version_id")?,
        target_kind: row.try_get_by("target_kind")?,
        target_id: row.try_get_by("target_id")?,
        environment_id: row.try_get_by("environment_definition_version_id")?,
        source_run_id: row.try_get_by("source_run_id")?,
        status: run_status(row.try_get_by("lifecycle_status")?),
        generation: row.try_get_by("generation")?,
        outcome_category: row.try_get_by("outcome_category")?,
        outcome_code: row.try_get_by("outcome_code")?,
        created_at: row.try_get_by("created_at")?,
        started_at: row.try_get_by("started_at")?,
        completed_at: row.try_get_by("completed_at")?,
    })
}

pub fn run_from_raw(
    raw: &RawRun,
    target: Option<EvaluationTargetSnapshot>,
    deployment_evidence_disposition: String,
) -> EvaluationRun {
    EvaluationRun {
        id: raw.id,
        project_id: raw.project_id,
        definition_version_id: raw.definition_version_id,
        target_kind: raw.target_kind.clone(),
        target_id: raw.target_id,
        environment_definition_version_id: raw.environment_id,
        source_run_id: raw.source_run_id,
        lifecycle_status: raw.status,
        generation: raw.generation,
        outcome_category: raw.outcome_category.clone(),
        outcome_code: raw.outcome_code.clone(),
        created_at: raw.created_at,
        started_at: raw.started_at,
        completed_at: raw.completed_at,
        target,
        deployment_evidence_disposition,
    }
}

/// Ports the private `Event` record: one claimed `evaluation_outbox_events` row.
#[derive(Clone)]
pub struct Event {
    pub id: Uuid,
    pub run_id: Uuid,
    pub event_type: String,
    pub case_run_id: Option<Uuid>,
    pub attempts: i32,
}

pub async fn definition_project(
    db: &impl sea_orm::ConnectionTrait,
    definition_id: Uuid,
) -> Result<Option<Uuid>, DbErr> {
    let statement = sea_orm::Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT project_id FROM evaluation_definitions WHERE id = $1",
        [definition_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(row.try_get_by("project_id")?)),
        None => Ok(None),
    }
}

pub async fn version_project(
    db: &impl sea_orm::ConnectionTrait,
    version_id: Uuid,
) -> Result<Option<Uuid>, DbErr> {
    let statement = sea_orm::Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT definition.project_id FROM evaluation_definition_versions versioned \
         JOIN evaluation_definitions definition ON definition.id = versioned.definition_id WHERE versioned.id = $1",
        [version_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(row.try_get_by("project_id")?)),
        None => Ok(None),
    }
}

/// Parses `evaluation_runs.lifecycle_status` once at the row boundary. The column's CHECK
/// constraint admits only the values `EvaluationRunStatus` names, so an unrecognized value is
/// schema drift and panics, as the GraphQL-layer parse of the same string did before this type
/// existed.
pub fn run_status(value: String) -> EvaluationRunStatus {
    value.parse().unwrap_or_else(|error| panic!("{error}"))
}
