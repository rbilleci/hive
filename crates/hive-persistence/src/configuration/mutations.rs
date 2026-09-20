//! Ports the `ConfigurationRepository` write commands: `createResource`, `updateDraft`,
//! `validate`, `publish`, `createMcpServer`, `updateMcpServer`, and `saveLegacyTool`.
//!
//! `GSR-PERSISTENCE`: each command opens a `sea_orm::DatabaseTransaction` via
//! `TransactionTrait::begin`, and every locked capability re-check calls
//! `capability::has_capability`/`capability::queries::*` directly with `lock: true` and `&txn` —
//! `ConnectionTrait` covers a transaction the same as a bare connection, so no
//! `capability::tx`-style hand-duplicated twin is needed here (see `capability/mod.rs`'s doc
//! comment on why that module's old `sqlx`-generic limitation does not apply to `sea_orm`).

use super::rows;
use crate::sql::json_array;
use hive_application::configuration::{
    digest, document, resource_identity, ConfigurationMutationResult, ConfigurationProblem,
    ConfigurationRepositoryError as RepositoryError, TypedReference,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use uuid::Uuid;

async fn configuration_write(
    txn: &impl ConnectionTrait,
    actor: Uuid,
    project: Uuid,
) -> Result<bool, sea_orm::DbErr> {
    crate::capability::has_capability(
        txn,
        actor,
        crate::capability::CONFIGURATION_AUTHOR,
        crate::capability::Scope::Project(project),
        true,
    )
    .await
}

async fn active_project(txn: &impl ConnectionTrait, project: Uuid) -> Result<bool, sea_orm::DbErr> {
    crate::capability::queries::active_project(txn, project, true).await
}

#[allow(clippy::too_many_arguments)]
pub async fn create_resource(
    db: &DatabaseConnection,
    actor: Uuid,
    project: Uuid,
    kind: String,
    name: String,
    identity: String,
    content: String,
    dependencies: Vec<TypedReference>,
) -> Result<ConfigurationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(rows::other)?;
    if !configuration_write(&txn, actor, project)
        .await
        .map_err(rows::other)?
        || !active_project(&txn, project).await.map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::forbidden(),
        ));
    }
    if !rows::resolved(&txn, project, &dependencies)
        .await
        .map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::invalid_draft(),
        ));
    }
    let mut resource_diagnostics = rows::diagnostics(&kind, &content, &dependencies);
    if kind == "MODEL_PROFILE"
        && !rows::models_available(
            &txn,
            &dependencies,
            rows::profile_environment(&content).as_deref(),
        )
        .await
        .map_err(rows::other)?
    {
        resource_diagnostics
            .push("A selected model is unavailable in the requested environment.".to_string());
    }
    let id = Uuid::new_v4();
    let resource_document = document(&kind, &name, &identity, &content, &dependencies);
    let resource_digest = digest(&resource_document);
    let insert_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO reusable_resources (id, project_id, resource_kind, name, identity, current_draft_revision, lifecycle_status) VALUES ($1, $2, $3, $4, $5, 1, 'ACTIVE') ON CONFLICT (project_id, resource_kind, identity) DO NOTHING",
        [
            id.into(),
            project.into(),
            kind.clone().into(),
            name.clone().into(),
            identity.clone().into(),
        ],
    );
    let inserted = match txn.execute_raw(insert_statement).await {
        Ok(result) => result,
        Err(error) if rows::is_unique_violation(&error) => {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::invalid(),
            ));
        }
        Err(error) => return Err(rows::other(error)),
    };
    if inserted.rows_affected() == 0 {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::invalid(),
        ));
    }
    rows::insert_draft(
        &txn,
        id,
        1,
        &content,
        &resource_document,
        &resource_digest,
        &dependencies,
        &resource_diagnostics,
    )
    .await
    .map_err(rows::other)?;
    rows::audit(
        &txn,
        actor,
        project,
        "REUSABLE_RESOURCE_CREATED",
        id,
        &resource_digest,
        "",
    )
    .await
    .map_err(rows::other)?;
    let value = rows::resource(&txn, project, id)
        .await
        .map_err(rows::other)?
        .expect("the resource just created is visible in the same transaction");
    txn.commit().await.map_err(rows::other)?;
    Ok(ConfigurationMutationResult::resource(value))
}

