//! What the shared console needs beyond plain entity reads.
//!
//! `capabilities` is a computed field (`docs/idiomatic-seaography-plan.md`, A4) on the generated
//! `Organizations`, `Projects` and `Principals` objects: the capability codes the *requesting*
//! principal holds at that row's scope, answered by the capability evaluator. The console gates
//! every route and button on them. The impls live in this crate because of the orphan rule;
//! `hive-api` attaches them to the generated objects.
//!
//! The display-preferences write is a command on SeaORM entities.

#![allow(non_snake_case)] // a computed field is named after its method

use crate::administration::computed;
use crate::authority::RequestAuthority;
use crate::capability::{self, Scope};
use crate::entity::enums::{ColorScheme, DisplayDensity, SidebarState};
use crate::entity::{organizations, principal_display_preferences, principals, projects};
use hive_application::console::{
    ConsoleRepository, ConsoleRepositoryError as RepositoryError, DisplayPreferencesMutationResult,
    DisplayPreferencesProblem, UserDisplayPreferences,
};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveEnum, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, QuerySelect, Set,
    TransactionTrait,
};
// `#[CustomFields]` expands to paths that start with `async_graphql::`.
use seaography::async_graphql::{self, Context};
use seaography::CustomFields;
use std::collections::BTreeSet;
use uuid::Uuid;

/// The code groups checked at every organization and project scope.
fn scoped_code_groups() -> [&'static [&'static str]; 3] {
    [
        capability::ADMINISTRATION_CAPABILITIES,
        &[capability::AUDIT_VIEW, capability::AUDIT_SENSITIVE_VIEW],
        capability::CONFIGURATION_CAPABILITIES,
    ]
}

