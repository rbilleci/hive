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
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::cursors::{
    decode_number_id_cursor, decode_ordinal_id_cursor, decode_target_cursor, decode_text_id_cursor,
    decode_time_id_cursor, encode_number_id_cursor, encode_ordinal_id_cursor, encode_target_cursor,
    encode_text_id_cursor, encode_time_id_cursor,
};
use super::rows::{
    artifact_row, audit_row, case_row, definition_project, draft_row, metric_row, raw_run_row,
    redacted_draft, redacted_version, run_from_raw, run_status, snapshot_row, target_from_row,
    target_row, version_project, version_row, RawRun, Target,
};
use crate::capability::tx;

pub(super) async fn can(
    conn: &mut PgConnection,
    principal: Uuid,
    capability: &str,
    project: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    tx::has_evaluation_capability(conn, principal, capability, project, lock).await
}

pub async fn definitions(
    conn: &mut PgConnection,
    principal: Uuid,
    project: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<EvaluationDefinitionConnection>, sqlx::Error> {
    if !can(
        conn,
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
    let cursor = match decode_time_id_cursor(after, &scope, "") {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let can_author = can(
        conn,
        principal,
        tx::EVALUATION_DEFINITION_AUTHOR,
        project,
        false,
    )
    .await?;
    let can_publish = can(
        conn,
        principal,
        tx::EVALUATION_DEFINITION_PUBLISH,
        project,
        false,
    )
    .await?;
    let rows = sqlx::query(
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
    )
    .bind(project)
    .bind(cursor.as_ref().map(|value| value.time))
    .bind(cursor.as_ref().map(|value| value.id))
    .bind((first + 1) as i64)
    .fetch_all(&mut *conn)
    .await?;
    let mut values: Vec<EvaluationDefinition> = rows
        .iter()
        .map(|row| definition_row(row, can_author, can_publish))
        .collect();
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationDefinition>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_time_id_cursor(&scope, "", value.created_at, value.id),
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
    row: &sqlx::postgres::PgRow,
    can_author: bool,
    can_publish: bool,
) -> EvaluationDefinition {
    let definition_id: Uuid = row.get("id");
    let draft = draft_row(
        definition_id,
        row.get("draft_document"),
        row.get("draft_revision"),
        row.get("draft_validation_status"),
        row.get("draft_diagnostics"),
        row.get("draft_based_on_version_id"),
        row.get("draft_updated_at"),
    );
    let latest_id: Option<Uuid> = row.get("latest_id");
    let latest_version = latest_id.map(|_| version_row(row, "latest_"));
    EvaluationDefinition {
        id: definition_id,
        project_id: row.get("project_id"),
        slug: row.get("slug"),
        lifecycle_status: row.get("lifecycle_status"),
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
        created_at: row.get("created_at"),
    }
}

pub async fn definition(
    conn: &mut PgConnection,
    principal: Uuid,
    definition_id: Uuid,
    lock: bool,
) -> Result<Option<EvaluationDefinition>, sqlx::Error> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let sql = format!("SELECT project_id FROM evaluation_definitions WHERE id = $1{suffix}");
    let row: Option<(Uuid,)> = sqlx::query_as(&sql)
        .bind(definition_id)
        .fetch_optional(&mut *conn)
        .await?;
    let Some((project,)) = row else {
        return Ok(None);
    };
    if !can(
        conn,
        principal,
        tx::EVALUATION_DEFINITION_VIEW,
        project,
        lock,
    )
    .await?
    {
        return Ok(None);
    }
    let can_author = can(
        conn,
        principal,
        tx::EVALUATION_DEFINITION_AUTHOR,
        project,
        false,
    )
    .await?;
    let can_publish = can(
        conn,
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
    let row = sqlx::query(&sql)
        .bind(definition_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|row| definition_row(&row, can_author, can_publish)))
}

pub async fn draft(
    conn: &mut PgConnection,
    definition_id: Uuid,
    lock: bool,
) -> Result<hive_application::evaluation::models::EvaluationDefinitionDraft, sqlx::Error> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let sql = format!(
        "SELECT canonical_document::text, revision, validation_status, diagnostics::text, based_on_version_id, updated_at \
         FROM evaluation_definition_drafts WHERE definition_id = $1{suffix}"
    );
    let row = sqlx::query(&sql)
        .bind(definition_id)
        .fetch_one(&mut *conn)
        .await?;
    Ok(draft_row(
        definition_id,
        row.get(0),
        row.get(1),
        row.get(2),
        row.get(3),
        row.get(4),
        row.get(5),
    ))
}

pub async fn version(
    conn: &mut PgConnection,
    version_id: Uuid,
) -> Result<Option<EvaluationDefinitionVersion>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, definition_id, version_number, canonical_document::text AS canonical_document, content_digest, \
             based_on_version_id, published_by, published_at \
         FROM evaluation_definition_versions WHERE id = $1",
    )
    .bind(version_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| version_row(&row, "")))
}