pub async fn update_draft(
    db: &DatabaseConnection,
    actor: Uuid,
    project: Uuid,
    id: Uuid,
    expected_revision: i64,
    content: String,
    dependencies: Vec<TypedReference>,
) -> Result<ConfigurationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(rows::other)?;
    if !configuration_write(&txn, actor, project)
        .await
        .map_err(rows::other)?
        || !active_project(&txn, project).await.map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::forbidden(),
        ));
    }
    let Some(current) = rows::locked_resource(&txn, project, id)
        .await
        .map_err(rows::other)?
    else {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::unavailable(),
        ));
    };
    if current.draft_revision != expected_revision {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::conflict(id, expected_revision, current.draft_revision),
        ));
    }
    if current.lifecycle != "ACTIVE" {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::lifecycle(),
        ));
    }
    if !rows::resolved(&txn, project, &dependencies)
        .await
        .map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::invalid_draft(),
        ));
    }
    let next_revision = expected_revision + 1;
    let mut resource_diagnostics = rows::diagnostics(&current.kind, &content, &dependencies);
    if current.kind == "MODEL_PROFILE"
        && !rows::models_available(
            &txn,
            &dependencies,
            rows::profile_environment(&content).as_deref(),
        )
        .await
        .map_err(rows::other)?
    {
        resource_diagnostics
            .push("A selected model is unavailable in the requested environment.".to_string());
    }
    let resource_document = document(
        &current.kind,
        &current.name,
        &current.identity,
        &content,
        &dependencies,
    );
    let resource_digest = digest(&resource_document);
    rows::insert_draft(
        &txn,
        id,
        next_revision,
        &content,
        &resource_document,
        &resource_digest,
        &dependencies,
        &resource_diagnostics,
    )
    .await
    .map_err(rows::other)?;
    let update_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE reusable_resources SET current_draft_revision = $1 WHERE id = $2",
        [next_revision.into(), id.into()],
    );
    txn.execute_raw(update_statement)
        .await
        .map_err(rows::other)?;
    rows::audit(
        &txn,
        actor,
        project,
        "REUSABLE_RESOURCE_DRAFT_UPDATED",
        id,
        &resource_digest,
        "",
    )
    .await
    .map_err(rows::other)?;
    let value = rows::resource(&txn, project, id)
        .await
        .map_err(rows::other)?
        .expect("the resource just updated is visible in the same transaction");

    match txn.commit().await {
        Ok(()) => Ok(ConfigurationMutationResult::resource(value)),
        Err(error) if crate::sql::is_serialization_failure_db(&error) => {
            let retry = db.begin().await.map_err(rows::other)?;
            let raced = rows::locked_resource(&retry, project, id)
                .await
                .map_err(rows::other)?;
            Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::conflict(
                    id,
                    expected_revision,
                    raced
                        .map(|value| value.draft_revision)
                        .unwrap_or(expected_revision),
                ),
            ))
        }
        Err(error) => Err(rows::other(error)),
    }
}

