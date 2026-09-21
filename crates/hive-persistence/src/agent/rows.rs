//! The row-level layer the four draft commands and the computed fields share: the draft document
//! as text, the locked single-row reads and writes over `agents`, `agent_drafts` and
//! `agent_versions`, and the two audit writers.
//!
//! A version's content digest is taken over the draft document as Postgres writes a `jsonb` value
//! as text. Every version published so far was digested that way, and republishing an unchanged
//! draft finds its version by that digest, so `stored_document_text` reads the text with a `CAST`
//! rather than re-serializing it here.

use crate::audit::context::request_metadata;
use crate::capability;
use crate::capability::queries::{for_update_of, organization_memberships_of_project};
pub(super) use crate::configuration::rows::catalog_release;
use crate::configuration::rows::resolved;
use crate::entity::enums::{
    AgentAuthoringAuditAction, AgentDraftAuditAction, DraftValidationStatus, EvaluationTargetKind,
};
use crate::entity::{
    agent_authoring_audit_events, agent_draft_audit_events, agent_drafts, agent_versions, agents,
    environment_definition_versions, evaluation_target_projections, organization_memberships,
    projects,
};
use crate::guard;
use hive_application::agent::canonical_document;
use hive_application::agent::AgentDraftDiagnostic;
use sea_orm::sea_query::{Expr, ExprTrait, IntoTableRef, OnConflict};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, JoinType, NotSet, QueryFilter,
    QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A validation diagnostic as `agent_drafts.validation_diagnostics` stores it.
#[derive(Serialize, Deserialize)]
struct StoredDiagnostic {
    code: String,
    severity: String,
    message: String,
    path: Vec<String>,
}

fn diagnostics_json(diagnostics: &[AgentDraftDiagnostic]) -> serde_json::Value {
    let stored: Vec<StoredDiagnostic> = diagnostics
        .iter()
        .map(|value| StoredDiagnostic {
            code: value.code.clone(),
            severity: value.severity.clone(),
            message: value.message.clone(),
            path: value.path.clone(),
        })
        .collect();
    serde_json::to_value(stored).expect("diagnostics always serialize")
}

/// A stored JSON document as the text the application's document rules read.
pub(super) fn document_text(document: &serde_json::Value) -> String {
    serde_json::to_string(document).expect("a stored JSON document always serializes")
}

fn document_value(document: &str) -> serde_json::Value {
    serde_json::from_str(document).expect("a canonical document is always valid JSON")
}

/// The agent, when it is in `project` and `principal` is an active member of the project's
/// organization. With `lock`, the agent and membership rows are locked `FOR UPDATE`.
pub(super) async fn visible_agent(
    db: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
    agent: Uuid,
    lock: bool,
) -> Result<Option<agents::Model>, DbErr> {
    let select = agents::Entity::find()
        .inner_join(projects::Entity)
        .join(JoinType::InnerJoin, organization_memberships_of_project())
        .filter(projects::Column::Id.eq(project))
        .filter(agents::Column::Id.eq(agent))
        .filter(organization_memberships::Column::PrincipalId.eq(principal))
        .filter(
            Expr::col(organization_memberships::Column::StartedAt.as_column_ref())
                .lte(Expr::current_timestamp()),
        )
        .filter(organization_memberships::Column::EndedAt.is_null());
    let select = if lock {
        for_update_of(
            select,
            [
                agents::Entity.into_table_ref(),
                organization_memberships::Entity.into_table_ref(),
            ],
        )
    } else {
        select
    };
    // Every matching row is fetched, so a locking select locks every membership it matches.
    Ok(select.all(db).await?.into_iter().next())
}

/// The draft every agent starts from, as a row that is not stored yet.
pub(super) fn default_draft(agent: &agents::Model) -> agent_drafts::Model {
    agent_drafts::Model {
        document: document_value(&canonical_document::default_document(&agent.display_name)),
        revision: 1,
        validation_status: DraftValidationStatus::NotValidated,
        validation_diagnostics: serde_json::Value::Array(Vec::new()),
        validated_at: None,
        updated_at: chrono::Utc::now().into(),
        agent_id: agent.id,
    }
}

/// Stores the agent's default draft unless it already has one.
pub(super) async fn ensure_draft(
    db: &impl ConnectionTrait,
    agent: &agents::Model,
) -> Result<(), DbErr> {
    let draft = agent_drafts::ActiveModel {
        agent_id: Set(agent.id),
        document: Set(default_draft(agent).document),
        ..Default::default()
    };
    agent_drafts::Entity::insert(draft)
        .on_conflict(
            OnConflict::column(agent_drafts::Column::AgentId)
                .do_nothing()
                .to_owned(),
        )
        .try_insert()
        .exec_without_returning(db)
        .await?;
    Ok(())
}

