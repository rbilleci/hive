//! The rows the administration commands lock and write, one typed path per scope: an
//! organization's memberships live in `organization_memberships` / `organization_membership_roles`
//! and a project's in `project_memberships` / `project_membership_roles`, so every helper here
//! matches on the scope and works on that scope's entities.

use crate::audit::context::request_metadata;
use crate::entity::enums::{
    AdministrationScopeType, LifecycleStatus, OrganizationRoleCode, ProjectRoleCode,
};
use crate::entity::{
    administration_audit_events, organization_membership_roles, organization_memberships,
    organizations, project_approval_policies, project_approval_policy_versions,
    project_budget_policies, project_budget_policy_versions, project_membership_roles,
    project_memberships, project_settings_connections, projects,
};
use hive_application::administration::rules::{digest, matrix_json, parse_matrix};
use hive_application::administration::{
    AdministrationRepositoryError as RepositoryError, AdministrationScope, ApprovalRule,
};
use sea_orm::sea_query::{Expr, ExprTrait, Query};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, NotSet, QueryFilter, QuerySelect,
    Set,
};
use std::collections::BTreeMap;
use uuid::Uuid;

pub fn other(error: DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

pub fn is_unique_violation(error: &DbErr) -> bool {
    crate::retry::is_unique_violation_db(error)
}

/// `started_at <= CURRENT_TIMESTAMP`, on the database clock.
pub fn started<C: ColumnTrait>(started_at: C) -> sea_orm::sea_query::SimpleExpr {
    Expr::col(started_at.as_column_ref()).lte(Expr::current_timestamp())
}

pub fn scope_name(scope: AdministrationScope) -> &'static str {
    match scope {
        AdministrationScope::Organization => "ORGANIZATION",
        AdministrationScope::Project => "PROJECT",
    }
}

fn scope_type(scope: AdministrationScope) -> AdministrationScopeType {
    match scope {
        AdministrationScope::Organization => AdministrationScopeType::Organization,
        AdministrationScope::Project => AdministrationScopeType::Project,
    }
}

/// The organization or project row a command works on, locked `FOR UPDATE`.
pub struct ScopeRow {
    pub slug: String,
    pub status: LifecycleStatus,
    pub revision: i64,
}

impl ScopeRow {
    pub fn active(&self) -> bool {
        self.status == LifecycleStatus::Active
    }

    /// The text an audit digest is taken over.
    pub fn facts(&self) -> String {
        format!(
            "slug={} status={} revision={}",
            self.slug,
            self.status.to_value(),
            self.revision
        )
    }
}

/// Locks the scope row `FOR UPDATE`. Every administration command takes this lock first, before
/// the evaluator locks the actor's memberships, so two commands on one scope queue here and never
/// wait on each other's membership locks while holding a share of this row.
pub async fn locked_scope_row(
    db: &impl ConnectionTrait,
    scope: AdministrationScope,
    id: Uuid,
) -> Result<Option<ScopeRow>, DbErr> {
    Ok(match scope {
        AdministrationScope::Organization => organizations::Entity::find_by_id(id)
            .lock_exclusive()
            .one(db)
            .await?
            .map(|row| ScopeRow {
                slug: row.slug,
                status: row.lifecycle_status,
                revision: row.revision.unwrap_or_default(),
            }),
        AdministrationScope::Project => projects::Entity::find_by_id(id)
            .lock_exclusive()
            .one(db)
            .await?
            .map(|row| ScopeRow {
                slug: row.slug,
                status: row.lifecycle_status,
                revision: row.revision.unwrap_or_default(),
            }),
    })
}

/// Moves the scope row to `status` and its next revision, guarded by the revision the command
/// read. `false` when another writer got there first.
pub async fn update_lifecycle(
    db: &impl ConnectionTrait,
    scope: AdministrationScope,
    id: Uuid,
    expected_revision: i64,
    status: LifecycleStatus,
) -> Result<bool, DbErr> {
    let updated = match scope {
        AdministrationScope::Organization => {
            organizations::Entity::update_many()
                .col_expr(
                    organizations::Column::LifecycleStatus,
                    Expr::value(status.to_value()),
                )
                .col_expr(
                    organizations::Column::Revision,
                    Expr::col(organizations::Column::Revision).add(1),
                )
                .filter(organizations::Column::Id.eq(id))
                .filter(organizations::Column::Revision.eq(expected_revision))
                .exec(db)
                .await?
        }
        AdministrationScope::Project => {
            projects::Entity::update_many()
                .col_expr(
                    projects::Column::LifecycleStatus,
                    Expr::value(status.to_value()),
                )
                .col_expr(
                    projects::Column::Revision,
                    Expr::col(projects::Column::Revision).add(1),
                )
                .filter(projects::Column::Id.eq(id))
                .filter(projects::Column::Revision.eq(expected_revision))
                .exec(db)
                .await?
        }
    };
    Ok(updated.rows_affected == 1)
}

