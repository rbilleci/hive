//! Computed fields of the generated evaluation objects.
//! Each is derived from the row it is on, plus rows loaded through SeaORM. `hive-api` attaches
//! them to the generated objects.
//!
//! - `EvaluationDefinitions.canAuthor` / `canPublish`: answered by the capability evaluator for
//!   the requesting principal.
//! - `EvaluationDefinitions.draft`: the definition's one draft row; `latestVersion`: the newest
//!   published version, by version number.
//! - `EvaluationDefinitionDrafts.canonicalDocument` / `diagnostics` and
//!   `EvaluationDefinitionVersions.canonicalDocument`: the stored content, withheld (empty)
//!   without `EVALUATION_DEFINITION.AUTHOR`. The columns themselves are not part of the generated
//!   API, so no filter or order reaches them.
//! - `EvaluationDefinitionVersions.comparison(rightVersionId)`: this version and another version
//!   of the same definition, side by side.
//! - `EvaluationRuns.durationMillis` / `failureSummary` / `deploymentEvidenceDisposition` /
//!   `target`: the derived run facts and the frozen target snapshot.
//! - `EvaluationRuns.terminal` / `canCancel` / `canRerun`: the run state machine taken with the
//!   requesting principal's capabilities at the project, so a surface renders an action from these
//!   instead of restating the rules.
//! - `EvaluationAuditEvents.summary`: the `summary` fact of the event's retained fact document.
//! - `Projects.compatibleEvaluationTargets(definitionVersionId)`: the candidate targets a
//!   published definition version may run against. Empty without `EVALUATION_RUN.RUN` here. (The
//!   field itself is declared on `projects::Model` in `crate::console`, because a model takes one
//!   `#[CustomFields]` block.)

#![allow(non_snake_case)] // a computed field is named after its method

use crate::capability::{self, Scope};
use crate::console::requester;
use crate::entity::{
    deployment_evidence_snapshots, evaluation_audit_events, evaluation_definition_drafts,
    evaluation_definition_versions, evaluation_definitions, evaluation_runs,
    evaluation_target_projections, evaluation_target_snapshots, projects,
};
use hive_application::evaluation::document;
use hive_application::evaluation::outcome;
use hive_application::evaluation::EvaluationRunStatus;
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, Order, QueryFilter, QueryOrder,
};
// `#[CustomFields]` and `CustomOutputType` expand to paths that start with `async_graphql::`.
use seaography::async_graphql::{self, Context};
use seaography::{CustomFields, CustomOutputType};
use serde::Deserialize;
use uuid::Uuid;

/// One stored validation result of an evaluation draft. `path` locates the document section.
#[derive(CustomOutputType, Clone, Deserialize)]
pub struct EvaluationDraftDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub path: Vec<String>,
}

/// Two published versions of one definition, side by side.
#[derive(CustomOutputType, Clone)]
pub struct EvaluationDefinitionVersionComparison {
    /// The version the field was asked on.
    pub left: evaluation_definition_versions::Model,
    /// The version named by `rightVersionId`.
    pub right: evaluation_definition_versions::Model,
}

/// The stored canonical document as text. A definition document is written as `jsonb`, so this is
/// the parsed value re-serialized, not Postgres's own `::text` rendering of it.
fn document_text(value: &serde_json::Value) -> String {
    serde_json::to_string(value).expect("a stored document always serializes")
}

/// Whether the requesting principal holds `code` at `project_id`.
async fn held(ctx: &Context<'_>, project_id: Uuid, code: &str) -> async_graphql::Result<bool> {
    let (principal_id, db) = requester(ctx)?;
    Ok(
        capability::has_capability(db, principal_id, code, Scope::Project(project_id), false)
            .await?,
    )
}

/// Whether the requesting principal may read the content of `definition_id`'s documents.
async fn may_read_content(ctx: &Context<'_>, definition_id: Uuid) -> async_graphql::Result<bool> {
    let (_, db) = requester(ctx)?;
    let Some(definition) = evaluation_definitions::Entity::find_by_id(definition_id)
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    held(
        ctx,
        definition.project_id,
        capability::EVALUATION_DEFINITION_AUTHOR,
    )
    .await
}

