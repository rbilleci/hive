//! Ports `PostgresEvaluationRepository`'s read methods and their shared
//! private helpers (`definition`, `draft`, `version`, `rawRun`, `snapshot`,
//! `visibleRun`, `target`, ...).

use chrono::{DateTime, Utc};
use hive_application::evaluation::EvaluationRunStatus;
use hive_application::evaluation::{
    Connection as AppConnection, Edge, EvaluationArtifactMetadata, EvaluationAuditEvent,
    EvaluationCaseRun, EvaluationDefinition, EvaluationDefinitionConnection,
    EvaluationDefinitionVersion, EvaluationDefinitionVersionConnection, EvaluationMetricResult,
    EvaluationRun, EvaluationRunConnection, EvaluationTarget, EvaluationTargetSnapshot,
};
use sea_orm::{ConnectionTrait, DbErr, QueryResult, Statement};
use uuid::Uuid;

use super::cursors::{
    decode_number_id_cursor, decode_ordinal_id_cursor, decode_target_cursor, decode_text_id_cursor,
    decode_time_id_cursor, encode_number_id_cursor, encode_ordinal_id_cursor, encode_target_cursor,
    encode_text_id_cursor, encode_time_id_cursor,
};
use super::rows::{
    artifact_row, audit_row, case_row, definition_project, draft_row, metric_row, redacted_draft,
    redacted_version, run_from_raw, run_status, snapshot_row, target_from_row, target_row,
    version_project, version_row, RawRun, Target,
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

pub async fn definitions(
    db: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<EvaluationDefinitionConnection>, DbErr> {
    if !can(
        db,
        principal,
        tx::EVALUATION_DEFINITION_VIEW,
        project,
        false,
    )
    .await?
    {
        return Ok(None);
    }
    let scope = project.to_string();
    let cursor = match decode_time_id_cursor(after, "definitions", &scope, "") {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
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
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
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
         WHERE definition.project_id = $1 AND ($2::timestamptz IS NULL OR (definition.created_at, definition.id) < ($2::timestamptz, $3::uuid)) \
         ORDER BY definition.created_at DESC, definition.id DESC LIMIT $4",
        [
            project.into(),
            cursor.as_ref().map(|value| value.time).into(),
            cursor.as_ref().map(|value| value.id).into(),
            (first + 1).into(),
        ],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values: Vec<EvaluationDefinition> = rows_found
        .iter()
        .map(|row| definition_row(row, can_author, can_publish))
        .collect::<Result<Vec<_>, _>>()?;
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationDefinition>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_time_id_cursor("definitions", &scope, "", value.created_at, value.id),
            node: value,
        })
        .collect();
    let end_cursor = edges.last().map(|edge| edge.cursor.clone());
    Ok(Some(EvaluationDefinitionConnection {
        edges,
        has_next_page: has_next,
        end_cursor,
    }))
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

pub async fn definition_version(
    db: &impl ConnectionTrait,
    principal: Uuid,
    version_id: Uuid,
) -> Result<Option<EvaluationDefinitionVersion>, DbErr> {
    let Some(project) = version_project(db, version_id).await? else {
        return Ok(None);
    };
    if !can(
        db,
        principal,
        tx::EVALUATION_DEFINITION_VIEW,
        project,
        false,
    )
    .await?
    {
        return Ok(None);
    }
    let Some(value) = version(db, version_id).await? else {
        return Ok(None);
    };
    let can_author = can(
        db,
        principal,
        tx::EVALUATION_DEFINITION_AUTHOR,
        project,
        false,
    )
    .await?;
    Ok(Some(if can_author {
        value
    } else {
        redacted_version(&value)
    }))
}

pub async fn definition_versions(
    db: &impl ConnectionTrait,
    principal: Uuid,
    definition_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<EvaluationDefinitionVersionConnection>, DbErr> {
    let Some(project) = definition_project(db, definition_id).await? else {
        return Ok(None);
    };
    if !can(
        db,
        principal,
        tx::EVALUATION_DEFINITION_VIEW,
        project,
        false,
    )
    .await?
    {
        return Ok(None);
    }
    let scope = definition_id.to_string();
    let cursor = match decode_number_id_cursor(after, "versions", &scope) {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let can_author = can(
        db,
        principal,
        tx::EVALUATION_DEFINITION_AUTHOR,
        project,
        false,
    )
    .await?;
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, definition_id, version_number, CASE WHEN $1 THEN canonical_document::text ELSE '' END AS canonical_document, \
             content_digest, based_on_version_id, published_by, published_at \
         FROM evaluation_definition_versions WHERE definition_id = $2 \
           AND ($3::bigint IS NULL OR (version_number, id) < ($3::bigint, $4::uuid)) \
         ORDER BY version_number DESC, id DESC LIMIT $5",
        [
            can_author.into(),
            definition_id.into(),
            cursor.as_ref().map(|value| value.number).into(),
            cursor.as_ref().map(|value| value.id).into(),
            (first + 1).into(),
        ],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values: Vec<EvaluationDefinitionVersion> = rows_found
        .iter()
        .map(|row| version_row(row, ""))
        .collect::<Result<Vec<_>, _>>()?;
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationDefinitionVersion>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_number_id_cursor("versions", &scope, value.number, value.id),
            node: value,
        })
        .collect();
    let end_cursor = edges.last().map(|edge| edge.cursor.clone());
    Ok(Some(EvaluationDefinitionVersionConnection {
        edges,
        has_next_page: has_next,
        end_cursor,
    }))
}

pub async fn definition_version_usage(
    db: &impl ConnectionTrait,
    principal: Uuid,
    version_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<EvaluationRunConnection>, DbErr> {
    let Some(project) = version_project(db, version_id).await? else {
        return Ok(None);
    };
    if !can(db, principal, tx::EVALUATION_RUN_VIEW, project, false).await? {
        return Ok(None);
    }
    let scope = version_id.to_string();
    let cursor = match decode_time_id_cursor(after, "versionUsage", &scope, "") {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let rows = run_rows(
        db,
        "definition_version_id = $1",
        version_id,
        cursor.as_ref().map(|value| value.time),
        cursor.as_ref().map(|value| value.id),
        first,
    )
    .await?;
    let mut values = rows;
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationRun>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_time_id_cursor("versionUsage", &scope, "", value.created_at, value.id),
            node: value,
        })
        .collect();
    let end_cursor = edges.last().map(|edge| edge.cursor.clone());
    Ok(Some(EvaluationRunConnection {
        edges,
        has_next_page: has_next,
        end_cursor,
    }))
}

pub async fn runs(
    db: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
    status: Option<EvaluationRunStatus>,
    after: Option<&str>,
    first: i32,
) -> Result<Option<EvaluationRunConnection>, DbErr> {
    if !can(db, principal, tx::EVALUATION_RUN_VIEW, project, false).await? {
        return Ok(None);
    }
    let status = status.map(EvaluationRunStatus::as_str);
    let filter = status.unwrap_or("");
    let scope = project.to_string();
    let cursor = match decode_time_id_cursor(after, "runs", &scope, filter) {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        run_columns_sql(
            "run.project_id = $1 AND ($2::text IS NULL OR run.lifecycle_status = $2) \
               AND ($3::timestamptz IS NULL OR (run.created_at, run.id) < ($3::timestamptz, $4::uuid))",
            5,
        ),
        [
            project.into(),
            status.into(),
            cursor.as_ref().map(|value| value.time).into(),
            cursor.as_ref().map(|value| value.id).into(),
            (first + 1).into(),
        ],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values: Vec<EvaluationRun> = rows_found
        .iter()
        .map(run_row_full)
        .collect::<Result<Vec<_>, _>>()?;
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationRun>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_time_id_cursor("runs", &scope, filter, value.created_at, value.id),
            node: value,
        })
        .collect();
    let end_cursor = edges.last().map(|edge| edge.cursor.clone());
    Ok(Some(EvaluationRunConnection {
        edges,
        has_next_page: has_next,
        end_cursor,
    }))
}

/// Shared column list for `runs()`/`run_rows()`: `run.environment_definition_version_id` and
/// `snapshot.environment_definition_version_id` are the one name collision this join produces
/// (every other selected column is already unique across the two tables), so the snapshot's copy
/// gets an explicit alias for name-based decode.
fn run_columns_sql(predicate: &str, limit_index: usize) -> String {
    format!(
        "SELECT run.id,run.project_id,run.definition_version_id,run.target_kind,run.target_id,run.environment_definition_version_id,run.source_run_id,run.lifecycle_status,run.generation,run.outcome_category,run.outcome_code,run.created_at,run.started_at,run.completed_at, \
             snapshot.agent_version_id,snapshot.deployment_id,snapshot.environment_definition_version_id AS snapshot_environment_definition_version_id,snapshot.logical_environment_class,snapshot.agent_content_digest,snapshot.target_digest,snapshot.plan_digest,snapshot.package_digest,snapshot.binding_digest,snapshot.catalog_release_id,snapshot.catalog_release_digest,snapshot.environment_content_digest, \
             CASE WHEN EXISTS (SELECT 1 FROM deployment_evidence_snapshots evidence WHERE evidence.source_evaluation_run_id = run.id) THEN 'APPENDED' WHEN snapshot.deployment_id IS NOT NULL THEN 'NOT_PENDING' ELSE 'NOT_A_DEPLOYMENT' END AS deployment_evidence_disposition \
         FROM evaluation_runs run LEFT JOIN evaluation_target_snapshots snapshot ON snapshot.run_id = run.id \
         WHERE {predicate} \
         ORDER BY run.created_at DESC, run.id DESC LIMIT ${limit_index}"
    )
}

fn run_row_full(row: &QueryResult) -> Result<EvaluationRun, DbErr> {
    let raw = RawRun {
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
    };
    let has_snapshot: Option<Uuid> = row.try_get_by("agent_version_id")?;
    let target = match has_snapshot {
        Some(agent_version_id) => Some(EvaluationTargetSnapshot {
            agent_version_id,
            deployment_id: row.try_get_by("deployment_id")?,
            environment_definition_version_id: row
                .try_get_by("snapshot_environment_definition_version_id")?,
            logical_environment_class: row.try_get_by("logical_environment_class")?,
            agent_content_digest: row.try_get_by("agent_content_digest")?,
            target_digest: row.try_get_by("target_digest")?,
            plan_digest: row.try_get_by("plan_digest")?,
            package_digest: row.try_get_by("package_digest")?,
            binding_digest: row.try_get_by("binding_digest")?,
            catalog_release_id: row.try_get_by("catalog_release_id")?,
            catalog_release_digest: row.try_get_by("catalog_release_digest")?,
            environment_content_digest: row.try_get_by("environment_content_digest")?,
        }),
        None => None,
    };
    Ok(run_from_raw(
        &raw,
        target,
        row.try_get_by("deployment_evidence_disposition")?,
    ))
}

/// Ports `runRows(connection, "definition_version_id = ?", ...)`: the one caller whose predicate
/// has no `status` parameter alongside the scope.
async fn run_rows(
    db: &impl ConnectionTrait,
    predicate: &str,
    scope: Uuid,
    time: Option<DateTime<Utc>>,
    id: Option<Uuid>,
    first: i32,
) -> Result<Vec<EvaluationRun>, DbErr> {
    let full_predicate = format!(
        "run.{predicate} AND ($2::timestamptz IS NULL OR (run.created_at, run.id) < ($2::timestamptz, $3::uuid))"
    );
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        run_columns_sql(&full_predicate, 4),
        [scope.into(), time.into(), id.into(), (first + 1).into()],
    );
    let rows_found = db.query_all_raw(statement).await?;
    rows_found.iter().map(run_row_full).collect()
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

pub async fn visible_run(
    db: &impl ConnectionTrait,
    principal: Uuid,
    run: Uuid,
) -> Result<bool, DbErr> {
    let Some(raw) = raw_run(db, run, false).await? else {
        return Ok(false);
    };
    can(
        db,
        principal,
        tx::EVALUATION_RUN_VIEW,
        raw.project_id,
        false,
    )
    .await
}

pub async fn cases(
    db: &impl ConnectionTrait,
    principal: Uuid,
    run: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<AppConnection<EvaluationCaseRun>>, DbErr> {
    if !visible_run(db, principal, run).await? {
        return Ok(None);
    }
    let scope = run.to_string();
    let cursor = match decode_ordinal_id_cursor(after, "cases", &scope) {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id,case_key,ordinal,lifecycle_status,passed,failure_code,completed_at FROM evaluation_case_runs \
         WHERE run_id = $1 AND ($2::integer IS NULL OR (ordinal, id) > ($2::integer, $3::uuid)) ORDER BY ordinal, id LIMIT $4",
        [
            run.into(),
            cursor.as_ref().map(|value| value.ordinal).into(),
            cursor.as_ref().map(|value| value.id).into(),
            (first + 1).into(),
        ],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values: Vec<EvaluationCaseRun> = rows_found
        .iter()
        .map(case_row)
        .collect::<Result<Vec<_>, _>>()?;
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationCaseRun>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_ordinal_id_cursor("cases", &scope, value.ordinal, value.id),
            node: value,
        })
        .collect();
    let end_cursor = edges.last().map(|edge| edge.cursor.clone());
    Ok(Some(AppConnection {
        edges,
        has_next_page: has_next,
        end_cursor,
    }))
}

pub async fn metrics(
    db: &impl ConnectionTrait,
    principal: Uuid,
    run: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<AppConnection<EvaluationMetricResult>>, DbErr> {
    if !visible_run(db, principal, run).await? {
        return Ok(None);
    }
    let scope = run.to_string();
    let cursor = match decode_text_id_cursor(after, "metrics", &scope) {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id,metric_code,value::float8 AS value,threshold::float8 AS threshold,passed FROM evaluation_metric_results \
         WHERE run_id = $1 AND ($2::text IS NULL OR (metric_code, id) > ($2::text, $3::uuid)) ORDER BY metric_code, id LIMIT $4",
        [
            run.into(),
            cursor.as_ref().map(|value| value.text.clone()).into(),
            cursor.as_ref().map(|value| value.id).into(),
            (first + 1).into(),
        ],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values: Vec<EvaluationMetricResult> = rows_found
        .iter()
        .map(metric_row)
        .collect::<Result<Vec<_>, _>>()?;
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationMetricResult>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_text_id_cursor("metrics", &scope, &value.code, value.id),
            node: value,
        })
        .collect();
    let end_cursor = edges.last().map(|edge| edge.cursor.clone());
    Ok(Some(AppConnection {
        edges,
        has_next_page: has_next,
        end_cursor,
    }))
}

pub async fn artifacts(
    db: &impl ConnectionTrait,
    principal: Uuid,
    run: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<AppConnection<EvaluationArtifactMetadata>>, DbErr> {
    if !visible_run(db, principal, run).await? {
        return Ok(None);
    }
    let scope = run.to_string();
    let cursor = match decode_text_id_cursor(after, "artifacts", &scope) {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id,artifact_kind,content_digest,media_type,byte_length FROM evaluation_artifact_metadata \
         WHERE run_id = $1 AND ($2::text IS NULL OR (artifact_kind, id) > ($2::text, $3::uuid)) ORDER BY artifact_kind, id LIMIT $4",
        [
            run.into(),
            cursor.as_ref().map(|value| value.text.clone()).into(),
            cursor.as_ref().map(|value| value.id).into(),
            (first + 1).into(),
        ],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values: Vec<EvaluationArtifactMetadata> = rows_found
        .iter()
        .map(artifact_row)
        .collect::<Result<Vec<_>, _>>()?;
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationArtifactMetadata>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_text_id_cursor("artifacts", &scope, &value.kind, value.id),
            node: value,
        })
        .collect();
    let end_cursor = edges.last().map(|edge| edge.cursor.clone());
    Ok(Some(AppConnection {
        edges,
        has_next_page: has_next,
        end_cursor,
    }))
}

pub async fn audit(
    db: &impl ConnectionTrait,
    principal: Uuid,
    run: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<AppConnection<EvaluationAuditEvent>>, DbErr> {
    if !visible_run(db, principal, run).await? {
        return Ok(None);
    }
    let scope = run.to_string();
    let cursor = match decode_time_id_cursor(after, "audit", &scope, "") {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id,action,occurred_at,facts->>'summary' AS summary FROM evaluation_audit_events \
         WHERE run_id = $1 AND ($2::timestamptz IS NULL OR (occurred_at, id) < ($2::timestamptz, $3::uuid)) ORDER BY occurred_at DESC, id DESC LIMIT $4",
        [
            run.into(),
            cursor.as_ref().map(|value| value.time).into(),
            cursor.as_ref().map(|value| value.id).into(),
            (first + 1).into(),
        ],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values: Vec<EvaluationAuditEvent> = rows_found
        .iter()
        .map(audit_row)
        .collect::<Result<Vec<_>, _>>()?;
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationAuditEvent>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_time_id_cursor("audit", &scope, "", value.occurred_at, value.id),
            node: value,
        })
        .collect();
    let end_cursor = edges.last().map(|edge| edge.cursor.clone());
    Ok(Some(AppConnection {
        edges,
        has_next_page: has_next,
        end_cursor,
    }))
}

pub async fn targets(
    db: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
    definition_version_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<AppConnection<EvaluationTarget>>, DbErr> {
    if !can(db, principal, tx::EVALUATION_RUN_RUN, project, false).await? {
        return Ok(None);
    }
    if version_project(db, definition_version_id).await? != Some(project) {
        return Ok(None);
    }
    let Some(version_value) = version(db, definition_version_id).await? else {
        return Ok(None);
    };
    let mut kinds: Vec<String> =
        hive_application::evaluation::document::target_kinds(&version_value.canonical_document)
            .into_iter()
            .collect();
    kinds.sort();
    let mut environments: Vec<String> =
        hive_application::evaluation::document::environment_classes(
            &version_value.canonical_document,
        )
        .into_iter()
        .collect();
    environments.sort();
    let scope = format!("{project}|{definition_version_id}");
    let filter = format!("{}|{}", kinds.join(","), environments.join(","));
    let cursor = match decode_target_cursor(after, &scope, &filter) {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    if let Some(cursor) = &cursor {
        if !kinds.contains(&cursor.kind) || cursor.display_name.trim().is_empty() {
            return Ok(None);
        }
    }
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT target_kind, target_id, agent_version_id, environment_definition_version_id, logical_environment_class, display_name \
         FROM evaluation_target_projections \
         WHERE project_id = $1 AND target_kind = ANY($2::text[]) AND logical_environment_class = ANY($3::text[]) \
           AND ($4::text IS NULL OR (target_kind, display_name, target_id, environment_definition_version_id) > ($4::text, $5::text, $6::uuid, $7::uuid)) \
         ORDER BY target_kind, display_name, target_id, environment_definition_version_id LIMIT $8",
        [
            project.into(),
            kinds.clone().into(),
            environments.clone().into(),
            cursor.as_ref().map(|value| value.kind.clone()).into(),
            cursor.as_ref().map(|value| value.display_name.clone()).into(),
            cursor.as_ref().map(|value| value.id).into(),
            cursor.as_ref().map(|value| value.environment_id).into(),
            (first + 1).into(),
        ],
    );
    let rows_found = db.query_all_raw(statement).await?;
    let mut values: Vec<EvaluationTarget> = rows_found
        .iter()
        .map(target_row)
        .collect::<Result<Vec<_>, _>>()?;
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationTarget>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_target_cursor(
                &scope,
                &filter,
                &value.kind,
                &value.display_name,
                value.id,
                value.environment_definition_version_id,
            ),
            node: value,
        })
        .collect();
    let end_cursor = edges.last().map(|edge| edge.cursor.clone());
    Ok(Some(AppConnection {
        edges,
        has_next_page: has_next,
        end_cursor,
    }))
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
