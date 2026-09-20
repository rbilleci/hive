//! What the evaluation *commands* and the outbox worker still read: the definition and its draft
//! under lock, a published version, a raw run, its frozen target, and the small lookups
//! `mutations`/`worker` decide on.
//!
//! Every GraphQL read of the evaluation domain is a generated Seaography entity query now
//! (`docs/idiomatic-seaography-plan.md`, A2); the connection readers, their cursors and their row
//! mappers are gone with it. The statements below are the mutation half of the module and are
//! rewritten onto SeaORM with the eight commands, in their own slice.

use hive_application::evaluation::{
    EvaluationDefinition, EvaluationDefinitionVersion, EvaluationRun, EvaluationTargetSnapshot,
};
use sea_orm::{ConnectionTrait, DbErr, QueryResult, Statement};
use uuid::Uuid;

use super::rows::{
    draft_row, redacted_draft, redacted_version, run_from_raw, snapshot_row, target_from_row,
    version_row, RawRun, Target,
};
use crate::capability::tx;

pub(super) async fn can(
    db: &impl ConnectionTrait,
    principal: Uuid,
    capability: &str,
    project: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    tx::has_evaluation_capability(db, principal, capability, project, lock).await
}

fn definition_row(
    row: &QueryResult,
    can_author: bool,
    can_publish: bool,
) -> Result<EvaluationDefinition, DbErr> {
    let definition_id: Uuid = row.try_get_by("id")?;
    let draft = draft_row(
        definition_id,
        row.try_get_by("draft_document")?,
        row.try_get_by("draft_revision")?,
        row.try_get_by("draft_validation_status")?,
        row.try_get_by::<String, _>("draft_diagnostics")?.as_str(),
        row.try_get_by("draft_based_on_version_id")?,
        row.try_get_by("draft_updated_at")?,
    );
    let latest_id: Option<Uuid> = row.try_get_by("latest_id")?;
    let latest_version = match latest_id {
        Some(_) => Some(version_row(row, "latest_")?),
        None => None,
    };
    Ok(EvaluationDefinition {
        id: definition_id,
        project_id: row.try_get_by("project_id")?,
        slug: row.try_get_by("slug")?,
        lifecycle_status: row.try_get_by("lifecycle_status")?,
        draft: if can_author {
            draft
        } else {
            redacted_draft(&draft)
        },
        latest_version: latest_version.map(|value| {
            if can_author {
                value
            } else {
                redacted_version(&value)
            }
        }),
        can_author,
        can_publish,
        created_at: row.try_get_by("created_at")?,
    })
}

pub async fn definition(
    db: &impl ConnectionTrait,
    principal: Uuid,
    definition_id: Uuid,
    lock: bool,
) -> Result<Option<EvaluationDefinition>, DbErr> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let sql = format!("SELECT project_id FROM evaluation_definitions WHERE id = $1{suffix}");
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [definition_id.into()]);
    let Some(project_row) = db.query_one_raw(statement).await? else {
        return Ok(None);
    };
    let project: Uuid = project_row.try_get_by("project_id")?;
    if !can(db, principal, tx::EVALUATION_DEFINITION_VIEW, project, lock).await? {
        return Ok(None);
    }
    let can_author = can(
        db,
        principal,
        tx::EVALUATION_DEFINITION_AUTHOR,
        project,
        false,
    )
    .await?;
    let can_publish = can(
        db,
        principal,
        tx::EVALUATION_DEFINITION_PUBLISH,
        project,
        false,
    )
    .await?;
    let lock_suffix = if lock {
        " FOR UPDATE OF definition, draft"
    } else {
        ""
    };
    let sql = format!(
        "SELECT definition.id, definition.project_id, definition.slug, definition.lifecycle_status, definition.created_at, \
             draft.canonical_document::text AS draft_document, draft.revision AS draft_revision, draft.validation_status AS draft_validation_status, \
             draft.diagnostics::text AS draft_diagnostics, draft.based_on_version_id AS draft_based_on_version_id, draft.updated_at AS draft_updated_at, \
             latest.id AS latest_id, latest.definition_id AS latest_definition_id, latest.version_number AS latest_version_number, \
             latest.canonical_document::text AS latest_canonical_document, latest.content_digest AS latest_content_digest, \
             latest.based_on_version_id AS latest_based_on_version_id, latest.published_by AS latest_published_by, latest.published_at AS latest_published_at \
         FROM evaluation_definitions definition \
           JOIN evaluation_definition_drafts draft ON draft.definition_id = definition.id \
           LEFT JOIN LATERAL (SELECT id, definition_id, version_number, canonical_document, content_digest, based_on_version_id, published_by, published_at \
             FROM evaluation_definition_versions WHERE definition_id = definition.id ORDER BY version_number DESC, id DESC LIMIT 1) latest ON TRUE \
         WHERE definition.id = $1{lock_suffix}"
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [definition_id.into()]);
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(definition_row(&row, can_author, can_publish)?)),
        None => Ok(None),
    }
}