/// The newest published version of `definition_id`, by version number.
async fn latest_version(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
) -> Result<Option<evaluation_definition_versions::Model>, DbErr> {
    evaluation_definition_versions::Entity::find()
        .filter(evaluation_definition_versions::Column::DefinitionId.eq(definition_id))
        .order_by_desc(evaluation_definition_versions::Column::VersionNumber)
        .order_by_desc(evaluation_definition_versions::Column::Id)
        .one(db)
        .await
}

#[CustomFields]
impl evaluation_definitions::Model {
    /// Whether the requesting principal may edit and validate this definition's draft.
    pub async fn canAuthor(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        held(
            ctx,
            self.project_id,
            capability::EVALUATION_DEFINITION_AUTHOR,
        )
        .await
    }

    /// Whether the requesting principal may publish this definition's draft.
    pub async fn canPublish(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        held(
            ctx,
            self.project_id,
            capability::EVALUATION_DEFINITION_PUBLISH,
        )
        .await
    }

    /// The definition's draft: every definition has exactly one row from its creation on.
    pub async fn draft(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<evaluation_definition_drafts::Model> {
        let (_, db) = requester(ctx)?;
        evaluation_definition_drafts::Entity::find_by_id(self.id)
            .one(db)
            .await?
            .ok_or_else(|| async_graphql::Error::new("This evaluation definition is unavailable."))
    }

    /// The newest published version, or `null` before the first publication.
    pub async fn latestVersion(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<evaluation_definition_versions::Model>> {
        let (_, db) = requester(ctx)?;
        Ok(latest_version(db, self.id).await?)
    }
}

#[CustomFields]
impl evaluation_definition_drafts::Model {
    /// The canonical draft document; empty without `EVALUATION_DEFINITION.AUTHOR`.
    pub async fn canonicalDocument(&self, ctx: &Context<'_>) -> async_graphql::Result<String> {
        Ok(match may_read_content(ctx, self.definition_id).await? {
            true => document_text(&self.canonical_document),
            false => String::new(),
        })
    }

    /// The stored validation diagnostics; empty without `EVALUATION_DEFINITION.AUTHOR`.
    pub async fn diagnostics(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<EvaluationDraftDiagnostic>> {
        if !may_read_content(ctx, self.definition_id).await? {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_value(self.diagnostics.clone())
            .expect("stored evaluation diagnostics are always valid JSON"))
    }
}

#[CustomFields]
impl evaluation_definition_versions::Model {
    /// The published canonical document; empty without `EVALUATION_DEFINITION.AUTHOR`.
    pub async fn canonicalDocument(&self, ctx: &Context<'_>) -> async_graphql::Result<String> {
        Ok(match may_read_content(ctx, self.definition_id).await? {
            true => document_text(&self.canonical_document),
            false => String::new(),
        })
    }

    /// This version next to another one. `null` when `rightVersionId` is not a published version
    /// of the same definition.
    pub async fn comparison(
        &self,
        ctx: &Context<'_>,
        rightVersionId: String,
    ) -> async_graphql::Result<Option<EvaluationDefinitionVersionComparison>> {
        let (_, db) = requester(ctx)?;
        let Ok(right_id) = Uuid::parse_str(&rightVersionId) else {
            return Ok(None);
        };
        let right = evaluation_definition_versions::Entity::find_by_id(right_id)
            .filter(evaluation_definition_versions::Column::DefinitionId.eq(self.definition_id))
            .one(db)
            .await?;
        Ok(right.map(|right| EvaluationDefinitionVersionComparison {
            left: self.clone(),
            right,
        }))
    }
}

#[CustomFields]
impl evaluation_runs::Model {
    /// How long the run took, once it both started and completed.
    pub async fn durationMillis(&self, _ctx: &Context<'_>) -> async_graphql::Result<Option<i64>> {
        Ok(outcome::duration_millis(
            self.started_at.map(Into::into),
            self.completed_at.map(Into::into),
        ))
    }

    /// The redacted terminal summary of a run that did not pass.
    pub async fn failureSummary(
        &self,
        _ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<String>> {
        Ok(outcome::failure_summary(
            self.lifecycle_status.into(),
            self.outcome_category.map(Into::into),
        ))
    }

    /// `APPENDED` once the run's evidence reached a deployment, `NOT_PENDING` for a deployment
    /// target whose evidence never did, `NOT_A_DEPLOYMENT` otherwise.
    pub async fn deploymentEvidenceDisposition(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<String> {
        let (_, db) = requester(ctx)?;
        let appended = deployment_evidence_snapshots::Entity::find()
            .filter(deployment_evidence_snapshots::Column::SourceEvaluationRunId.eq(self.id))
            .one(db)
            .await?
            .is_some();
        if appended {
            return Ok("APPENDED".to_string());
        }
        let deployment = evaluation_target_snapshots::Entity::find_by_id(self.id)
            .one(db)
            .await?
            .and_then(|snapshot| snapshot.deployment_id);
        Ok(match deployment {
            Some(_) => "NOT_PENDING".to_string(),
            None => "NOT_A_DEPLOYMENT".to_string(),
        })
    }

    /// The target this run froze when it was queued.
    pub async fn target(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<evaluation_target_snapshots::Model>> {
        let (_, db) = requester(ctx)?;
        Ok(evaluation_target_snapshots::Entity::find_by_id(self.id)
            .one(db)
            .await?)
    }

    /// Whether the run has finished. A surface that polls the run stops when this is true.
    pub async fn terminal(&self, _ctx: &Context<'_>) -> async_graphql::Result<bool> {
        Ok(EvaluationRunStatus::from(self.lifecycle_status).is_terminal())
    }

    /// Whether the requesting principal may cancel this run now. A rendering hint: the command
    /// reauthorizes and rechecks the lifecycle under its own locks.
    pub async fn canCancel(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        Ok(
            EvaluationRunStatus::from(self.lifecycle_status).may_cancel()
                && held(ctx, self.project_id, capability::EVALUATION_RUN_CANCEL).await?,
        )
    }

    /// Whether the requesting principal may start a new run from this finished one.
    pub async fn canRerun(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        Ok(
            EvaluationRunStatus::from(self.lifecycle_status).is_terminal()
                && held(ctx, self.project_id, capability::EVALUATION_RUN_RERUN).await?,
        )
    }
}

#[CustomFields]
impl evaluation_audit_events::Model {
    /// The event's own one-line summary fact; empty when the event records none.
    pub async fn summary(&self, _ctx: &Context<'_>) -> async_graphql::Result<String> {
        Ok(self
            .facts
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string())
    }
}

/// The candidate targets `definition_version_id` may run against in `project`, ordered as the
/// deleted `evaluationTargets` query ordered them. Empty unless the requesting principal holds
/// `EVALUATION_RUN.RUN` here and the version belongs to this project.
pub async fn compatible_targets(
    ctx: &Context<'_>,
    project: &projects::Model,
    definition_version_id: &str,
) -> async_graphql::Result<Vec<evaluation_target_projections::Model>> {
    let (_, db) = requester(ctx)?;
    if !held(ctx, project.id, capability::EVALUATION_RUN_RUN).await? {
        return Ok(Vec::new());
    }
    let Ok(version_id) = Uuid::parse_str(definition_version_id) else {
        return Ok(Vec::new());
    };
    let Some(version) = evaluation_definition_versions::Entity::find_by_id(version_id)
        .one(db)
        .await?
    else {
        return Ok(Vec::new());
    };
    let Some(definition) = evaluation_definitions::Entity::find_by_id(version.definition_id)
        .one(db)
        .await?
    else {
        return Ok(Vec::new());
    };
    if definition.project_id != project.id {
        return Ok(Vec::new());
    }
    let document = document_text(&version.canonical_document);
    let kinds = document::target_kinds(&document);
    let classes = document::environment_classes(&document);
    let rows = evaluation_target_projections::Entity::find()
        .filter(evaluation_target_projections::Column::ProjectId.eq(project.id))
        .order_by(
            evaluation_target_projections::Column::TargetKind,
            Order::Asc,
        )
        .order_by(
            evaluation_target_projections::Column::DisplayName,
            Order::Asc,
        )
        .order_by(evaluation_target_projections::Column::TargetId, Order::Asc)
        .order_by(
            evaluation_target_projections::Column::EnvironmentDefinitionVersionId,
            Order::Asc,
        )
        .all(db)
        .await?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            kinds.contains(&row.target_kind.to_value())
                && classes.contains(&row.logical_environment_class.to_value())
        })
        .collect())
}