pub async fn validate(
    db: &DatabaseConnection,
    actor: Uuid,
    project: Uuid,
    id: Uuid,
    expected_revision: i64,
) -> Result<ConfigurationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(rows::other)?;
    if !configuration_write(&txn, actor, project)
        .await
        .map_err(rows::other)?
        || !active_project(&txn, project).await.map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::forbidden(),
        ));
    }
    let Some(current) = rows::locked_resource(&txn, project, id)
        .await
        .map_err(rows::other)?
    else {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::unavailable(),
        ));
    };
    if current.draft_revision != expected_revision {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::conflict(id, expected_revision, current.draft_revision),
        ));
    }
    if current.lifecycle != "ACTIVE" {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::lifecycle(),
        ));
    }
    let current_draft = rows::draft_row(&txn, id, expected_revision)
        .await
        .map_err(rows::other)?;
    let draft_refs = rows::typed(&current_draft.dependencies);
    let mut resource_diagnostics =
        rows::diagnostics(&current.kind, &current_draft.content, &draft_refs);
    if current.kind == "MODEL_PROFILE"
        && !rows::models_available(
            &txn,
            &draft_refs,
            rows::profile_environment(&current_draft.content).as_deref(),
        )
        .await
        .map_err(rows::other)?
    {
        resource_diagnostics
            .push("A selected model is unavailable in the requested environment.".to_string());
    }
    let status = if resource_diagnostics.is_empty() {
        "VALID"
    } else {
        "INVALID"
    };
    let update_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE reusable_resource_drafts SET validation_status = $1, diagnostics = $2::jsonb WHERE resource_id = $3 AND revision = $4",
        [
            status.into(),
            json_array(&resource_diagnostics).into(),
            id.into(),
            expected_revision.into(),
        ],
    );
    txn.execute_raw(update_statement)
        .await
        .map_err(rows::other)?;
    rows::audit(
        &txn,
        actor,
        project,
        "REUSABLE_RESOURCE_VALIDATED",
        id,
        &current_draft.digest,
        status,
    )
    .await
    .map_err(rows::other)?;
    let value = rows::resource(&txn, project, id)
        .await
        .map_err(rows::other)?
        .expect("the resource just validated is visible in the same transaction");

    match txn.commit().await {
        Ok(()) => Ok(ConfigurationMutationResult::resource(value)),
        Err(error) if crate::sql::is_serialization_failure_db(&error) => {
            let retry = db.begin().await.map_err(rows::other)?;
            let raced = rows::locked_resource(&retry, project, id)
                .await
                .map_err(rows::other)?;
            Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::conflict(
                    id,
                    expected_revision,
                    raced
                        .map(|value| value.draft_revision)
                        .unwrap_or(expected_revision),
                ),
            ))
        }
        Err(error) => Err(rows::other(error)),
    }
}