pub async fn draft(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
    lock: bool,
) -> Result<hive_application::evaluation::models::EvaluationDefinitionDraft, DbErr> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let sql = format!(
        "SELECT canonical_document::text AS canonical_document, revision, validation_status, diagnostics::text AS diagnostics, based_on_version_id, updated_at \
         FROM evaluation_definition_drafts WHERE definition_id = $1{suffix}"
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [definition_id.into()]);
    let row = db
        .query_one_raw(statement)
        .await?
        .expect("a definition always has exactly one draft row");
    Ok(draft_row(
        definition_id,
        row.try_get_by("canonical_document")?,
        row.try_get_by("revision")?,
        row.try_get_by("validation_status")?,
        row.try_get_by::<String, _>("diagnostics")?.as_str(),
        row.try_get_by("based_on_version_id")?,
        row.try_get_by("updated_at")?,
    ))
}

pub async fn version(
    db: &impl ConnectionTrait,
    version_id: Uuid,
) -> Result<Option<EvaluationDefinitionVersion>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, definition_id, version_number, canonical_document::text AS canonical_document, content_digest, \
             based_on_version_id, published_by, published_at \
         FROM evaluation_definition_versions WHERE id = $1",
        [version_id.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(version_row(&row, "")?)),
        None => Ok(None),
    }
}

pub async fn raw_run(
    db: &impl ConnectionTrait,
    id: Uuid,
    lock: bool,
) -> Result<Option<RawRun>, DbErr> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let sql = format!(
        "SELECT id, project_id, definition_version_id, target_kind, target_id, environment_definition_version_id, \
             source_run_id, lifecycle_status, generation, outcome_category, outcome_code, created_at, started_at, completed_at \
         FROM evaluation_runs WHERE id = $1{suffix}"
    );
    let statement = Statement::from_sql_and_values(db.get_database_backend(), &sql, [id.into()]);
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(super::rows::raw_run_row(&row)?)),
        None => Ok(None),
    }
}

pub async fn snapshot(
    db: &impl ConnectionTrait,
    run: Uuid,
) -> Result<Option<EvaluationTargetSnapshot>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT agent_version_id,deployment_id,environment_definition_version_id,logical_environment_class,agent_content_digest,target_digest,plan_digest,package_digest,binding_digest,catalog_release_id,catalog_release_digest,environment_content_digest \
         FROM evaluation_target_snapshots WHERE run_id = $1",
        [run.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(snapshot_row(&row)?)),
        None => Ok(None),
    }
}

pub async fn evidence_disposition(db: &impl ConnectionTrait, run: Uuid) -> Result<String, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT CASE WHEN EXISTS (SELECT 1 FROM deployment_evidence_snapshots WHERE source_evaluation_run_id = $1) THEN 'APPENDED' \
             WHEN EXISTS (SELECT 1 FROM evaluation_target_snapshots WHERE run_id = $2 AND deployment_id IS NOT NULL) THEN 'NOT_PENDING' \
             ELSE 'NOT_A_DEPLOYMENT' END AS disposition",
        [run.into(), run.into()],
    );
    db.query_one_raw(statement)
        .await?
        .expect("the CASE expression always returns exactly one row")
        .try_get_by("disposition")
}

pub async fn run(
    db: &impl ConnectionTrait,
    principal: Uuid,
    run_id: Uuid,
    lock: bool,
) -> Result<Option<EvaluationRun>, DbErr> {
    let Some(raw) = raw_run(db, run_id, lock).await? else {
        return Ok(None);
    };
    if !can(db, principal, tx::EVALUATION_RUN_VIEW, raw.project_id, lock).await? {
        return Ok(None);
    }
    let target = snapshot(db, run_id).await?;
    let disposition = evidence_disposition(db, run_id).await?;
    Ok(Some(run_from_raw(&raw, target, disposition)))
}