pub struct LockedMembership {
    pub revision: i64,
    pub ended: bool,
    pub principal_id: Uuid,
}

/// Locks one membership of the scope `FOR UPDATE`.
pub async fn locked_membership(
    db: &impl ConnectionTrait,
    scope: AdministrationScope,
    scope_id: Uuid,
    membership_id: Uuid,
) -> Result<Option<LockedMembership>, DbErr> {
    Ok(match scope {
        AdministrationScope::Organization => {
            organization_memberships::Entity::find_by_id(membership_id)
                .filter(organization_memberships::Column::OrganizationId.eq(scope_id))
                .lock_exclusive()
                .one(db)
                .await?
                .map(|row| LockedMembership {
                    revision: row.revision.unwrap_or_default(),
                    ended: row.ended_at.is_some(),
                    principal_id: row.principal_id,
                })
        }
        AdministrationScope::Project => project_memberships::Entity::find_by_id(membership_id)
            .filter(project_memberships::Column::ProjectId.eq(scope_id))
            .lock_exclusive()
            .one(db)
            .await?
            .map(|row| LockedMembership {
                revision: row.revision,
                ended: row.ended_at.is_some(),
                principal_id: row.principal_id,
            }),
    })
}

/// Inserts an active membership that starts now, on the database clock: a membership counts from
/// `started_at <= CURRENT_TIMESTAMP`, so the start is the database's instant, not this process's.
pub async fn insert_membership(
    db: &impl ConnectionTrait,
    scope: AdministrationScope,
    scope_id: Uuid,
    membership_id: Uuid,
    member: Uuid,
) -> Result<(), DbErr> {
    let mut insert = Query::insert();
    match scope {
        AdministrationScope::Organization => insert
            .into_table(organization_memberships::Entity)
            .columns([
                organization_memberships::Column::Id,
                organization_memberships::Column::OrganizationId,
                organization_memberships::Column::PrincipalId,
                organization_memberships::Column::StartedAt,
                organization_memberships::Column::Revision,
                organization_memberships::Column::ActiveMarker,
            ]),
        AdministrationScope::Project => insert.into_table(project_memberships::Entity).columns([
            project_memberships::Column::Id,
            project_memberships::Column::ProjectId,
            project_memberships::Column::PrincipalId,
            project_memberships::Column::StartedAt,
            project_memberships::Column::Revision,
            project_memberships::Column::ActiveMarker,
        ]),
    };
    insert
        .values([
            Expr::val(membership_id),
            Expr::val(scope_id),
            Expr::val(member),
            Expr::current_timestamp(),
            Expr::val(1_i64),
            Expr::val(true),
        ])
        .map_err(|error| DbErr::Custom(error.to_string()))?;
    db.execute(&insert).await?;
    Ok(())
}

/// Moves a membership to its next revision, guarded by the revision the command read.
pub async fn bump_membership(
    db: &impl ConnectionTrait,
    scope: AdministrationScope,
    membership_id: Uuid,
    expected_revision: i64,
) -> Result<bool, DbErr> {
    let updated = match scope {
        AdministrationScope::Organization => {
            organization_memberships::Entity::update_many()
                .col_expr(
                    organization_memberships::Column::Revision,
                    Expr::col(organization_memberships::Column::Revision).add(1),
                )
                .filter(organization_memberships::Column::Id.eq(membership_id))
                .filter(organization_memberships::Column::Revision.eq(expected_revision))
                .exec(db)
                .await?
        }
        AdministrationScope::Project => {
            project_memberships::Entity::update_many()
                .col_expr(
                    project_memberships::Column::Revision,
                    Expr::col(project_memberships::Column::Revision).add(1),
                )
                .filter(project_memberships::Column::Id.eq(membership_id))
                .filter(project_memberships::Column::Revision.eq(expected_revision))
                .exec(db)
                .await?
        }
    };
    Ok(updated.rows_affected == 1)
}