/// The agent's draft, locked `FOR UPDATE`. `ensure_draft` ran first, so it exists.
pub(super) async fn locked_draft(
    db: &impl ConnectionTrait,
    agent: Uuid,
) -> Result<agent_drafts::Model, DbErr> {
    agent_drafts::Entity::find_by_id(agent)
        .lock_exclusive()
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no draft for agent {agent}")))
}

/// The stored draft document as Postgres writes it as text; `None` when no draft is stored.
pub(super) async fn stored_document_text(
    db: &impl ConnectionTrait,
    agent: Uuid,
) -> Result<Option<String>, DbErr> {
    agent_drafts::Entity::find_by_id(agent)
        .select_only()
        .expr_as(
            Expr::col(agent_drafts::Column::Document.as_column_ref()).cast_as("text"),
            "document_text",
        )
        .into_tuple::<String>()
        .one(db)
        .await
}

/// The digest of the agent's stored draft document.
pub(super) async fn stored_digest(db: &impl ConnectionTrait, agent: Uuid) -> Result<String, DbErr> {
    let text = stored_document_text(db, agent)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no draft for agent {agent}")))?;
    Ok(canonical_document::digest(&text))
}

/// Replaces the document of a draft still at `expected_revision`; `false` when it moved on.
pub(super) async fn update_document(
    db: &impl ConnectionTrait,
    agent: Uuid,
    expected_revision: i64,
    document: &str,
) -> Result<bool, DbErr> {
    guard::bump(
        db,
        agent_drafts::Entity::update_many()
            .col_expr(
                agent_drafts::Column::Document,
                Expr::value(document_value(document)),
            )
            .col_expr(
                agent_drafts::Column::ValidationStatus,
                Expr::value(DraftValidationStatus::NotValidated.to_value()),
            )
            .col_expr(
                agent_drafts::Column::ValidationDiagnostics,
                Expr::value(serde_json::Value::Array(Vec::new())),
            )
            .col_expr(
                agent_drafts::Column::ValidatedAt,
                Expr::value(Option::<chrono::DateTime<chrono::FixedOffset>>::None),
            )
            .col_expr(agent_drafts::Column::UpdatedAt, Expr::current_timestamp())
            .filter(agent_drafts::Column::AgentId.eq(agent)),
        agent_drafts::Column::Revision,
        expected_revision,
    )
    .await
}

/// The document's own diagnostics, plus one when a dependency does not resolve to an exact local
/// catalog or project version.
pub(super) async fn diagnostics(
    db: &impl ConnectionTrait,
    project: Uuid,
    document: &str,
) -> Result<Vec<AgentDraftDiagnostic>, DbErr> {
    let mut values = canonical_document::validate(document);
    let malformed = values.iter().any(|value| {
        value.code == "DEPENDENCIES_MALFORMED" || value.code == "DEPENDENCY_REFERENCE_INVALID"
    });
    if !malformed {
        let dependencies = canonical_document::dependencies(document);
        if !resolved(db, project, &dependencies).await? {
            values.push(AgentDraftDiagnostic {
                code: "DEPENDENCY_UNRESOLVED".to_string(),
                severity: "ERROR".to_string(),
                message:
                    "Every dependency must resolve to an exact local catalog or project version."
                        .to_string(),
                path: vec!["review".to_string(), "dependencies".to_string()],
            });
        }
    }
    Ok(values)
}

/// Records the validation of a draft still at `expected_revision`; `false` when it moved on.
pub(super) async fn validate_document(
    db: &impl ConnectionTrait,
    project: Uuid,
    agent: Uuid,
    expected_revision: i64,
    document: &str,
) -> Result<bool, DbErr> {
    let computed = diagnostics(db, project, document).await?;
    let valid = computed.iter().all(|value| value.severity != "ERROR");
    let status = if valid {
        DraftValidationStatus::Valid
    } else {
        DraftValidationStatus::Invalid
    };
    guard::bump(
        db,
        agent_drafts::Entity::update_many()
            .col_expr(
                agent_drafts::Column::ValidationStatus,
                Expr::value(status.to_value()),
            )
            .col_expr(
                agent_drafts::Column::ValidationDiagnostics,
                Expr::value(diagnostics_json(&computed)),
            )
            .col_expr(agent_drafts::Column::ValidatedAt, Expr::current_timestamp())
            .col_expr(agent_drafts::Column::UpdatedAt, Expr::current_timestamp())
            .filter(agent_drafts::Column::AgentId.eq(agent)),
        agent_drafts::Column::Revision,
        expected_revision,
    )
    .await
}

