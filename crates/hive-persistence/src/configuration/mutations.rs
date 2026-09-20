//! The configuration commands (`createReusableResource`, `updateReusableResourceDraft`,
//! `validateReusableResource`, `publishReusableResource`, `createProjectMcpServer`,
//! `updateProjectMcpServer`, `saveProjectToolConnectionMetadata`) on SeaORM entities.
//!
//! Each runs in one transaction: it re-checks the principal's authority with the evaluator's
//! locking checks, locks the row it changes `FOR UPDATE`, compares the revision, writes with the
//! revision in the `WHERE` clause, and records its audit row before it commits. A command answers
//! with the stored `reusable_resources` or `project_tool_connections` row itself; the GraphQL
//! payload exposes it as the same generated type the reads use.
//!
//! Aurora DSQL reports a lost optimistic race as SQLSTATE 40001 at commit, where Postgres would
//! have blocked on the row lock; `committed` turns that into the same revision conflict, reading
//! the revision the winner left behind.
//!
//! A refusal returns before `commit`, and the dropped transaction rolls back.

use super::rows;
use crate::capability;
use crate::entity::enums::{
    ConnectionLifecycleStatus, LifecycleStatus, LogicalEnvironmentClass, ReusableResourceKind,
    ReusableResourceValidationStatus,
};
use crate::entity::{
    project_tool_connections, reusable_resource_drafts, reusable_resource_versions,
    reusable_resources,
};
use crate::sql::is_serialization_failure_db;
use hive_application::configuration::{
    digest, document, resource_identity, ConfigurationMutationResult, ConfigurationProblem,
    ConfigurationRepositoryError as RepositoryError, TypedReference,
};
use sea_orm::sea_query::{Expr, ExprTrait, OnConflict};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr,
    EntityTrait, NotSet, QueryFilter, Set, TransactionTrait, TryInsertResult,
};
use uuid::Uuid;

pub type MutationResult =
    ConfigurationMutationResult<reusable_resources::Model, project_tool_connections::Model>;

fn refused(problem: ConfigurationProblem) -> Result<MutationResult, RepositoryError> {
    Ok(ConfigurationMutationResult::refused(problem))
}

fn approved<E: ActiveEnum<Value = String>>(value: &str) -> Option<E> {
    E::try_from_value(&value.to_string()).ok()
}

/// `CONFIGURATION.AUTHOR` on an active project, checked under the evaluator's locks.
async fn may_write(txn: &impl ConnectionTrait, actor: Uuid, project: Uuid) -> Result<bool, DbErr> {
    Ok(configuration_write(txn, actor, project).await?
        && capability::queries::active_project(txn, project, true).await?)
}

async fn configuration_write(
    txn: &impl ConnectionTrait,
    actor: Uuid,
    project: Uuid,
) -> Result<bool, DbErr> {
    capability::has_capability(
        txn,
        actor,
        capability::CONFIGURATION_AUTHOR,
        capability::Scope::Project(project),
        true,
    )
    .await
}

/// Which row a command changed, for the revision a lost commit race reports.
#[derive(Clone, Copy)]
enum Subject {
    Resource(Uuid),
    Tool(Uuid),
}