pub async fn publish(
    db: &DatabaseConnection,
    actor: Uuid,
    project: Uuid,
    id: Uuid,
    expected_revision: i64,
) -> Result<ConfigurationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(rows::other)?;
    let can_publish = crate::capability::has_capability(
        db,
        actor,
        crate::capability::CONFIGURATION_PUBLISH,
        crate::capability::Scope::Project(project),
        false,
    )
    .await
    .map_err(rows::other)?;
    // The write re-check happens below via `configuration_write`, which also covers
    // CONFIGURATION.PUBLISH (the same branch as CONFIGURATION.AUTHOR); this earlier,
    // unlocked check exists only so an unauthorized caller never reaches the locked
    // resource read at all, matching Java's `publish()` capability call ordering
    // (checked before any row lock is taken).
    if !can_publish
        || !configuration_write(&txn, actor, project)
            .await
            .map_err(rows::other)?
        || !active_project(&txn, project).await.map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::forbidden(),
        ));
    }
    let Some(current) = rows::locked_resource(&txn, project, id)
        .await
        .map_err(rows::other)?
    else {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::unavailable(),
        ));
    };
    if current.draft_revision != expected_revision {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::conflict(id, expected_revision, current.draft_revision),
        ));
    }
    if current.lifecycle != "ACTIVE" {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::lifecycle(),
        ));
    }
    let current_draft = rows::draft_row(&txn, id, expected_revision)
        .await
        .map_err(rows::other)?;
    let draft_refs = rows::typed(&current_draft.dependencies);
    if current_draft.validation != "VALID"
        || !rows::resolved(&txn, project, &draft_refs)
            .await
            .map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::invalid_draft(),
        ));
    }
    if let Some(published_version) = current.published_version {
        if Some(current_draft.digest.clone())
            == rows::published_digest(&txn, id, published_version)
                .await
                .map_err(rows::other)?
        {
            let value = rows::resource(&txn, project, id)
                .await
                .map_err(rows::other)?
                .expect("the resource just published is visible in the same transaction");
            txn.commit().await.map_err(rows::other)?;
            return Ok(ConfigurationMutationResult::resource(value));
        }
    }
    let next_version = current
        .published_version
        .map(|version| version + 1)
        .unwrap_or(1);
    let insert_version_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO reusable_resource_versions (resource_id, version, canonical_document, content_digest, dependencies, published_by) VALUES ($1, $2, $3::jsonb, $4, $5::jsonb, $6)",
        [
            id.into(),
            next_version.into(),
            current_draft.document.clone().into(),
            current_draft.digest.clone().into(),
            json_array(&current_draft.dependencies).into(),
            actor.into(),
        ],
    );
    txn.execute_raw(insert_version_statement)
        .await
        .map_err(rows::other)?;
    let update_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE reusable_resources SET current_published_version = $1 WHERE id = $2",
        [next_version.into(), id.into()],
    );
    txn.execute_raw(update_statement)
        .await
        .map_err(rows::other)?;
    rows::audit(
        &txn,
        actor,
        project,
        "REUSABLE_RESOURCE_PUBLISHED",
        id,
        &current_draft.digest,
        &format!("v{next_version}"),
    )
    .await
    .map_err(rows::other)?;
    let value = rows::resource(&txn, project, id)
        .await
        .map_err(rows::other)?
        .expect("the resource just published is visible in the same transaction");

    match txn.commit().await {
        Ok(()) => Ok(ConfigurationMutationResult::resource(value)),
        Err(error) if crate::sql::is_serialization_failure_db(&error) => {
            let retry = db.begin().await.map_err(rows::other)?;
            let raced = rows::locked_resource(&retry, project, id)
                .await
                .map_err(rows::other)?;
            Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::conflict(
                    id,
                    expected_revision,
                    raced
                        .map(|value| value.draft_revision)
                        .unwrap_or(expected_revision),
                ),
            ))
        }
        Err(error) => Err(rows::other(error)),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn create_mcp_server(
    db: &DatabaseConnection,
    actor: Uuid,
    project: Uuid,
    server_id: String,
    name: String,
    definition: TypedReference,
    environment: String,
    enabled: bool,
    transport_type: String,
    command: Option<String>,
    arguments: Vec<String>,
    remote_url: Option<String>,
    redacted_bindings: Vec<String>,
    tools: Vec<String>,
    resources: Vec<String>,
    prompts: Vec<String>,
) -> Result<ConfigurationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(rows::other)?;
    if !configuration_write(&txn, actor, project)
        .await
        .map_err(rows::other)?
        || !active_project(&txn, project).await.map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::forbidden(),
        ));
    }
    if !rows::catalog_definition(&txn, &definition, Some(&environment))
        .await
        .map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::invalid_draft(),
        ));
    }
    let id = Uuid::new_v4();
    let insert_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO project_tool_connections (id, project_id, server_id, name, definition_identity, definition_version, environment, enabled, transport_type, \
             stdio_command, stdio_arguments, remote_url, redacted_secret_reference, redacted_bindings, declared_tools, declared_resources, declared_prompts, \
             lifecycle_status, rotation_summary, revision) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::jsonb, $12, $13, $14::jsonb, $15::jsonb, $16::jsonb, $17::jsonb, 'ACTIVE', '', 1)",
        [
            id.into(),
            project.into(),
            server_id.clone().into(),
            name.clone().into(),
            definition.identity.clone().into(),
            definition.version.clone().into(),
            environment.clone().into(),
            enabled.into(),
            transport_type.clone().into(),
            command.clone().into(),
            json_array(&arguments).into(),
            remote_url.clone().into(),
            rows::first_binding(&redacted_bindings).into(),
            json_array(&redacted_bindings).into(),
            json_array(&tools).into(),
            json_array(&resources).into(),
            json_array(&prompts).into(),
        ],
    );
    if let Err(error) = txn.execute_raw(insert_statement).await {
        if rows::is_unique_violation(&error) {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::invalid(),
            ));
        }
        return Err(rows::other(error));
    }
    let content_digest = rows::mcp_digest(
        &server_id,
        &name,
        &definition,
        &environment,
        enabled,
        &transport_type,
        command.as_deref(),
        &arguments,
        remote_url.as_deref(),
        &redacted_bindings,
        &tools,
        &resources,
        &prompts,
    );
    rows::audit(
        &txn,
        actor,
        project,
        "MCP_SERVER_CREATED",
        id,
        &content_digest,
        "ACTIVE",
    )
    .await
    .map_err(rows::other)?;
    let value = rows::mcp_server(&txn, project, id)
        .await
        .map_err(rows::other)?;
    txn.commit().await.map_err(rows::other)?;
    Ok(ConfigurationMutationResult::mcp_server(value))
}