/// The agent's highest published version number.
pub(super) async fn latest_version_number(
    db: &impl ConnectionTrait,
    agent: Uuid,
) -> Result<Option<i64>, DbErr> {
    Ok(agent_versions::Entity::find()
        .filter(agent_versions::Column::AgentId.eq(agent))
        .select_only()
        .column_as(agent_versions::Column::VersionNumber.max(), "latest")
        .into_tuple::<Option<i64>>()
        .one(db)
        .await?
        .flatten())
}

/// The agent's newest published version.
pub(super) async fn latest_version(
    db: &impl ConnectionTrait,
    agent: Uuid,
) -> Result<Option<agent_versions::Model>, DbErr> {
    use sea_orm::QueryOrder;
    agent_versions::Entity::find()
        .filter(agent_versions::Column::AgentId.eq(agent))
        .order_by_desc(agent_versions::Column::VersionNumber)
        .one(db)
        .await
}

pub(super) async fn version_for_digest(
    db: &impl ConnectionTrait,
    agent: Uuid,
    digest: &str,
) -> Result<Option<agent_versions::Model>, DbErr> {
    agent_versions::Entity::find()
        .filter(agent_versions::Column::AgentId.eq(agent))
        .filter(agent_versions::Column::ContentDigest.eq(digest))
        .one(db)
        .await
}

pub(super) async fn legacy_audit(
    db: &impl ConnectionTrait,
    principal: Uuid,
    action: AgentDraftAuditAction,
    draft: &agent_drafts::Model,
    content_digest: String,
) -> Result<(), DbErr> {
    let metadata = request_metadata();
    let event = agent_draft_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        agent_id: Set(draft.agent_id),
        principal_id: Set(principal),
        action: Set(action),
        revision: Set(draft.revision),
        content_digest: Set(content_digest),
        occurred_at: NotSet,
        request_id: Set(metadata.request_id),
        correlation_id: Set(metadata.correlation_id),
        graphql_operation: Set(metadata.graphql_operation),
        source_ip: Set(metadata.source_ip),
        user_agent: Set(metadata.user_agent),
    };
    agent_draft_audit_events::Entity::insert(event)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

/// One agent authoring audit row. The three ids are named rather than positional: they are
/// adjacent `Uuid`s, and a transposed pair would compile and file the row against the wrong
/// project, agent or actor.
pub(super) struct AuthoringEvent {
    pub project: Uuid,
    pub agent: Uuid,
    pub principal: Uuid,
    pub action: AgentAuthoringAuditAction,
    pub revision: i64,
    pub version_id: Option<Uuid>,
    pub content_digest: String,
}

pub(super) async fn authoring_audit(
    db: &impl ConnectionTrait,
    entry: AuthoringEvent,
) -> Result<(), DbErr> {
    let organization_id = capability::queries::project_organization(db, entry.project, false)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("no project with id {}", entry.project)))?;
    let metadata = request_metadata();
    let event = agent_authoring_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        agent_id: Set(entry.agent),
        project_id: Set(entry.project),
        actor_principal_id: Set(entry.principal),
        action: Set(entry.action),
        revision: Set(Some(entry.revision)),
        version_id: Set(entry.version_id),
        content_digest: Set(entry.content_digest),
        occurred_at: NotSet,
        request_id: Set(metadata.request_id),
        correlation_id: Set(metadata.correlation_id),
        graphql_operation: Set(metadata.graphql_operation),
        source_ip: Set(metadata.source_ip),
        user_agent: Set(metadata.user_agent),
        organization_id: Set(Some(organization_id)),
    };
    agent_authoring_audit_events::Entity::insert(event)
        .exec_without_returning(db)
        .await?;
    Ok(())
}

/// `AGENT_DRAFT.CREATE` and `AGENT_DRAFT.PUBLISH` are the same rule: an active project and an
/// editor role in it.
pub(super) async fn can_create_or_publish(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
) -> Result<bool, DbErr> {
    Ok(
        capability::queries::active_project(txn, project, true).await?
            && capability::queries::legacy_or_developer(txn, principal, project, true).await?,
    )
}