/// Ends a membership now, on the database clock, guarded by the revision the command read.
pub async fn end_membership(
    db: &impl ConnectionTrait,
    scope: AdministrationScope,
    membership_id: Uuid,
    expected_revision: i64,
) -> Result<bool, DbErr> {
    let no_marker: Option<bool> = None;
    let updated = match scope {
        AdministrationScope::Organization => {
            organization_memberships::Entity::update_many()
                .col_expr(
                    organization_memberships::Column::EndedAt,
                    Expr::current_timestamp(),
                )
                .col_expr(
                    organization_memberships::Column::Revision,
                    Expr::col(organization_memberships::Column::Revision).add(1),
                )
                .col_expr(
                    organization_memberships::Column::ActiveMarker,
                    Expr::val(no_marker),
                )
                .filter(organization_memberships::Column::Id.eq(membership_id))
                .filter(organization_memberships::Column::Revision.eq(expected_revision))
                .exec(db)
                .await?
        }
        AdministrationScope::Project => {
            project_memberships::Entity::update_many()
                .col_expr(
                    project_memberships::Column::EndedAt,
                    Expr::current_timestamp(),
                )
                .col_expr(
                    project_memberships::Column::Revision,
                    Expr::col(project_memberships::Column::Revision).add(1),
                )
                .col_expr(
                    project_memberships::Column::ActiveMarker,
                    Expr::val(no_marker),
                )
                .filter(project_memberships::Column::Id.eq(membership_id))
                .filter(project_memberships::Column::Revision.eq(expected_revision))
                .exec(db)
                .await?
        }
    };
    Ok(updated.rows_affected == 1)
}

/// A membership's role codes, sorted.
pub async fn current_roles(
    db: &impl ConnectionTrait,
    scope: AdministrationScope,
    membership_id: Uuid,
) -> Result<Vec<String>, DbErr> {
    let mut roles: Vec<String> = match scope {
        AdministrationScope::Organization => organization_membership_roles::Entity::find()
            .filter(organization_membership_roles::Column::MembershipId.eq(membership_id))
            .all(db)
            .await?
            .into_iter()
            .map(|row| row.role_code.to_value())
            .collect(),
        AdministrationScope::Project => project_membership_roles::Entity::find()
            .filter(project_membership_roles::Column::MembershipId.eq(membership_id))
            .all(db)
            .await?
            .into_iter()
            .map(|row| row.role_code.to_value())
            .collect(),
    };
    roles.sort();
    Ok(roles)
}

/// The role codes as the scope's enum, or `None` when one is not a role of that scope.
fn approved_roles<E: ActiveEnum<Value = String>>(roles: &[String]) -> Option<Vec<E>> {
    roles
        .iter()
        .map(|role| E::try_from_value(role).ok())
        .collect()
}

/// Replaces a membership's roles with `roles`. `false` when a code is not a role of the scope.
pub async fn replace_roles(
    db: &impl ConnectionTrait,
    scope: AdministrationScope,
    membership_id: Uuid,
    roles: &[String],
) -> Result<bool, DbErr> {
    match scope {
        AdministrationScope::Organization => {
            let Some(codes) = approved_roles::<OrganizationRoleCode>(roles) else {
                return Ok(false);
            };
            organization_membership_roles::Entity::delete_many()
                .filter(organization_membership_roles::Column::MembershipId.eq(membership_id))
                .exec(db)
                .await?;
            if !codes.is_empty() {
                organization_membership_roles::Entity::insert_many(codes.into_iter().map(
                    |role_code| organization_membership_roles::ActiveModel {
                        membership_id: Set(membership_id),
                        role_code: Set(role_code),
                    },
                ))
                .exec_without_returning(db)
                .await?;
            }
        }
        AdministrationScope::Project => {
            let Some(codes) = approved_roles::<ProjectRoleCode>(roles) else {
                return Ok(false);
            };
            project_membership_roles::Entity::delete_many()
                .filter(project_membership_roles::Column::MembershipId.eq(membership_id))
                .exec(db)
                .await?;
            if !codes.is_empty() {
                project_membership_roles::Entity::insert_many(codes.into_iter().map(|role_code| {
                    project_membership_roles::ActiveModel {
                        membership_id: Set(membership_id),
                        role_code: Set(role_code),
                    }
                }))
                .exec_without_returning(db)
                .await?;
            }
        }
    }
    Ok(true)
}

/// Locks one settings connection of the project `FOR UPDATE`.
pub async fn locked_settings_connection(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    connection_id: Uuid,
) -> Result<Option<project_settings_connections::Model>, DbErr> {
    project_settings_connections::Entity::find_by_id(connection_id)
        .filter(project_settings_connections::Column::ProjectId.eq(project_id))
        .lock_exclusive()
        .one(db)
        .await
}

/// Locks the project's budget policy row `FOR UPDATE`.
pub async fn locked_budget_policy(
    db: &impl ConnectionTrait,
    project_id: Uuid,
) -> Result<Option<project_budget_policies::Model>, DbErr> {
    project_budget_policies::Entity::find_by_id(project_id)
        .lock_exclusive()
        .one(db)
        .await
}