#[allow(clippy::too_many_arguments)]
pub async fn update_mcp_server(
    db: &DatabaseConnection,
    actor: Uuid,
    project: Uuid,
    server: Uuid,
    expected_revision: i64,
    name: String,
    definition: TypedReference,
    environment: String,
    enabled: bool,
    transport_type: String,
    command: Option<String>,
    arguments: Vec<String>,
    remote_url: Option<String>,
    redacted_bindings: Vec<String>,
    tools: Vec<String>,
    resources: Vec<String>,
    prompts: Vec<String>,
    lifecycle_status: String,
) -> Result<ConfigurationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(rows::other)?;
    if !configuration_write(&txn, actor, project)
        .await
        .map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::forbidden(),
        ));
    }
    let Some(existing) = rows::locked_tool(&txn, project, server)
        .await
        .map_err(rows::other)?
    else {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::unavailable(),
        ));
    };
    if existing.revision != expected_revision {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::conflict(server, expected_revision, existing.revision),
        ));
    }
    if !active_project(&txn, project).await.map_err(rows::other)? && lifecycle_status != "ARCHIVED"
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::lifecycle(),
        ));
    }
    if !rows::catalog_definition(&txn, &definition, Some(&environment))
        .await
        .map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::invalid_draft(),
        ));
    }
    let update_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE project_tool_connections SET name = $1, definition_identity = $2, definition_version = $3, environment = $4, enabled = $5, transport_type = $6, \
             stdio_command = $7, stdio_arguments = $8::jsonb, remote_url = $9, redacted_secret_reference = $10, redacted_bindings = $11::jsonb, declared_tools = $12::jsonb, \
             declared_resources = $13::jsonb, declared_prompts = $14::jsonb, lifecycle_status = $15, rotation_summary = '', revision = revision + 1 \
         WHERE id = $16 AND project_id = $17",
        [
            name.clone().into(),
            definition.identity.clone().into(),
            definition.version.clone().into(),
            environment.clone().into(),
            enabled.into(),
            transport_type.clone().into(),
            command.clone().into(),
            json_array(&arguments).into(),
            remote_url.clone().into(),
            rows::first_binding(&redacted_bindings).into(),
            json_array(&redacted_bindings).into(),
            json_array(&tools).into(),
            json_array(&resources).into(),
            json_array(&prompts).into(),
            lifecycle_status.clone().into(),
            server.into(),
            project.into(),
        ],
    );
    if let Err(error) = txn.execute_raw(update_statement).await {
        if rows::is_unique_violation(&error) {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::invalid(),
            ));
        }
        return Err(rows::other(error));
    }
    let content_digest = rows::mcp_digest(
        &existing.server_id,
        &name,
        &definition,
        &environment,
        enabled,
        &transport_type,
        command.as_deref(),
        &arguments,
        remote_url.as_deref(),
        &redacted_bindings,
        &tools,
        &resources,
        &prompts,
    );
    rows::audit(
        &txn,
        actor,
        project,
        "MCP_SERVER_UPDATED",
        server,
        &content_digest,
        &lifecycle_status,
    )
    .await
    .map_err(rows::other)?;
    let value = rows::mcp_server(&txn, project, server)
        .await
        .map_err(rows::other)?;

    match txn.commit().await {
        Ok(()) => Ok(ConfigurationMutationResult::mcp_server(value)),
        Err(error) if crate::sql::is_serialization_failure_db(&error) => {
            let retry = db.begin().await.map_err(rows::other)?;
            let raced = rows::locked_tool(&retry, project, server)
                .await
                .map_err(rows::other)?;
            Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::conflict(
                    server,
                    expected_revision,
                    raced
                        .map(|value| value.revision)
                        .unwrap_or(expected_revision),
                ),
            ))
        }
        Err(error) => Err(rows::other(error)),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn save_legacy_tool(
    db: &DatabaseConnection,
    actor: Uuid,
    project: Uuid,
    tool: Option<Uuid>,
    expected_revision: i64,
    name: String,
    definition: TypedReference,
    environment: String,
    redacted_secret_reference: String,
    lifecycle: String,
    rotation_summary: String,
) -> Result<ConfigurationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(rows::other)?;
    if !configuration_write(&txn, actor, project)
        .await
        .map_err(rows::other)?
        || !active_project(&txn, project).await.map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::forbidden(),
        ));
    }
    if !rows::catalog_definition(&txn, &definition, Some(&environment))
        .await
        .map_err(rows::other)?
    {
        return Ok(ConfigurationMutationResult::refused(
            ConfigurationProblem::invalid_draft(),
        ));
    }
    let tool_id = tool.unwrap_or_else(Uuid::new_v4);
    let next_revision = if tool.is_none() {
        1
    } else {
        expected_revision + 1
    };
    let server_id = match tool {
        Some(existing_id) => {
            let Some(existing) = rows::locked_tool(&txn, project, existing_id)
                .await
                .map_err(rows::other)?
            else {
                return Ok(ConfigurationMutationResult::refused(
                    ConfigurationProblem::unavailable(),
                ));
            };
            if existing.revision != expected_revision {
                return Ok(ConfigurationMutationResult::refused(
                    ConfigurationProblem::conflict(
                        existing_id,
                        expected_revision,
                        existing.revision,
                    ),
                ));
            }
            existing.server_id
        }
        None => resource_identity::from_display_name(&name)
            .unwrap_or_else(|| "legacy-server".to_string()),
    };
    let upsert_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO project_tool_connections (id, project_id, server_id, name, definition_identity, definition_version, environment, redacted_secret_reference, redacted_bindings, lifecycle_status, rotation_summary, revision) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10, $11, $12) \
         ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, definition_identity = EXCLUDED.definition_identity, definition_version = EXCLUDED.definition_version, \
           environment = EXCLUDED.environment, redacted_secret_reference = EXCLUDED.redacted_secret_reference, redacted_bindings = EXCLUDED.redacted_bindings, \
           lifecycle_status = EXCLUDED.lifecycle_status, rotation_summary = EXCLUDED.rotation_summary, revision = EXCLUDED.revision",
        [
            tool_id.into(),
            project.into(),
            server_id.clone().into(),
            name.clone().into(),
            definition.identity.clone().into(),
            definition.version.clone().into(),
            environment.clone().into(),
            redacted_secret_reference.clone().into(),
            json_array(std::slice::from_ref(&redacted_secret_reference)).into(),
            lifecycle.clone().into(),
            rotation_summary.clone().into(),
            next_revision.into(),
        ],
    );
    if let Err(error) = txn.execute_raw(upsert_statement).await {
        if rows::is_unique_violation(&error) {
            return Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::invalid(),
            ));
        }
        return Err(rows::other(error));
    }
    let content_digest = digest(&format!(
        "{name}{}{environment}{redacted_secret_reference}",
        definition.value()
    ));
    rows::audit(
        &txn,
        actor,
        project,
        "LEGACY_TOOL_METADATA_SAVED",
        tool_id,
        &content_digest,
        &lifecycle,
    )
    .await
    .map_err(rows::other)?;
    let value = rows::mcp_server(&txn, project, tool_id)
        .await
        .map_err(rows::other)?;

    match txn.commit().await {
        Ok(()) => Ok(ConfigurationMutationResult::mcp_server(value)),
        Err(error) if crate::sql::is_serialization_failure_db(&error) => {
            let retry = db.begin().await.map_err(rows::other)?;
            let raced = match tool {
                Some(existing_id) => rows::locked_tool(&retry, project, existing_id)
                    .await
                    .map_err(rows::other)?,
                None => None,
            };
            Ok(ConfigurationMutationResult::refused(
                ConfigurationProblem::conflict(
                    tool_id,
                    expected_revision,
                    raced
                        .map(|value| value.revision)
                        .unwrap_or(expected_revision),
                ),
            ))
        }
        Err(error) => Err(rows::other(error)),
    }
}