/// Makes a published version an evaluation target in every environment of its catalog release.
/// Aurora DSQL has no triggers, so publication maintains the projection itself.
pub(super) async fn project_agent_version_target(
    db: &impl ConnectionTrait,
    agent: &agents::Model,
    version: &agent_versions::Model,
) -> Result<(), DbErr> {
    let environments = environment_definition_versions::Entity::find()
        .filter(
            environment_definition_versions::Column::CatalogReleaseId
                .eq(version.catalog_release_id.clone()),
        )
        .all(db)
        .await?;
    if environments.is_empty() {
        return Ok(());
    }
    let targets =
        environments
            .into_iter()
            .map(|environment| evaluation_target_projections::ActiveModel {
                project_id: Set(agent.project_id),
                target_kind: Set(EvaluationTargetKind::AgentVersion),
                target_id: Set(version.id),
                agent_version_id: Set(version.id),
                environment_definition_version_id: Set(environment.id),
                logical_environment_class: Set(environment.logical_environment_class),
                display_name: Set(agent.display_name.clone()),
            });
    evaluation_target_projections::Entity::insert_many(targets)
        .on_conflict(
            OnConflict::columns([
                evaluation_target_projections::Column::TargetKind,
                evaluation_target_projections::Column::TargetId,
                evaluation_target_projections::Column::EnvironmentDefinitionVersionId,
            ])
            .update_columns([
                evaluation_target_projections::Column::ProjectId,
                evaluation_target_projections::Column::AgentVersionId,
                evaluation_target_projections::Column::LogicalEnvironmentClass,
                evaluation_target_projections::Column::DisplayName,
            ])
            .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod draft_row_tests {
    use super::{default_draft, diagnostics_json, document_text, document_value};
    use crate::entity::agents;
    use crate::entity::enums::{AgentLifecycleStatus, DraftValidationStatus};
    use hive_application::agent::AgentDraftDiagnostic;
    use serde_json::json;
    use uuid::{uuid, Uuid};

    const AGENT: Uuid = uuid!("11111111-1111-1111-1111-111111111111");

    fn agent(display_name: &str) -> agents::Model {
        agents::Model {
            project_id: Uuid::nil(),
            slug: "feedback-triage".to_string(),
            display_name: display_name.to_string(),
            lifecycle_status: AgentLifecycleStatus::Active,
            id: AGENT,
        }
    }

    /// The stored text is the parsed value re-serialized, so key order is the serializer's and
    /// there is no whitespace. The content digest is taken over exactly this.
    #[test]
    fn a_document_renders_as_compact_json() {
        assert_eq!(
            document_text(&json!({ "b": 1, "a": [2, 3] })),
            r#"{"a":[2,3],"b":1}"#
        );
        assert_eq!(document_text(&json!({})), "{}");
    }

    #[test]
    fn a_document_round_trips_through_its_stored_text() {
        let document = json!({ "general": { "displayName": "Feedback Triage" } });
        assert_eq!(document_value(&document_text(&document)), document);
    }

    #[test]
    fn diagnostics_store_exactly_four_keys_each() {
        assert_eq!(
            diagnostics_json(&[AgentDraftDiagnostic {
                code: "MODEL_MISSING".to_string(),
                severity: "ERROR".to_string(),
                message: "A model is required.".to_string(),
                path: vec!["model".to_string()],
            }]),
            json!([{
                "code": "MODEL_MISSING",
                "severity": "ERROR",
                "message": "A model is required.",
                "path": ["model"],
            }])
        );
        assert_eq!(diagnostics_json(&[]), json!([]));
    }

    /// The default draft is answered to a reader before any row exists, so it must look like the
    /// row a first save would write: revision 1, unvalidated, no diagnostics.
    #[test]
    fn the_default_draft_is_revision_one_and_not_yet_validated() {
        let draft = default_draft(&agent("Feedback Triage"));
        assert_eq!(draft.agent_id, AGENT);
        assert_eq!(draft.revision, 1);
        assert_eq!(draft.validation_status, DraftValidationStatus::NotValidated);
        assert_eq!(draft.validation_diagnostics, json!([]));
        assert_eq!(draft.validated_at, None);
    }

    /// The agent's own display name is carried into the document, so the console's first render
    /// names the agent rather than a placeholder.
    #[test]
    fn the_default_draft_carries_the_agents_display_name() {
        let draft = default_draft(&agent("Feedback Triage"));
        assert_eq!(
            draft.document["general"]["displayName"],
            json!("Feedback Triage")
        );
        for section in [
            "general",
            "instructions",
            "harness",
            "model",
            "tools",
            "skills",
        ] {
            assert!(
                draft.document.get(section).is_some(),
                "the default document has no `{section}` section"
            );
        }
    }
}