/// The budget policy version a policy row points at; none before the first version.
pub async fn budget_version(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    revision: i64,
) -> Result<Option<project_budget_policy_versions::Model>, DbErr> {
    project_budget_policy_versions::Entity::find_by_id((project_id, revision))
        .one(db)
        .await
}

pub struct CurrentApproval {
    pub id: Uuid,
    pub revision: i64,
    pub digest: String,
    pub matrix: BTreeMap<String, ApprovalRule>,
}

/// Locks the project's approval policy row `FOR UPDATE` and reads the version it points at.
pub async fn locked_current_approval(
    db: &impl ConnectionTrait,
    project_id: Uuid,
) -> Result<Option<CurrentApproval>, DbErr> {
    let Some(policy) = project_approval_policies::Entity::find()
        .filter(project_approval_policies::Column::ProjectId.eq(project_id))
        .lock_exclusive()
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let Some(version) =
        project_approval_policy_versions::Entity::find_by_id((policy.id, policy.current_revision))
            .one(db)
            .await?
    else {
        return Ok(None);
    };
    Ok(Some(CurrentApproval {
        id: policy.id,
        revision: version.revision,
        digest: version.digest,
        matrix: stored_matrix(&version.matrix)?,
    }))
}

pub fn stored_matrix(value: &serde_json::Value) -> Result<BTreeMap<String, ApprovalRule>, DbErr> {
    parse_matrix(value).map_err(|error| DbErr::Custom(format!("approval policy matrix: {error}")))
}

/// Appends a policy version. Its digest is taken over the canonical matrix text.
pub async fn insert_policy_version(
    db: &impl ConnectionTrait,
    policy_id: Uuid,
    revision: i64,
    matrix: &BTreeMap<String, ApprovalRule>,
    reason: &str,
) -> Result<String, DbErr> {
    let canonical = matrix_json(matrix);
    let canonical_digest = digest(&canonical);
    let stored: serde_json::Value = serde_json::from_str(&canonical)
        .map_err(|error| DbErr::Custom(format!("approval policy matrix: {error}")))?;
    project_approval_policy_versions::Entity::insert(
        project_approval_policy_versions::ActiveModel {
            digest: Set(canonical_digest.clone()),
            matrix: Set(stored),
            change_reason: Set(reason.to_string()),
            created_at: NotSet,
            policy_id: Set(policy_id),
            revision: Set(revision),
        },
    )
    .exec_without_returning(db)
    .await?;
    Ok(canonical_digest)
}

/// One administration audit row, written in the command's transaction.
pub struct AuditEvent<'a> {
    pub actor: Uuid,
    pub scope: AdministrationScope,
    pub scope_id: Uuid,
    pub action: &'a str,
    pub reason: Option<&'a str>,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub material: serde_json::Value,
}

pub async fn audit(db: &impl ConnectionTrait, event: AuditEvent<'_>) -> Result<(), DbErr> {
    let mut facts = serde_json::Map::new();
    facts.insert("action".to_string(), event.action.into());
    facts.insert(
        "actorPrincipalId".to_string(),
        event.actor.to_string().into(),
    );
    if let Some(after) = &event.after_digest {
        facts.insert("afterDigest".to_string(), after.clone().into());
    }
    if let Some(before) = &event.before_digest {
        facts.insert("beforeDigest".to_string(), before.clone().into());
    }
    facts.insert("material".to_string(), event.material);
    if let Some(reason) = event.reason {
        facts.insert("reason".to_string(), reason.into());
    }
    facts.insert("scopeId".to_string(), event.scope_id.to_string().into());
    facts.insert("scopeType".to_string(), scope_name(event.scope).into());

    let metadata = request_metadata();
    administration_audit_events::Entity::insert(administration_audit_events::ActiveModel {
        id: Set(Uuid::new_v4()),
        actor_principal_id: Set(event.actor),
        scope_type: Set(scope_type(event.scope)),
        scope_id: Set(event.scope_id),
        action: Set(event.action.to_string()),
        reason: Set(event.reason.map(str::to_string)),
        before_digest: Set(event.before_digest),
        after_digest: Set(event.after_digest),
        occurred_at: NotSet,
        facts: Set(serde_json::Value::Object(facts)),
        request_id: Set(metadata.request_id),
        correlation_id: Set(metadata.correlation_id),
        graphql_operation: Set(metadata.graphql_operation),
        source_ip: Set(metadata.source_ip),
        user_agent: Set(metadata.user_agent),
    })
    .exec_without_returning(db)
    .await?;
    Ok(())
}