/// Ports the private `target(Connection, project, kind, targetId, environment)`: resolves an
/// `AGENT_VERSION` target from `agent_versions`, or a `DEPLOYMENT` target from `deployment_policy_snapshots`.
pub async fn resolve_target(
    db: &impl ConnectionTrait,
    project: Uuid,
    kind: &str,
    target_id: Uuid,
    environment: Uuid,
) -> Result<Option<Target>, DbErr> {
    match kind {
        "AGENT_VERSION" => {
            let statement = Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT versioned.id AS agent_version_id, NULL::uuid AS deployment_id, environment.id AS environment_definition_version_id, environment.logical_environment_class AS environment_class, versioned.content_digest AS agent_digest, \
                     NULL::text AS target_digest, NULL::text AS plan_digest, NULL::text AS package_digest, NULL::text AS binding_digest, versioned.catalog_release_id, versioned.catalog_release_digest, environment.content_digest AS environment_digest \
                 FROM agent_versions versioned JOIN agents agent ON agent.id = versioned.agent_id \
                   JOIN environment_definition_versions environment ON environment.id = $1 \
                 WHERE versioned.id = $2 AND agent.project_id = $3 AND environment.catalog_release_id = versioned.catalog_release_id",
                [environment.into(), target_id.into(), project.into()],
            );
            match db.query_one_raw(statement).await? {
                Some(row) => Ok(Some(target_from_row(&row)?)),
                None => Ok(None),
            }
        }
        "DEPLOYMENT" => {
            let statement = Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT deployment.agent_version_id, deployment.id AS deployment_id, environment.id AS environment_definition_version_id, environment.logical_environment_class AS environment_class, versioned.content_digest AS agent_digest, \
                     policy.target_digest, policy.plan_digest, policy.package_digest, policy.binding_digest, versioned.catalog_release_id, versioned.catalog_release_digest, environment.content_digest AS environment_digest \
                 FROM deployments deployment JOIN agent_versions versioned ON versioned.id = deployment.agent_version_id \
                   JOIN environment_definition_versions environment ON environment.id = deployment.environment_definition_version_id \
                   JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id \
                 WHERE deployment.id = $1 AND deployment.project_id = $2 AND deployment.environment_definition_version_id = $3",
                [target_id.into(), project.into(), environment.into()],
            );
            match db.query_one_raw(statement).await? {
                Some(row) => Ok(Some(target_from_row(&row)?)),
                None => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

pub async fn target_from_snapshot(
    db: &impl ConnectionTrait,
    run: Uuid,
) -> Result<Option<Target>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT agent_version_id,deployment_id,environment_definition_version_id,logical_environment_class AS environment_class,agent_content_digest AS agent_digest,target_digest,plan_digest,package_digest,binding_digest,catalog_release_id,catalog_release_digest,environment_content_digest AS environment_digest \
         FROM evaluation_target_snapshots WHERE run_id = $1",
        [run.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(target_from_row(&row)?)),
        None => Ok(None),
    }
}

pub async fn version_for_digest(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
    digest: &str,
) -> Result<Option<EvaluationDefinitionVersion>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id FROM evaluation_definition_versions WHERE definition_id = $1 AND content_digest = $2",
        [definition_id.into(), digest.into()],
    );
    match db.query_one_raw(statement).await? {
        Some(row) => version(db, row.try_get_by("id")?).await,
        None => Ok(None),
    }
}

pub async fn next_version_number(
    db: &impl ConnectionTrait,
    definition_id: Uuid,
) -> Result<i64, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT COALESCE(MAX(version_number), 0) + 1 AS next_version_number FROM evaluation_definition_versions WHERE definition_id = $1",
        [definition_id.into()],
    );
    db.query_one_raw(statement)
        .await?
        .expect("COALESCE(...) always returns exactly one row")
        .try_get_by("next_version_number")
}

pub async fn project_active(db: &impl ConnectionTrait, project: Uuid) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT lifecycle_status = 'ACTIVE' AS active FROM projects WHERE id = $1 FOR UPDATE",
        [project.into()],
    );
    Ok(db
        .query_one_raw(statement)
        .await?
        .map(|row| row.try_get_by("active"))
        .transpose()?
        .unwrap_or(false))
}