/// Commits. A commit lost to a concurrent writer (SQLSTATE 40001) is the revision conflict the
/// row lock would have produced, with the revision the winner left behind.
async fn committed(
    db: &DatabaseConnection,
    txn: DatabaseTransaction,
    project: Uuid,
    subject: Subject,
    expected_revision: i64,
    result: MutationResult,
) -> Result<MutationResult, RepositoryError> {
    match txn.commit().await {
        Ok(()) => Ok(result),
        Err(error) if is_serialization_failure_db(&error) => {
            let retry = db.begin().await.map_err(rows::other)?;
            let (id, raced) = match subject {
                Subject::Resource(id) => (
                    id,
                    rows::locked_resource(&retry, project, id)
                        .await
                        .map_err(rows::other)?
                        .map(|resource| resource.current_draft_revision),
                ),
                Subject::Tool(id) => (
                    id,
                    rows::locked_tool(&retry, project, id)
                        .await
                        .map_err(rows::other)?
                        .map(|tool| tool.revision),
                ),
            };
            refused(ConfigurationProblem::conflict(
                id,
                expected_revision,
                raced.unwrap_or(expected_revision),
            ))
        }
        Err(error) => Err(rows::other(error)),
    }
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
) -> Result<MutationResult, RepositoryError> {
    let Some(resource_kind) = approved::<ReusableResourceKind>(&kind) else {
        return refused(ConfigurationProblem::invalid());
    };
    let txn = db.begin().await.map_err(rows::other)?;
    if !may_write(&txn, actor, project).await.map_err(rows::other)? {
        return refused(ConfigurationProblem::forbidden());
    }
    if !rows::resolved(&txn, project, &dependencies)
        .await
        .map_err(rows::other)?
    {
        return refused(ConfigurationProblem::invalid_draft());
    }
    let resource_diagnostics =
        rows::resource_diagnostics(&txn, resource_kind, &content, &dependencies)
            .await
            .map_err(rows::other)?;
    let id = Uuid::new_v4();
    let resource_document = document(&kind, &name, &identity, &content, &dependencies);
    let resource_digest = digest(&resource_document);
    // A taken identity is a conflict on the identity key; a taken name is a unique violation.
    let inserted = reusable_resources::Entity::insert(reusable_resources::ActiveModel {
        project_id: Set(project),
        resource_kind: Set(resource_kind),
        name: Set(name),
        identity: Set(identity),
        current_draft_revision: Set(1),
        current_published_version: Set(None),
        lifecycle_status: Set(LifecycleStatus::Active),
        id: Set(id),
    })
    .on_conflict(
        OnConflict::columns([
            reusable_resources::Column::ProjectId,
            reusable_resources::Column::ResourceKind,
            reusable_resources::Column::Identity,
        ])
        .do_nothing()
        .to_owned(),
    )
    .try_insert()
    .exec_without_returning(&txn)
    .await;
    match inserted {
        Ok(TryInsertResult::Inserted(1)) => {}
        Ok(_) => return refused(ConfigurationProblem::invalid()),
        Err(error) if rows::is_unique_violation(&error) => {
            return refused(ConfigurationProblem::invalid());
        }
        Err(error) => return Err(rows::other(error)),
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
    let value = rows::stored_resource(&txn, id).await.map_err(rows::other)?;
    txn.commit().await.map_err(rows::other)?;
    Ok(ConfigurationMutationResult::resource(value))
}

/// The resource a draft command works on, locked, or the refusal that stops the command.
async fn editable_resource(
    txn: &impl ConnectionTrait,
    project: Uuid,
    id: Uuid,
    expected_revision: i64,
) -> Result<Result<reusable_resources::Model, ConfigurationProblem>, DbErr> {
    let Some(current) = rows::locked_resource(txn, project, id).await? else {
        return Ok(Err(ConfigurationProblem::unavailable()));
    };
    if current.current_draft_revision != expected_revision {
        return Ok(Err(ConfigurationProblem::conflict(
            id,
            expected_revision,
            current.current_draft_revision,
        )));
    }
    if current.lifecycle_status != LifecycleStatus::Active {
        return Ok(Err(ConfigurationProblem::lifecycle()));
    }
    Ok(Ok(current))
}

pub async fn update_draft(
    db: &DatabaseConnection,
    actor: Uuid,
    project: Uuid,
    id: Uuid,
    expected_revision: i64,
    content: String,
    dependencies: Vec<TypedReference>,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(rows::other)?;
    if !may_write(&txn, actor, project).await.map_err(rows::other)? {
        return refused(ConfigurationProblem::forbidden());
    }
    let current = match editable_resource(&txn, project, id, expected_revision)
        .await
        .map_err(rows::other)?
    {
        Ok(current) => current,
        Err(problem) => return refused(problem),
    };
    if !rows::resolved(&txn, project, &dependencies)
        .await
        .map_err(rows::other)?
    {
        return refused(ConfigurationProblem::invalid_draft());
    }
    let next_revision = expected_revision + 1;
    let resource_diagnostics =
        rows::resource_diagnostics(&txn, current.resource_kind, &content, &dependencies)
            .await
            .map_err(rows::other)?;
    let resource_document = document(
        &current.resource_kind.to_value(),
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
    let moved = reusable_resources::Entity::update_many()
        .col_expr(
            reusable_resources::Column::CurrentDraftRevision,
            Expr::value(next_revision),
        )
        .filter(reusable_resources::Column::Id.eq(id))
        .filter(reusable_resources::Column::CurrentDraftRevision.eq(expected_revision))
        .exec(&txn)
        .await
        .map_err(rows::other)?;
    if moved.rows_affected != 1 {
        let actual = rows::stored_resource(&txn, id)
            .await
            .map_err(rows::other)?
            .current_draft_revision;
        return refused(ConfigurationProblem::conflict(
            id,
            expected_revision,
            actual,
        ));
    }
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
    let value = rows::stored_resource(&txn, id).await.map_err(rows::other)?;
    committed(
        db,
        txn,
        project,
        Subject::Resource(id),
        expected_revision,
        ConfigurationMutationResult::resource(value),
    )
    .await
}

pub async fn validate(
    db: &DatabaseConnection,
    actor: Uuid,
    project: Uuid,
    id: Uuid,
    expected_revision: i64,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(rows::other)?;
    if !may_write(&txn, actor, project).await.map_err(rows::other)? {
        return refused(ConfigurationProblem::forbidden());
    }
    let current = match editable_resource(&txn, project, id, expected_revision)
        .await
        .map_err(rows::other)?
    {
        Ok(current) => current,
        Err(problem) => return refused(problem),
    };
    let current_draft = rows::draft_row(&txn, id, expected_revision)
        .await
        .map_err(rows::other)?;
    let draft_refs = rows::typed(&rows::strings(&current_draft.dependencies));
    let resource_diagnostics = rows::resource_diagnostics(
        &txn,
        current.resource_kind,
        &current_draft.content,
        &draft_refs,
    )
    .await
    .map_err(rows::other)?;
    let status = if resource_diagnostics.is_empty() {
        ReusableResourceValidationStatus::Valid
    } else {
        ReusableResourceValidationStatus::Invalid
    };
    reusable_resource_drafts::Entity::update_many()
        .col_expr(
            reusable_resource_drafts::Column::ValidationStatus,
            Expr::value(status.to_value()),
        )
        .col_expr(
            reusable_resource_drafts::Column::Diagnostics,
            Expr::value(serde_json::json!(resource_diagnostics)),
        )
        .filter(reusable_resource_drafts::Column::ResourceId.eq(id))
        .filter(reusable_resource_drafts::Column::Revision.eq(expected_revision))
        .exec(&txn)
        .await
        .map_err(rows::other)?;
    rows::audit(
        &txn,
        actor,
        project,
        "REUSABLE_RESOURCE_VALIDATED",
        id,
        &current_draft.content_digest,
        &status.to_value(),
    )
    .await
    .map_err(rows::other)?;
    committed(
        db,
        txn,
        project,
        Subject::Resource(id),
        expected_revision,
        ConfigurationMutationResult::resource(current),
    )
    .await
}

pub async fn publish(
    db: &DatabaseConnection,
    actor: Uuid,
    project: Uuid,
    id: Uuid,
    expected_revision: i64,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(rows::other)?;
    // Checked without a lock first, so a caller without the capability takes no row lock at all.
    let can_publish = capability::has_capability(
        db,
        actor,
        capability::CONFIGURATION_PUBLISH,
        capability::Scope::Project(project),
        false,
    )
    .await
    .map_err(rows::other)?;
    if !can_publish || !may_write(&txn, actor, project).await.map_err(rows::other)? {
        return refused(ConfigurationProblem::forbidden());
    }
    let current = match editable_resource(&txn, project, id, expected_revision)
        .await
        .map_err(rows::other)?
    {
        Ok(current) => current,
        Err(problem) => return refused(problem),
    };
    let current_draft = rows::draft_row(&txn, id, expected_revision)
        .await
        .map_err(rows::other)?;
    let draft_refs = rows::typed(&rows::strings(&current_draft.dependencies));
    if current_draft.validation_status != ReusableResourceValidationStatus::Valid
        || !rows::resolved(&txn, project, &draft_refs)
            .await
            .map_err(rows::other)?
    {
        return refused(ConfigurationProblem::invalid_draft());
    }
    // Publishing the content of the current version again records nothing new.
    if let Some(published_version) = current.current_published_version {
        if Some(current_draft.content_digest.clone())
            == rows::published_digest(&txn, id, published_version)
                .await
                .map_err(rows::other)?
        {
            txn.commit().await.map_err(rows::other)?;
            return Ok(ConfigurationMutationResult::resource(current));
        }
    }
    let next_version = current
        .current_published_version
        .map_or(1, |version| version + 1);
    reusable_resource_versions::Entity::insert(reusable_resource_versions::ActiveModel {
        canonical_document: Set(current_draft.canonical_document.clone()),
        content_digest: Set(current_draft.content_digest.clone()),
        dependencies: Set(current_draft.dependencies.clone()),
        published_by: Set(actor),
        published_at: NotSet,
        resource_id: Set(id),
        version: Set(next_version),
    })
    .exec_without_returning(&txn)
    .await
    .map_err(rows::other)?;
    let moved = reusable_resources::Entity::update_many()
        .col_expr(
            reusable_resources::Column::CurrentPublishedVersion,
            Expr::value(next_version),
        )
        .filter(reusable_resources::Column::Id.eq(id))
        .filter(reusable_resources::Column::CurrentDraftRevision.eq(expected_revision))
        .exec(&txn)
        .await
        .map_err(rows::other)?;
    if moved.rows_affected != 1 {
        let actual = rows::stored_resource(&txn, id)
            .await
            .map_err(rows::other)?
            .current_draft_revision;
        return refused(ConfigurationProblem::conflict(
            id,
            expected_revision,
            actual,
        ));
    }
    rows::audit(
        &txn,
        actor,
        project,
        "REUSABLE_RESOURCE_PUBLISHED",
        id,
        &current_draft.content_digest,
        &format!("v{next_version}"),
    )
    .await
    .map_err(rows::other)?;
    let value = rows::stored_resource(&txn, id).await.map_err(rows::other)?;
    committed(
        db,
        txn,
        project,
        Subject::Resource(id),
        expected_revision,
        ConfigurationMutationResult::resource(value),
    )
    .await
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
) -> Result<MutationResult, RepositoryError> {
    let Some(environment_class) = approved::<LogicalEnvironmentClass>(&environment) else {
        return refused(ConfigurationProblem::invalid());
    };
    let txn = db.begin().await.map_err(rows::other)?;
    if !may_write(&txn, actor, project).await.map_err(rows::other)? {
        return refused(ConfigurationProblem::forbidden());
    }
    if !rows::catalog_definition(&txn, &definition, Some(&environment))
        .await
        .map_err(rows::other)?
    {
        return refused(ConfigurationProblem::invalid_draft());
    }
    let id = Uuid::new_v4();
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
    let inserted =
        project_tool_connections::Entity::insert(project_tool_connections::ActiveModel {
            project_id: Set(project),
            name: Set(name),
            definition_identity: Set(definition.identity.clone()),
            definition_version: Set(definition.version.clone()),
            environment: Set(environment_class),
            redacted_secret_reference: Set(rows::first_binding(&redacted_bindings)),
            lifecycle_status: Set(ConnectionLifecycleStatus::Active),
            rotation_summary: Set(String::new()),
            revision: Set(1),
            server_id: Set(Some(server_id)),
            enabled: Set(Some(enabled)),
            transport_type: Set(Some(transport_type)),
            stdio_command: Set(command),
            stdio_arguments: Set(Some(serde_json::json!(arguments))),
            remote_url: Set(remote_url),
            redacted_bindings: Set(Some(serde_json::json!(redacted_bindings))),
            declared_tools: Set(Some(serde_json::json!(tools))),
            declared_resources: Set(Some(serde_json::json!(resources))),
            declared_prompts: Set(Some(serde_json::json!(prompts))),
            id: Set(id),
        })
        .exec_without_returning(&txn)
        .await;
    match inserted {
        Ok(_) => {}
        Err(error) if rows::is_unique_violation(&error) => {
            return refused(ConfigurationProblem::invalid());
        }
        Err(error) => return Err(rows::other(error)),
    }
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
    let value = rows::stored_tool(&txn, id).await.map_err(rows::other)?;
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
) -> Result<MutationResult, RepositoryError> {
    let (Some(environment_class), Some(lifecycle)) = (
        approved::<LogicalEnvironmentClass>(&environment),
        approved::<ConnectionLifecycleStatus>(&lifecycle_status),
    ) else {
        return refused(ConfigurationProblem::invalid());
    };
    let txn = db.begin().await.map_err(rows::other)?;
    if !configuration_write(&txn, actor, project)
        .await
        .map_err(rows::other)?
    {
        return refused(ConfigurationProblem::forbidden());
    }
    let Some(existing) = rows::locked_tool(&txn, project, server)
        .await
        .map_err(rows::other)?
    else {
        return refused(ConfigurationProblem::unavailable());
    };
    if existing.revision != expected_revision {
        return refused(ConfigurationProblem::conflict(
            server,
            expected_revision,
            existing.revision,
        ));
    }
    // An archived project's server may still be archived.
    if !capability::queries::active_project(&txn, project, true)
        .await
        .map_err(rows::other)?
        && lifecycle != ConnectionLifecycleStatus::Archived
    {
        return refused(ConfigurationProblem::lifecycle());
    }
    if !rows::catalog_definition(&txn, &definition, Some(&environment))
        .await
        .map_err(rows::other)?
    {
        return refused(ConfigurationProblem::invalid_draft());
    }
    let content_digest = rows::mcp_digest(
        existing.server_id.as_deref().unwrap_or_default(),
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
    use project_tool_connections::Column;
    let updated = project_tool_connections::Entity::update_many()
        .col_expr(Column::Name, Expr::value(name))
        .col_expr(
            Column::DefinitionIdentity,
            Expr::value(definition.identity.clone()),
        )
        .col_expr(
            Column::DefinitionVersion,
            Expr::value(definition.version.clone()),
        )
        .col_expr(
            Column::Environment,
            Expr::value(environment_class.to_value()),
        )
        .col_expr(Column::Enabled, Expr::value(enabled))
        .col_expr(Column::TransportType, Expr::value(transport_type))
        .col_expr(Column::StdioCommand, Expr::value(command))
        .col_expr(
            Column::StdioArguments,
            Expr::value(serde_json::json!(arguments)),
        )
        .col_expr(Column::RemoteUrl, Expr::value(remote_url))
        .col_expr(
            Column::RedactedSecretReference,
            Expr::value(rows::first_binding(&redacted_bindings)),
        )
        .col_expr(
            Column::RedactedBindings,
            Expr::value(serde_json::json!(redacted_bindings)),
        )
        .col_expr(Column::DeclaredTools, Expr::value(serde_json::json!(tools)))
        .col_expr(
            Column::DeclaredResources,
            Expr::value(serde_json::json!(resources)),
        )
        .col_expr(
            Column::DeclaredPrompts,
            Expr::value(serde_json::json!(prompts)),
        )
        .col_expr(Column::LifecycleStatus, Expr::value(lifecycle.to_value()))
        .col_expr(Column::RotationSummary, Expr::value(String::new()))
        .col_expr(Column::Revision, Expr::col(Column::Revision).add(1))
        .filter(Column::Id.eq(server))
        .filter(Column::ProjectId.eq(project))
        .filter(Column::Revision.eq(expected_revision))
        .exec(&txn)
        .await;
    match updated {
        Ok(result) if result.rows_affected == 1 => {}
        Ok(_) => {
            let actual = rows::stored_tool(&txn, server)
                .await
                .map_err(rows::other)?
                .revision;
            return refused(ConfigurationProblem::conflict(
                server,
                expected_revision,
                actual,
            ));
        }
        Err(error) if rows::is_unique_violation(&error) => {
            return refused(ConfigurationProblem::invalid());
        }
        Err(error) => return Err(rows::other(error)),
    }
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
    let value = rows::stored_tool(&txn, server).await.map_err(rows::other)?;
    committed(
        db,
        txn,
        project,
        Subject::Tool(server),
        expected_revision,
        ConfigurationMutationResult::mcp_server(value),
    )
    .await
}

/// Saves the legacy tool metadata of an MCP server row: a new inert row when `tool` is `None`,
/// otherwise the named row at `expected_revision`. It never sets a transport.
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
) -> Result<MutationResult, RepositoryError> {
    let (Some(environment_class), Some(lifecycle_status)) = (
        approved::<LogicalEnvironmentClass>(&environment),
        approved::<ConnectionLifecycleStatus>(&lifecycle),
    ) else {
        return refused(ConfigurationProblem::invalid());
    };
    let txn = db.begin().await.map_err(rows::other)?;
    if !may_write(&txn, actor, project).await.map_err(rows::other)? {
        return refused(ConfigurationProblem::forbidden());
    }
    if !rows::catalog_definition(&txn, &definition, Some(&environment))
        .await
        .map_err(rows::other)?
    {
        return refused(ConfigurationProblem::invalid_draft());
    }
    let content_digest = digest(&format!(
        "{name}{}{environment}{redacted_secret_reference}",
        definition.value()
    ));
    let bindings = serde_json::json!([redacted_secret_reference]);
    use project_tool_connections::Column;
    let (tool_id, written) = match tool {
        Some(existing_id) => {
            let Some(existing) = rows::locked_tool(&txn, project, existing_id)
                .await
                .map_err(rows::other)?
            else {
                return refused(ConfigurationProblem::unavailable());
            };
            if existing.revision != expected_revision {
                return refused(ConfigurationProblem::conflict(
                    existing_id,
                    expected_revision,
                    existing.revision,
                ));
            }
            let updated = project_tool_connections::Entity::update_many()
                .col_expr(Column::Name, Expr::value(name))
                .col_expr(
                    Column::DefinitionIdentity,
                    Expr::value(definition.identity.clone()),
                )
                .col_expr(
                    Column::DefinitionVersion,
                    Expr::value(definition.version.clone()),
                )
                .col_expr(
                    Column::Environment,
                    Expr::value(environment_class.to_value()),
                )
                .col_expr(
                    Column::RedactedSecretReference,
                    Expr::value(redacted_secret_reference),
                )
                .col_expr(Column::RedactedBindings, Expr::value(bindings))
                .col_expr(
                    Column::LifecycleStatus,
                    Expr::value(lifecycle_status.to_value()),
                )
                .col_expr(Column::RotationSummary, Expr::value(rotation_summary))
                .col_expr(Column::Revision, Expr::value(expected_revision + 1))
                .filter(Column::Id.eq(existing_id))
                .filter(Column::Revision.eq(expected_revision))
                .exec(&txn)
                .await
                .map(|result| result.rows_affected == 1);
            (existing_id, updated)
        }
        None => {
            let id = Uuid::new_v4();
            let server_id = resource_identity::from_display_name(&name)
                .unwrap_or_else(|| "legacy-server".to_string());
            let inserted =
                project_tool_connections::Entity::insert(project_tool_connections::ActiveModel {
                    project_id: Set(project),
                    name: Set(name),
                    definition_identity: Set(definition.identity.clone()),
                    definition_version: Set(definition.version.clone()),
                    environment: Set(environment_class),
                    redacted_secret_reference: Set(redacted_secret_reference),
                    lifecycle_status: Set(lifecycle_status),
                    rotation_summary: Set(rotation_summary),
                    revision: Set(1),
                    server_id: Set(Some(server_id)),
                    redacted_bindings: Set(Some(bindings)),
                    id: Set(id),
                    ..Default::default()
                })
                .exec_without_returning(&txn)
                .await
                .map(|_| true);
            (id, inserted)
        }
    };
    match written {
        Ok(true) => {}
        Ok(false) => {
            let actual = rows::stored_tool(&txn, tool_id)
                .await
                .map_err(rows::other)?
                .revision;
            return refused(ConfigurationProblem::conflict(
                tool_id,
                expected_revision,
                actual,
            ));
        }
        Err(error) if rows::is_unique_violation(&error) => {
            return refused(ConfigurationProblem::invalid());
        }
        Err(error) => return Err(rows::other(error)),
    }
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
    let value = rows::stored_tool(&txn, tool_id)
        .await
        .map_err(rows::other)?;
    committed(
        db,
        txn,
        project,
        Subject::Tool(tool_id),
        expected_revision,
        ConfigurationMutationResult::mcp_server(value),
    )
    .await
}
