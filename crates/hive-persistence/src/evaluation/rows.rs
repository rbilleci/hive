//! The small shared types and parsers the evaluation commands and the outbox worker pass around,
//! and the two owning-project lookups they start from. Everything here is SeaORM entities; the
//! row-by-name mappers went with the hand-written statements.

use crate::entity::enums::LogicalEnvironmentClass;
use crate::entity::{evaluation_definition_versions, evaluation_definitions, evaluation_runs};
use hive_application::evaluation::document::EvaluationDiagnostic;
use hive_application::evaluation::EvaluationRunStatus;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, QuerySelect, RelationTrait};
use serde::Serialize;
use uuid::Uuid;

#[derive(Serialize)]
struct RawDiagnostic {
    code: String,
    severity: String,
    message: String,
    path: Vec<String>,
}

/// The diagnostics as the `jsonb` column holds them.
pub fn diagnostics_json(diagnostics: &[EvaluationDiagnostic]) -> serde_json::Value {
    let raw: Vec<RawDiagnostic> = diagnostics
        .iter()
        .map(|value| RawDiagnostic {
            code: value.code.clone(),
            severity: value.severity.clone(),
            message: value.message.clone(),
            path: value.path.clone(),
        })
        .collect();
    serde_json::to_value(raw).expect("diagnostics always serialize")
}

/// The stored canonical document as text. The column is `jsonb`, so this is the parsed value
/// re-serialized, exactly as the generated API's computed `canonicalDocument` renders it.
pub fn document_text(value: &serde_json::Value) -> String {
    serde_json::to_string(value).expect("a stored document always serializes")
}

/// The resolved facts one of `queries::resolve_target`'s two branches, or
/// `queries::target_from_snapshot`, supplies before `run`/`rerun` insert
/// `evaluation_target_snapshots`. `deployment_id` and the four policy digests are `None` for an
/// `AGENT_VERSION` target: only a `DEPLOYMENT` target's policy snapshot supplies them.
pub struct Target {
    pub agent_version_id: Uuid,
    pub deployment_id: Option<Uuid>,
    pub environment_definition_version_id: Uuid,
    pub environment_class: LogicalEnvironmentClass,
    pub agent_digest: String,
    pub target_digest: Option<String>,
    pub plan_digest: Option<String>,
    pub package_digest: Option<String>,
    pub binding_digest: Option<String>,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub environment_digest: String,
}

/// One claimed `evaluation_outbox_events` row.
#[derive(Clone)]
pub struct Event {
    pub id: Uuid,
    pub run_id: Uuid,
    pub event_type: String,
    pub case_run_id: Option<Uuid>,
    pub attempts: i32,
}

pub fn run_status(run: &evaluation_runs::Model) -> EvaluationRunStatus {
    run.lifecycle_status.into()
}

pub async fn definition_project(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
) -> Result<Option<Uuid>, DbErr> {
    evaluation_definitions::Entity::find_by_id(definition_id)
        .select_only()
        .column(evaluation_definitions::Column::ProjectId)
        .into_tuple::<Uuid>()
        .one(db)
        .await
}

pub async fn version_project(
    db: &impl ConnectionTrait,
    version_id: Uuid,
) -> Result<Option<Uuid>, DbErr> {
    evaluation_definition_versions::Entity::find_by_id(version_id)
        .join(
            sea_orm::JoinType::InnerJoin,
            evaluation_definition_versions::Relation::EvaluationDefinitions.def(),
        )
        .select_only()
        .column(evaluation_definitions::Column::ProjectId)
        .into_tuple::<Uuid>()
        .one(db)
        .await
}