pub async fn definition_version(
    conn: &mut PgConnection,
    principal: Uuid,
    version_id: Uuid,
) -> Result<Option<EvaluationDefinitionVersion>, sqlx::Error> {
    let Some(project) = version_project(conn, version_id).await? else {
        return Ok(None);
    };
    if !can(
        conn,
        principal,
        tx::EVALUATION_DEFINITION_VIEW,
        project,
        false,
    )
    .await?
    {
        return Ok(None);
    }
    let Some(value) = version(conn, version_id).await? else {
        return Ok(None);
    };
    let can_author = can(
        conn,
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
    conn: &mut PgConnection,
    principal: Uuid,
    definition_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<EvaluationDefinitionVersionConnection>, sqlx::Error> {
    let Some(project) = definition_project(conn, definition_id).await? else {
        return Ok(None);
    };
    if !can(
        conn,
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
    let cursor = match decode_number_id_cursor(after, &scope) {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let can_author = can(
        conn,
        principal,
        tx::EVALUATION_DEFINITION_AUTHOR,
        project,
        false,
    )
    .await?;
    let rows = sqlx::query(
        "SELECT id, definition_id, version_number, CASE WHEN $1 THEN canonical_document::text ELSE '' END AS canonical_document, \
             content_digest, based_on_version_id, published_by, published_at \
         FROM evaluation_definition_versions WHERE definition_id = $2 \
           AND ($3::bigint IS NULL OR (version_number, id) < ($3::bigint, $4::uuid)) \
         ORDER BY version_number DESC, id DESC LIMIT $5",
    )
    .bind(can_author)
    .bind(definition_id)
    .bind(cursor.as_ref().map(|value| value.number))
    .bind(cursor.as_ref().map(|value| value.id))
    .bind((first + 1) as i64)
    .fetch_all(&mut *conn)
    .await?;
    let mut values: Vec<EvaluationDefinitionVersion> =
        rows.iter().map(|row| version_row(row, "")).collect();
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationDefinitionVersion>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_number_id_cursor(&scope, value.number, value.id),
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
    conn: &mut PgConnection,
    principal: Uuid,
    version_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<EvaluationRunConnection>, sqlx::Error> {
    let Some(project) = version_project(conn, version_id).await? else {
        return Ok(None);
    };
    if !can(conn, principal, tx::EVALUATION_RUN_VIEW, project, false).await? {
        return Ok(None);
    }
    let scope = version_id.to_string();
    let cursor = match decode_time_id_cursor(after, &scope, "") {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let rows = run_rows(
        conn,
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
            cursor: encode_time_id_cursor(&scope, "", value.created_at, value.id),
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
    conn: &mut PgConnection,
    principal: Uuid,
    project: Uuid,
    status: Option<EvaluationRunStatus>,
    after: Option<&str>,
    first: i32,
) -> Result<Option<EvaluationRunConnection>, sqlx::Error> {
    if !can(conn, principal, tx::EVALUATION_RUN_VIEW, project, false).await? {
        return Ok(None);
    }
    let status = status.map(EvaluationRunStatus::as_str);
    let filter = status.unwrap_or("");
    let scope = project.to_string();
    let cursor = match decode_time_id_cursor(after, &scope, filter) {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let rows = sqlx::query(
        "SELECT run.id,run.project_id,run.definition_version_id,run.target_kind,run.target_id,run.environment_definition_version_id,run.source_run_id,run.lifecycle_status,run.generation,run.outcome_category,run.outcome_code,run.created_at,run.started_at,run.completed_at, \
             snapshot.agent_version_id,snapshot.deployment_id,snapshot.environment_definition_version_id,snapshot.logical_environment_class,snapshot.agent_content_digest,snapshot.target_digest,snapshot.plan_digest,snapshot.package_digest,snapshot.binding_digest,snapshot.catalog_release_id,snapshot.catalog_release_digest,snapshot.environment_content_digest, \
             CASE WHEN EXISTS (SELECT 1 FROM deployment_evidence_snapshots evidence WHERE evidence.source_evaluation_run_id = run.id) THEN 'APPENDED' WHEN snapshot.deployment_id IS NOT NULL THEN 'NOT_PENDING' ELSE 'NOT_A_DEPLOYMENT' END \
         FROM evaluation_runs run LEFT JOIN evaluation_target_snapshots snapshot ON snapshot.run_id = run.id \
         WHERE run.project_id = $1 AND ($2::text IS NULL OR run.lifecycle_status = $2) \
           AND ($3::timestamptz IS NULL OR (run.created_at, run.id) < ($3::timestamptz, $4::uuid)) \
         ORDER BY run.created_at DESC, run.id DESC LIMIT $5",
    )
    .bind(project)
    .bind(status)
    .bind(cursor.as_ref().map(|value| value.time))
    .bind(cursor.as_ref().map(|value| value.id))
    .bind((first + 1) as i64)
    .fetch_all(&mut *conn)
    .await?;
    let mut values: Vec<EvaluationRun> = rows.iter().map(run_row_full).collect();
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationRun>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_time_id_cursor(&scope, filter, value.created_at, value.id),
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

fn run_row_full(row: &sqlx::postgres::PgRow) -> EvaluationRun {
    let raw = RawRun {
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
    };
    let has_snapshot: Option<Uuid> = row.get(14);
    let target = has_snapshot.map(|_| EvaluationTargetSnapshot {
        agent_version_id: row.get(14),
        deployment_id: row.get(15),
        environment_definition_version_id: row.get(16),
        logical_environment_class: row.get(17),
        agent_content_digest: row.get(18),
        target_digest: row.get(19),
        plan_digest: row.get(20),
        package_digest: row.get(21),
        binding_digest: row.get(22),
        catalog_release_id: row.get(23),
        catalog_release_digest: row.get(24),
        environment_content_digest: row.get(25),
    });
    run_from_raw(&raw, target, row.get(26))
}

/// Ports `runRows(connection, "definition_version_id = ?", ...)`: the one caller whose predicate
/// has no `status` parameter alongside the scope.
async fn run_rows(
    conn: &mut PgConnection,
    predicate: &str,
    scope: Uuid,
    time: Option<DateTime<Utc>>,
    id: Option<Uuid>,
    first: i32,
) -> Result<Vec<EvaluationRun>, sqlx::Error> {
    let sql = format!(
        "SELECT run.id,run.project_id,run.definition_version_id,run.target_kind,run.target_id,run.environment_definition_version_id,run.source_run_id,run.lifecycle_status,run.generation,run.outcome_category,run.outcome_code,run.created_at,run.started_at,run.completed_at, \
             snapshot.agent_version_id,snapshot.deployment_id,snapshot.environment_definition_version_id,snapshot.logical_environment_class,snapshot.agent_content_digest,snapshot.target_digest,snapshot.plan_digest,snapshot.package_digest,snapshot.binding_digest,snapshot.catalog_release_id,snapshot.catalog_release_digest,snapshot.environment_content_digest, \
             CASE WHEN EXISTS (SELECT 1 FROM deployment_evidence_snapshots evidence WHERE evidence.source_evaluation_run_id = run.id) THEN 'APPENDED' WHEN snapshot.deployment_id IS NOT NULL THEN 'NOT_PENDING' ELSE 'NOT_A_DEPLOYMENT' END \
         FROM evaluation_runs run LEFT JOIN evaluation_target_snapshots snapshot ON snapshot.run_id = run.id \
         WHERE run.{predicate} AND ($2::timestamptz IS NULL OR (run.created_at, run.id) < ($2::timestamptz, $3::uuid)) \
         ORDER BY run.created_at DESC, run.id DESC LIMIT $4"
    );
    let rows = sqlx::query(&sql)
        .bind(scope)
        .bind(time)
        .bind(id)
        .bind((first + 1) as i64)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(run_row_full).collect())
}

pub async fn raw_run(
    conn: &mut PgConnection,
    id: Uuid,
    lock: bool,
) -> Result<Option<RawRun>, sqlx::Error> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let sql = format!(
        "SELECT id, project_id, definition_version_id, target_kind, target_id, environment_definition_version_id, \
             source_run_id, lifecycle_status, generation, outcome_category, outcome_code, created_at, started_at, completed_at \
         FROM evaluation_runs WHERE id = $1{suffix}"
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.as_ref().map(raw_run_row))
}

pub async fn snapshot(
    conn: &mut PgConnection,
    run: Uuid,
) -> Result<Option<EvaluationTargetSnapshot>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT agent_version_id,deployment_id,environment_definition_version_id,logical_environment_class,agent_content_digest,target_digest,plan_digest,package_digest,binding_digest,catalog_release_id,catalog_release_digest,environment_content_digest \
         FROM evaluation_target_snapshots WHERE run_id = $1",
    )
    .bind(run)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.as_ref().map(snapshot_row))
}

pub async fn evidence_disposition(
    conn: &mut PgConnection,
    run: Uuid,
) -> Result<String, sqlx::Error> {
    let row: (String,) = sqlx::query_as(
        "SELECT CASE WHEN EXISTS (SELECT 1 FROM deployment_evidence_snapshots WHERE source_evaluation_run_id = $1) THEN 'APPENDED' \
             WHEN EXISTS (SELECT 1 FROM evaluation_target_snapshots WHERE run_id = $2 AND deployment_id IS NOT NULL) THEN 'NOT_PENDING' \
             ELSE 'NOT_A_DEPLOYMENT' END",
    )
    .bind(run)
    .bind(run)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.0)
}

pub async fn run(
    conn: &mut PgConnection,
    principal: Uuid,
    run_id: Uuid,
    lock: bool,
) -> Result<Option<EvaluationRun>, sqlx::Error> {
    let Some(raw) = raw_run(conn, run_id, lock).await? else {
        return Ok(None);
    };
    if !can(
        conn,
        principal,
        tx::EVALUATION_RUN_VIEW,
        raw.project_id,
        lock,
    )
    .await?
    {
        return Ok(None);
    }
    let target = snapshot(conn, run_id).await?;
    let disposition = evidence_disposition(conn, run_id).await?;
    Ok(Some(run_from_raw(&raw, target, disposition)))
}

pub async fn visible_run(
    conn: &mut PgConnection,
    principal: Uuid,
    run: Uuid,
) -> Result<bool, sqlx::Error> {
    let Some(raw) = raw_run(conn, run, false).await? else {
        return Ok(false);
    };
    can(
        conn,
        principal,
        tx::EVALUATION_RUN_VIEW,
        raw.project_id,
        false,
    )
    .await
}

pub async fn cases(
    conn: &mut PgConnection,
    principal: Uuid,
    run: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<AppConnection<EvaluationCaseRun>>, sqlx::Error> {
    if !visible_run(conn, principal, run).await? {
        return Ok(None);
    }
    let scope = run.to_string();
    let cursor = match decode_ordinal_id_cursor(after, &scope) {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let rows = sqlx::query(
        "SELECT id,case_key,ordinal,lifecycle_status,passed,failure_code,completed_at FROM evaluation_case_runs \
         WHERE run_id = $1 AND ($2::integer IS NULL OR (ordinal, id) > ($2::integer, $3::uuid)) ORDER BY ordinal, id LIMIT $4",
    )
    .bind(run)
    .bind(cursor.as_ref().map(|value| value.ordinal))
    .bind(cursor.as_ref().map(|value| value.id))
    .bind((first + 1) as i64)
    .fetch_all(&mut *conn)
    .await?;
    let mut values: Vec<EvaluationCaseRun> = rows.iter().map(case_row).collect();
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationCaseRun>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_ordinal_id_cursor(&scope, value.ordinal, value.id),
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
    conn: &mut PgConnection,
    principal: Uuid,
    run: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<AppConnection<EvaluationMetricResult>>, sqlx::Error> {
    if !visible_run(conn, principal, run).await? {
        return Ok(None);
    }
    let scope = run.to_string();
    let cursor = match decode_text_id_cursor(after, &scope) {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let rows = sqlx::query(
        "SELECT id,metric_code,value::float8,threshold::float8,passed FROM evaluation_metric_results \
         WHERE run_id = $1 AND ($2::text IS NULL OR (metric_code, id) > ($2::text, $3::uuid)) ORDER BY metric_code, id LIMIT $4",
    )
    .bind(run)
    .bind(cursor.as_ref().map(|value| value.text.clone()))
    .bind(cursor.as_ref().map(|value| value.id))
    .bind((first + 1) as i64)
    .fetch_all(&mut *conn)
    .await?;
    let mut values: Vec<EvaluationMetricResult> = rows.iter().map(metric_row).collect();
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationMetricResult>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_text_id_cursor(&scope, &value.code, value.id),
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
    conn: &mut PgConnection,
    principal: Uuid,
    run: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<AppConnection<EvaluationArtifactMetadata>>, sqlx::Error> {
    if !visible_run(conn, principal, run).await? {
        return Ok(None);
    }
    let scope = run.to_string();
    let cursor = match decode_text_id_cursor(after, &scope) {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let rows = sqlx::query(
        "SELECT id,artifact_kind,content_digest,media_type,byte_length FROM evaluation_artifact_metadata \
         WHERE run_id = $1 AND ($2::text IS NULL OR (artifact_kind, id) > ($2::text, $3::uuid)) ORDER BY artifact_kind, id LIMIT $4",
    )
    .bind(run)
    .bind(cursor.as_ref().map(|value| value.text.clone()))
    .bind(cursor.as_ref().map(|value| value.id))
    .bind((first + 1) as i64)
    .fetch_all(&mut *conn)
    .await?;
    let mut values: Vec<EvaluationArtifactMetadata> = rows.iter().map(artifact_row).collect();
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationArtifactMetadata>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_text_id_cursor(&scope, &value.kind, value.id),
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
    conn: &mut PgConnection,
    principal: Uuid,
    run: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<AppConnection<EvaluationAuditEvent>>, sqlx::Error> {
    if !visible_run(conn, principal, run).await? {
        return Ok(None);
    }
    let scope = run.to_string();
    let cursor = match decode_time_id_cursor(after, &scope, "") {
        Ok(value) => value,
        Err(()) => return Ok(None),
    };
    let rows = sqlx::query(
        "SELECT id,action,occurred_at,facts->>'summary' FROM evaluation_audit_events \
         WHERE run_id = $1 AND ($2::timestamptz IS NULL OR (occurred_at, id) < ($2::timestamptz, $3::uuid)) ORDER BY occurred_at DESC, id DESC LIMIT $4",
    )
    .bind(run)
    .bind(cursor.as_ref().map(|value| value.time))
    .bind(cursor.as_ref().map(|value| value.id))
    .bind((first + 1) as i64)
    .fetch_all(&mut *conn)
    .await?;
    let mut values: Vec<EvaluationAuditEvent> = rows.iter().map(audit_row).collect();
    let has_next = values.len() > first as usize;
    if has_next {
        values.truncate(first as usize);
    }
    let edges: Vec<Edge<EvaluationAuditEvent>> = values
        .into_iter()
        .map(|value| Edge {
            cursor: encode_time_id_cursor(&scope, "", value.occurred_at, value.id),
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
    conn: &mut PgConnection,
    principal: Uuid,
    project: Uuid,
    definition_version_id: Uuid,
    after: Option<&str>,
    first: i32,
) -> Result<Option<AppConnection<EvaluationTarget>>, sqlx::Error> {
    if !can(conn, principal, tx::EVALUATION_RUN_RUN, project, false).await? {
        return Ok(None);
    }
    if version_project(conn, definition_version_id).await? != Some(project) {
        return Ok(None);
    }
    let Some(version_value) = version(conn, definition_version_id).await? else {
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
    let rows = sqlx::query(
        "SELECT target_kind, target_id, agent_version_id, environment_definition_version_id, logical_environment_class, display_name \
         FROM evaluation_target_projections \
         WHERE project_id = $1 AND target_kind = ANY($2::text[]) AND logical_environment_class = ANY($3::text[]) \
           AND ($4::text IS NULL OR (target_kind, display_name, target_id, environment_definition_version_id) > ($4::text, $5::text, $6::uuid, $7::uuid)) \
         ORDER BY target_kind, display_name, target_id, environment_definition_version_id LIMIT $8",
    )
    .bind(project)
    .bind(&kinds)
    .bind(&environments)
    .bind(cursor.as_ref().map(|value| value.kind.clone()))
    .bind(cursor.as_ref().map(|value| value.display_name.clone()))
    .bind(cursor.as_ref().map(|value| value.id))
    .bind(cursor.as_ref().map(|value| value.environment_id))
    .bind((first + 1) as i64)
    .fetch_all(&mut *conn)
    .await?;
    let mut values: Vec<EvaluationTarget> = rows.iter().map(target_row).collect();
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
    conn: &mut PgConnection,
    project: Uuid,
    kind: &str,
    target_id: Uuid,
    environment: Uuid,
) -> Result<Option<Target>, sqlx::Error> {
    match kind {
        "AGENT_VERSION" => {
            let row = sqlx::query(
                "SELECT versioned.id, NULL::uuid, environment.id, environment.logical_environment_class, versioned.content_digest, \
                     NULL::text, NULL::text, NULL::text, NULL::text, versioned.catalog_release_id, versioned.catalog_release_digest, environment.content_digest \
                 FROM agent_versions versioned JOIN agents agent ON agent.id = versioned.agent_id \
                   JOIN environment_definition_versions environment ON environment.id = $1 \
                 WHERE versioned.id = $2 AND agent.project_id = $3 AND environment.catalog_release_id = versioned.catalog_release_id",
            )
            .bind(environment)
            .bind(target_id)
            .bind(project)
            .fetch_optional(&mut *conn)
            .await?;
            Ok(row.as_ref().map(target_from_row))
        }
        "DEPLOYMENT" => {
            let row = sqlx::query(
                "SELECT deployment.agent_version_id, deployment.id, environment.id, environment.logical_environment_class, versioned.content_digest, \
                     policy.target_digest, policy.plan_digest, policy.package_digest, policy.binding_digest, versioned.catalog_release_id, versioned.catalog_release_digest, environment.content_digest \
                 FROM deployments deployment JOIN agent_versions versioned ON versioned.id = deployment.agent_version_id \
                   JOIN environment_definition_versions environment ON environment.id = deployment.environment_definition_version_id \
                   JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id \
                 WHERE deployment.id = $1 AND deployment.project_id = $2 AND deployment.environment_definition_version_id = $3",
            )
            .bind(target_id)
            .bind(project)
            .bind(environment)
            .fetch_optional(&mut *conn)
            .await?;
            Ok(row.as_ref().map(target_from_row))
        }
        _ => Ok(None),
    }
}

pub async fn target_from_snapshot(
    conn: &mut PgConnection,
    run: Uuid,
) -> Result<Option<Target>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT agent_version_id,deployment_id,environment_definition_version_id,logical_environment_class,agent_content_digest,target_digest,plan_digest,package_digest,binding_digest,catalog_release_id,catalog_release_digest,environment_content_digest \
         FROM evaluation_target_snapshots WHERE run_id = $1",
    )
    .bind(run)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.as_ref().map(target_from_row))
}

pub async fn version_for_digest(
    conn: &mut PgConnection,
    definition_id: Uuid,
    digest: &str,
) -> Result<Option<EvaluationDefinitionVersion>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM evaluation_definition_versions WHERE definition_id = $1 AND content_digest = $2")
        .bind(definition_id)
        .bind(digest)
        .fetch_optional(&mut *conn)
        .await?;
    match row {
        Some((id,)) => version(conn, id).await,
        None => Ok(None),
    }
}

pub async fn next_version_number(
    conn: &mut PgConnection,
    definition_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as("SELECT COALESCE(MAX(version_number), 0) + 1 FROM evaluation_definition_versions WHERE definition_id = $1")
        .bind(definition_id)
        .fetch_one(&mut *conn)
        .await?;
    Ok(row.0)
}

pub async fn project_active(conn: &mut PgConnection, project: Uuid) -> Result<bool, sqlx::Error> {
    let row: Option<(bool,)> =
        sqlx::query_as("SELECT lifecycle_status = 'ACTIVE' FROM projects WHERE id = $1 FOR UPDATE")
            .bind(project)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.map(|row| row.0).unwrap_or(false))
}