async fn held(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    scope: Scope,
    codes: &[&'static str],
    granted: &mut BTreeSet<&'static str>,
) -> Result<(), DbErr> {
    for &code in codes {
        if capability::has_capability(db, principal_id, code, scope, false).await? {
            granted.insert(code);
        }
    }
    Ok(())
}

/// The codes `principal_id` holds on its own principal scope.
pub async fn principal_capabilities(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    scope_id: Uuid,
) -> Result<Vec<String>, DbErr> {
    let mut granted = BTreeSet::new();
    held(
        db,
        principal_id,
        Scope::Principal(scope_id),
        &[capability::PREFERENCES_UPDATE],
        &mut granted,
    )
    .await?;
    Ok(sorted(granted))
}

/// The codes `principal_id` holds at an organization.
pub async fn organization_capabilities(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Uuid,
) -> Result<Vec<String>, DbErr> {
    let scope = Scope::Organization(organization_id);
    let mut granted = BTreeSet::new();
    held(
        db,
        principal_id,
        scope,
        &[capability::ORGANIZATION_VIEW],
        &mut granted,
    )
    .await?;
    for group in scoped_code_groups() {
        held(db, principal_id, scope, group, &mut granted).await?;
    }
    Ok(sorted(granted))
}

/// The codes `principal_id` holds at a project. Deployment and evaluation grants are taken from
/// the evaluator's per-project sets, as the console context always did.
pub async fn project_capabilities(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<Vec<String>, DbErr> {
    let scope = Scope::Project(project_id);
    let mut granted = BTreeSet::new();
    held(
        db,
        principal_id,
        scope,
        &[
            capability::PROJECT_VIEW,
            capability::AGENT_VIEW,
            capability::AGENT_DRAFT_UPDATE,
        ],
        &mut granted,
    )
    .await?;
    for group in scoped_code_groups() {
        held(db, principal_id, scope, group, &mut granted).await?;
    }
    granted.extend(capability::deployment_capabilities(db, principal_id, project_id, false).await?);
    granted.extend(capability::evaluation_capabilities(db, principal_id, project_id, false).await?);
    Ok(sorted(granted))
}

fn sorted(granted: BTreeSet<&'static str>) -> Vec<String> {
    granted.into_iter().map(str::to_string).collect()
}

/// The requesting principal and the connection, from the GraphQL request data.
pub(crate) fn requester<'a>(
    ctx: &'a Context<'_>,
) -> async_graphql::Result<(Uuid, &'a DatabaseConnection)> {
    let principal_id = ctx
        .data::<RequestAuthority>()?
        .0
        .as_ref()
        .map(|authority| authority.principal_id)
        .ok_or_else(|| async_graphql::Error::new("Access could not be determined; try again."))?;
    Ok((principal_id, ctx.data::<DatabaseConnection>()?))
}

#[CustomFields]
impl principals::Model {
    /// The capability codes the requesting principal holds on this principal.
    pub async fn capabilities(&self, ctx: &Context<'_>) -> async_graphql::Result<Vec<String>> {
        let (principal_id, db) = requester(ctx)?;
        Ok(principal_capabilities(db, principal_id, self.id).await?)
    }
}

#[CustomFields]
impl organizations::Model {
    /// The capability codes the requesting principal holds at this organization.
    pub async fn capabilities(&self, ctx: &Context<'_>) -> async_graphql::Result<Vec<String>> {
        let (principal_id, db) = requester(ctx)?;
        Ok(organization_capabilities(db, principal_id, self.id).await?)
    }

    /// The organization role codes an administrator may assign.
    pub async fn assignableRoles(&self, _ctx: &Context<'_>) -> async_graphql::Result<Vec<String>> {
        Ok(computed::assignable_organization_roles().await)
    }

    /// The principals a membership of this organization can be added for: those that hold or
    /// held one. Empty unless the requesting principal holds `ORGANIZATION_MEMBERSHIP.VIEW` here.
    pub async fn availablePrincipals(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<principals::Model>> {
        let (principal_id, db) = requester(ctx)?;
        Ok(computed::available_organization_principals(db, principal_id, self.id).await?)
    }
}

#[CustomFields]
impl projects::Model {
    /// The capability codes the requesting principal holds at this project.
    pub async fn capabilities(&self, ctx: &Context<'_>) -> async_graphql::Result<Vec<String>> {
        let (principal_id, db) = requester(ctx)?;
        Ok(project_capabilities(db, principal_id, self.id).await?)
    }

    /// The project role codes the requesting principal may assign. Only a platform
    /// administrator is offered `DEPLOYMENT_APPROVER`.
    pub async fn assignableRoles(&self, ctx: &Context<'_>) -> async_graphql::Result<Vec<String>> {
        let (principal_id, db) = requester(ctx)?;
        Ok(computed::assignable_project_roles(db, principal_id).await?)
    }

    /// The principals a membership of this project can be added for: those that hold or held a
    /// membership of the owning organization. Empty unless the requesting principal holds
    /// `PROJECT_MEMBERSHIP.VIEW` here.
    pub async fn availablePrincipals(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<principals::Model>> {
        let (principal_id, db) = requester(ctx)?;
        Ok(computed::available_project_principals(db, principal_id, self).await?)
    }
}

fn other(error: DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

fn approved<E: ActiveEnum<Value = String>>(value: &str) -> Result<E, RepositoryError> {
    E::try_from_value(&value.to_string()).map_err(other)
}

pub struct PgConsoleRepository {
    db: DatabaseConnection,
}

impl PgConsoleRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl ConsoleRepository for PgConsoleRepository {
    async fn update_preferences(
        &self,
        principal_id: Uuid,
        color_scheme: &str,
        density: &str,
        sidebar_state: &str,
    ) -> Result<DisplayPreferencesMutationResult, RepositoryError> {
        let row = principal_display_preferences::ActiveModel {
            color_scheme: Set(approved::<ColorScheme>(color_scheme)?),
            density: Set(approved::<DisplayDensity>(density)?),
            sidebar_state: Set(Some(approved::<SidebarState>(sidebar_state)?)),
            principal_id: Set(principal_id),
        };

        let tx = self.db.begin().await.map_err(other)?;
        let principal = principals::Entity::find_by_id(principal_id)
            .lock_exclusive()
            .one(&tx)
            .await
            .map_err(other)?;
        if principal.is_none() {
            tx.rollback().await.map_err(other)?;
            return Ok(DisplayPreferencesMutationResult::refused(
                DisplayPreferencesProblem::NotFound,
            ));
        }
        principal_display_preferences::Entity::insert(row)
            .on_conflict(
                OnConflict::column(principal_display_preferences::Column::PrincipalId)
                    .update_columns([
                        principal_display_preferences::Column::ColorScheme,
                        principal_display_preferences::Column::Density,
                        principal_display_preferences::Column::SidebarState,
                    ])
                    .to_owned(),
            )
            .exec_without_returning(&tx)
            .await
            .map_err(other)?;
        tx.commit().await.map_err(other)?;

        Ok(DisplayPreferencesMutationResult::success(
            UserDisplayPreferences {
                principal_id,
                color_scheme: color_scheme.to_string(),
                density: density.to_string(),
                sidebar_state: sidebar_state.to_string(),
            },
        ))
    }
}
