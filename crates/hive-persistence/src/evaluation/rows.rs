//! Row-mapping helpers shared by `queries`/`mutations`/`worker`.

use chrono::{DateTime, Utc};
use hive_application::evaluation::document::EvaluationDiagnostic;
use hive_application::evaluation::EvaluationRunStatus;
use hive_application::evaluation::{
    EvaluationArtifactMetadata, EvaluationAuditEvent, EvaluationCaseRun, EvaluationDefinitionDraft,
    EvaluationDefinitionVersion, EvaluationMetricResult, EvaluationRun, EvaluationTarget,
    EvaluationTargetSnapshot,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
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

pub fn version_row(row: &sqlx::postgres::PgRow, prefix: &str) -> EvaluationDefinitionVersion {
    EvaluationDefinitionVersion {
        id: row.get(format!("{prefix}id").as_str()),
        definition_id: row.get(format!("{prefix}definition_id").as_str()),
        number: row.get(format!("{prefix}version_number").as_str()),
        canonical_document: row.get(format!("{prefix}canonical_document").as_str()),
        content_digest: row.get(format!("{prefix}content_digest").as_str()),
        based_on_version_id: row.get(format!("{prefix}based_on_version_id").as_str()),
        published_by: row.get(format!("{prefix}published_by").as_str()),
        published_at: row.get(format!("{prefix}published_at").as_str()),
    }
}

pub fn case_row(row: &sqlx::postgres::PgRow) -> EvaluationCaseRun {
    EvaluationCaseRun {
        id: row.get(0),
        key: row.get(1),
        ordinal: row.get(2),
        lifecycle_status: row.get(3),
        passed: row.get(4),
        failure_code: row.get(5),
        completed_at: row.get(6),
    }
}

/// The query binds `value`/`threshold` cast to `float8` (not `numeric`) so this can decode them as
/// plain `f64`, avoiding a dependency on sqlx's `bigdecimal` feature — matching how
/// `EvaluationScoringPolicy`'s already-ported Rust port (`scoring.rs`) also computes rates as `f64`.
pub fn metric_row(row: &sqlx::postgres::PgRow) -> EvaluationMetricResult {
    EvaluationMetricResult {
        id: row.get(0),
        code: row.get(1),
        value: row.get(2),
        threshold: row.get(3),
        passed: row.get(4),
    }
}

pub fn artifact_row(row: &sqlx::postgres::PgRow) -> EvaluationArtifactMetadata {
    EvaluationArtifactMetadata {
        id: row.get(0),
        kind: row.get(1),
        content_digest: row.get(2),
        media_type: row.get(3),
        byte_length: row.get(4),
    }
}

pub fn audit_row(row: &sqlx::postgres::PgRow) -> EvaluationAuditEvent {
    EvaluationAuditEvent {
        id: row.get(0),
        action: row.get(1),
        occurred_at: row.get(2),
        summary: row.get::<Option<String>, _>(3).unwrap_or_default(),
    }
}

pub fn target_row(row: &sqlx::postgres::PgRow) -> EvaluationTarget {
    EvaluationTarget {
        kind: row.get(0),
        id: row.get(1),
        agent_version_id: row.get(2),
        environment_definition_version_id: row.get(3),
        logical_environment_class: row.get(4),
        display_name: row.get(5),
    }
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

pub fn target_from_row(row: &sqlx::postgres::PgRow) -> Target {
    Target {
        agent_version_id: row.get(0),
        deployment_id: row.get(1),
        environment_definition_version_id: row.get(2),
        environment_class: row.get(3),
        agent_digest: row.get(4),
        target_digest: row.get(5),
        plan_digest: row.get(6),
        package_digest: row.get(7),
        binding_digest: row.get(8),
        catalog_release_id: row.get(9),
        catalog_release_digest: row.get(10),
        environment_digest: row.get(11),
    }
}

pub fn snapshot_row(row: &sqlx::postgres::PgRow) -> EvaluationTargetSnapshot {
    EvaluationTargetSnapshot {
        agent_version_id: row.get(0),
        deployment_id: row.get(1),
        environment_definition_version_id: row.get(2),
        logical_environment_class: row.get(3),
        agent_content_digest: row.get(4),
        target_digest: row.get(5),
        plan_digest: row.get(6),
        package_digest: row.get(7),
        binding_digest: row.get(8),
        catalog_release_id: row.get(9),
        catalog_release_digest: row.get(10),
        environment_content_digest: row.get(11),
    }
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

pub fn raw_run_row(row: &sqlx::postgres::PgRow) -> RawRun {
    RawRun {
        id: row.get(0),
        project_id: row.get(1),
        definition_version_id: row.get(2),
        target_kind: row.get(3),
        target_id: row.get(4),
        environment_id: row.get(5),
        source_run_id: row.get(6),
        status: run_status(row.get(7)),
        generation: row.get(8),
        outcome_category: row.get(9),
        outcome_code: row.get(10),
        created_at: row.get(11),
        started_at: row.get(12),
        completed_at: row.get(13),
    }
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
    conn: &mut PgConnection,
    definition_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT project_id FROM evaluation_definitions WHERE id = $1")
            .bind(definition_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.map(|row| row.0))
}

pub async fn version_project(
    conn: &mut PgConnection,
    version_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT definition.project_id FROM evaluation_definition_versions versioned \
         JOIN evaluation_definitions definition ON definition.id = versioned.definition_id WHERE versioned.id = $1",
    )
    .bind(version_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| row.0))
}

/// Parses `evaluation_runs.lifecycle_status` once at the row boundary. The column's CHECK
/// constraint admits only the values `EvaluationRunStatus` names, so an unrecognized value is
/// schema drift and panics, as the GraphQL-layer parse of the same string did before this type
/// existed.
pub fn run_status(value: String) -> EvaluationRunStatus {
    value.parse().unwrap_or_else(|error| panic!("{error}"))
}
