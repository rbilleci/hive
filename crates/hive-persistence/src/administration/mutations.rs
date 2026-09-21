//! The administration commands (`createProject`, `addAdministrationMembership`,
//! `replaceAdministrationMembershipRoles`, `endAdministrationMembership`,
//! `archiveAdministrationScope`, `restoreAdministrationScope`, `updateProjectBudgetPolicy`,
//! `updateProjectApprovalPolicy`, `updateProjectGeneral`, `saveProjectSettingsConnection`) on
//! SeaORM entities.
//!
//! Each runs in one transaction and takes its locks in one order:
//!
//! 1. the scope row (the organization or the project), `FOR UPDATE`;
//! 2. the actor's authority, through the evaluator's locking checks (platform assignment,
//!    organization memberships and roles, project memberships and roles);
//! 3. the row the command changes (a membership, the budget or approval policy, a connection).
//!
//! The evaluator itself locks the scope row (`FOR KEY SHARE`) before the memberships, so taking
//! the scope row first here means two commands on one scope queue on that row. Taking it after the
//! capability check, as this module once did, let two transactions each hold a key share of the
//! row and one hold the membership locks the other waited for, which Postgres ended as a deadlock.
//!
//! Every guarded update carries the revision it read in its `WHERE` clause and checks the row
//! count. A command answers with the stored `organizations` or `projects` row; the GraphQL payload
//! exposes it as the generated type the reads use. A refusal returns before `commit`, and the
//! dropped transaction rolls back.

use super::rows::{self, AuditEvent, ScopeRow};
use super::scopes;
use crate::capability::{self, queries::organization_memberships_of_project, Scope};
use crate::entity::enums::{
    ConnectionLifecycleStatus, CredentialStatus, LifecycleStatus, LogicalEnvironmentClass,
};
use crate::entity::{
    organization_memberships, organizations, principals, project_approval_policies,
    project_budget_policies, project_budget_policy_versions, project_settings_connections,
    projects,
};
use crate::error::repository_error;
use hive_application::administration::rules::{
    canonical_roles, changes_deployment_approver, default_matrix, digest, matrix_json,
    safety_reducing, weakens,
};
use hive_application::administration::{
    AdministrationMutationResult, AdministrationProblem, AdministrationScope, ApprovalRule,
};
use hive_application::RepositoryError;
use sea_orm::sea_query::{Expr, ExprTrait, LockType};
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait, JoinType,
    NotSet, QueryFilter, QuerySelect, Set, TransactionTrait,
};
use std::collections::BTreeMap;
use std::sync::LazyLock;
use uuid::Uuid;

pub type MutationResult = AdministrationMutationResult<organizations::Model, projects::Model>;

static SLUG_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").unwrap());

const INITIAL_POLICY_REASON: &str = "Initial fixed local P-05 policy";

fn refused(problem: AdministrationProblem) -> Result<MutationResult, RepositoryError> {
    Ok(AdministrationMutationResult::refused(problem))
}

fn approved<E: ActiveEnum<Value = String>>(value: &str) -> Option<E> {
    E::try_from_value(&value.to_string()).ok()
}

fn capability_scope(scope: AdministrationScope, scope_id: Uuid) -> Scope {
    match scope {
        AdministrationScope::Organization => Scope::Organization(scope_id),
        AdministrationScope::Project => Scope::Project(scope_id),
    }
}

/// Locks the scope row, then checks `capability` under the evaluator's locks. `Err` is the
/// refusal: `FORBIDDEN` without the capability (which a missing scope also answers, so a caller
/// learns nothing about scopes it may not administer), `NOT_FOUND` otherwise.
async fn authorized_scope(
    txn: &impl ConnectionTrait,
    actor: Uuid,
    capability: &str,
    scope: AdministrationScope,
    scope_id: Uuid,
) -> Result<Result<ScopeRow, AdministrationProblem>, DbErr> {
    let row = rows::locked_scope_row(txn, scope, scope_id).await?;
    let granted = capability::has_capability(
        txn,
        actor,
        capability,
        capability_scope(scope, scope_id),
        true,
    )
    .await?;
    if !granted {
        return Ok(Err(AdministrationProblem::forbidden()));
    }
    Ok(row.ok_or_else(AdministrationProblem::unavailable))
}

/// Only a platform administrator may grant or remove `DEPLOYMENT_APPROVER`. The assignment is
/// locked `FOR UPDATE`, so the answer holds until the command commits.
async fn approval_role_transition_allowed(
    txn: &impl ConnectionTrait,
    actor: Uuid,
    scope: AdministrationScope,
    previous: &[String],
    next: &[String],
) -> Result<bool, DbErr> {
    if scope != AdministrationScope::Project || !changes_deployment_approver(previous, next) {
        return Ok(true);
    }
    capability::queries::has_platform_admin(txn, actor, true).await
}

/// Whether `member` is an active member of the organization that owns the project. Both rows are
/// locked `FOR KEY SHARE`.
async fn project_organization_membership_exists(
    txn: &impl ConnectionTrait,
    project_id: Uuid,
    member: Uuid,
) -> Result<bool, DbErr> {
    let rows = projects::Entity::find()
        .select_only()
        .column(projects::Column::Id)
        .join(JoinType::InnerJoin, organization_memberships_of_project())
        .filter(projects::Column::Id.eq(project_id))
        .filter(organization_memberships::Column::PrincipalId.eq(member))
        .filter(
            Expr::col((
                organization_memberships::Entity,
                organization_memberships::Column::StartedAt,
            ))
            .lte(Expr::current_timestamp()),
        )
        .filter(organization_memberships::Column::EndedAt.is_null())
        .lock(LockType::KeyShare)
        .into_tuple::<Uuid>()
        .all(txn)
        .await?;
    Ok(!rows.is_empty())
}

async fn stored_organization(
    db: &DatabaseConnection,
    id: Uuid,
) -> Result<MutationResult, RepositoryError> {
    let row = organizations::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(repository_error)?
        .ok_or_else(|| repository_error(DbErr::RecordNotFound(format!("organization {id}"))))?;
    Ok(AdministrationMutationResult::organization(row))
}

async fn stored_project(
    db: &DatabaseConnection,
    id: Uuid,
) -> Result<MutationResult, RepositoryError> {
    let row = projects::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(repository_error)?
        .ok_or_else(|| repository_error(DbErr::RecordNotFound(format!("project {id}"))))?;
    Ok(AdministrationMutationResult::project(row))
}

/// The scope row as the command left it, read after the commit.
async fn stored_scope(
    db: &DatabaseConnection,
    scope: AdministrationScope,
    scope_id: Uuid,
) -> Result<MutationResult, RepositoryError> {
    match scope {
        AdministrationScope::Organization => stored_organization(db, scope_id).await,
        AdministrationScope::Project => stored_project(db, scope_id).await,
    }
}

/// A guarded update that matched no row lost to a concurrent writer. The row lock this command
/// holds rules that out on PostgreSQL; where it can happen, the winner left at least the next
/// revision behind, and that is what the conflict reports.
fn lost_update(resource_id: Uuid, expected_revision: i64) -> AdministrationProblem {
    AdministrationProblem::conflict(
        resource_id.to_string(),
        expected_revision,
        expected_revision + 1,
    )
}

pub async fn create_project(
    db: &DatabaseConnection,
    actor: Uuid,
    organization_id: Uuid,
    expected_revision: i64,
    slug: String,
    display_name: String,
    description: Option<String>,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(repository_error)?;
    let organization = match authorized_scope(
        &txn,
        actor,
        "PROJECT.CREATE",
        AdministrationScope::Organization,
        organization_id,
    )
    .await
    .map_err(repository_error)?
    {
        Ok(organization) => organization,
        Err(problem) => return refused(problem),
    };
    if organization.revision != expected_revision {
        return refused(AdministrationProblem::conflict(
            organization_id.to_string(),
            expected_revision,
            organization.revision,
        ));
    }
    if !organization.active() {
        return refused(AdministrationProblem::protected_lifecycle());
    }
    if slug.trim().is_empty() || display_name.trim().is_empty() || !SLUG_PATTERN.is_match(&slug) {
        return refused(AdministrationProblem::invalid());
    }

    let project_id = Uuid::new_v4();
    let display_name = display_name.trim().to_string();
    let inserted = projects::Entity::insert(projects::ActiveModel {
        organization_id: Set(organization_id),
        slug: Set(slug.clone()),
        display_name: Set(display_name.clone()),
        lifecycle_status: Set(LifecycleStatus::Active),
        revision: Set(Some(1)),
        description: Set(Some(description.unwrap_or_default().trim().to_string())),
        id: Set(project_id),
    })
    .exec_without_returning(&txn)
    .await;
    match inserted {
        Ok(_) => {}
        Err(error) if rows::is_unique_violation(&error) => {
            return refused(AdministrationProblem::invalid());
        }
        Err(error) => return Err(repository_error(error)),
    }
    project_budget_policies::Entity::insert(project_budget_policies::ActiveModel {
        current_revision: NotSet,
        project_id: Set(project_id),
    })
    .exec_without_returning(&txn)
    .await
    .map_err(repository_error)?;
    // A project's approval policy shares the project's id.
    project_approval_policies::Entity::insert(project_approval_policies::ActiveModel {
        project_id: Set(project_id),
        current_revision: Set(1),
        id: Set(project_id),
    })
    .exec_without_returning(&txn)
    .await
    .map_err(repository_error)?;
    let policy_digest = rows::insert_policy_version(
        &txn,
        project_id,
        1,
        &default_matrix(),
        INITIAL_POLICY_REASON,
    )
    .await
    .map_err(repository_error)?;
    rows::audit(
        &txn,
        AuditEvent {
            actor,
            scope: AdministrationScope::Project,
            scope_id: project_id,
            action: "PROJECT_APPROVAL_POLICY_CREATED",
            reason: Some(INITIAL_POLICY_REASON),
            before_digest: None,
            after_digest: Some(policy_digest),
            material: serde_json::json!({"revision": 1, "policyId": project_id.to_string()}),
        },
    )
    .await
    .map_err(repository_error)?;
    rows::audit(
        &txn,
        AuditEvent {
            actor,
            scope: AdministrationScope::Project,
            scope_id: project_id,
            action: "PROJECT_CREATED",
            reason: None,
            before_digest: None,
            after_digest: Some(digest(&project_id.to_string())),
            material: serde_json::json!({"slug": slug, "displayName": display_name}),
        },
    )
    .await
    .map_err(repository_error)?;

    txn.commit().await.map_err(repository_error)?;
    stored_project(db, project_id).await
}

pub async fn add_membership(
    db: &DatabaseConnection,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
    member: Uuid,
    role_codes: Vec<String>,
    expected_scope_revision: i64,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(repository_error)?;
    let capability_code = format!("{}_MEMBERSHIP.ADD", rows::scope_name(scope));
    let owner = match authorized_scope(&txn, actor, &capability_code, scope, scope_id)
        .await
        .map_err(repository_error)?
    {
        Ok(owner) => owner,
        Err(problem) => return refused(problem),
    };
    if owner.revision != expected_scope_revision {
        return refused(AdministrationProblem::conflict(
            scope_id.to_string(),
            expected_scope_revision,
            owner.revision,
        ));
    }
    if scope != AdministrationScope::Organization && !owner.active() {
        return refused(AdministrationProblem::protected_lifecycle());
    }
    let known = principals::Entity::find_by_id(member)
        .one(&txn)
        .await
        .map_err(repository_error)?
        .is_some();
    if !known {
        return refused(AdministrationProblem::unavailable());
    }
    if scope == AdministrationScope::Project
        && !project_organization_membership_exists(&txn, scope_id, member)
            .await
            .map_err(repository_error)?
    {
        return refused(AdministrationProblem::invalid());
    }
    if !approval_role_transition_allowed(&txn, actor, scope, &[], &role_codes)
        .await
        .map_err(repository_error)?
    {
        return refused(AdministrationProblem::forbidden());
    }

    let membership_id = Uuid::new_v4();
    match rows::insert_membership(&txn, scope, scope_id, membership_id, member).await {
        Ok(()) => {}
        // The member already has an active membership of this scope.
        Err(error) if rows::is_unique_violation(&error) => {
            return refused(AdministrationProblem::invalid());
        }
        Err(error) => return Err(repository_error(error)),
    }
    if !rows::replace_roles(&txn, scope, membership_id, &role_codes)
        .await
        .map_err(repository_error)?
    {
        return refused(AdministrationProblem::invalid());
    }
    scopes::refresh_membership_scope(&txn, scope, member, scope_id)
        .await
        .map_err(repository_error)?;
    rows::audit(
        &txn,
        AuditEvent {
            actor,
            scope,
            scope_id,
            action: &format!("{}_MEMBERSHIP_ADDED", rows::scope_name(scope)),
            reason: None,
            before_digest: None,
            after_digest: Some(digest(&canonical_roles(&role_codes))),
            material: serde_json::json!({
                "membershipId": membership_id.to_string(),
                "memberPrincipalId": member.to_string(),
                "roleCodes": role_codes,
            }),
        },
    )
    .await
    .map_err(repository_error)?;

    txn.commit().await.map_err(repository_error)?;
    stored_scope(db, scope, scope_id).await
}

pub async fn replace_membership(
    db: &DatabaseConnection,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
    membership_id: Uuid,
    role_codes: Vec<String>,
    expected_revision: i64,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(repository_error)?;
    let capability_code = format!("{}_MEMBERSHIP.CHANGE_ROLES", rows::scope_name(scope));
    let owner = match authorized_scope(&txn, actor, &capability_code, scope, scope_id)
        .await
        .map_err(repository_error)?
    {
        Ok(owner) => owner,
        Err(problem) => return refused(problem),
    };
    let Some(membership) = rows::locked_membership(&txn, scope, scope_id, membership_id)
        .await
        .map_err(repository_error)?
    else {
        return refused(AdministrationProblem::unavailable());
    };
    if membership.revision != expected_revision {
        return refused(AdministrationProblem::conflict(
            membership_id.to_string(),
            expected_revision,
            membership.revision,
        ));
    }
    if membership.ended {
        return refused(AdministrationProblem::protected_lifecycle());
    }
    if scope == AdministrationScope::Project && !owner.active() {
        return refused(AdministrationProblem::protected_lifecycle());
    }

    let previous = rows::current_roles(&txn, scope, membership_id)
        .await
        .map_err(repository_error)?;
    if previous == role_codes {
        txn.commit().await.map_err(repository_error)?;
        return stored_scope(db, scope, scope_id).await;
    }
    if !approval_role_transition_allowed(&txn, actor, scope, &previous, &role_codes)
        .await
        .map_err(repository_error)?
    {
        return refused(AdministrationProblem::forbidden());
    }
    if !rows::replace_roles(&txn, scope, membership_id, &role_codes)
        .await
        .map_err(repository_error)?
    {
        return refused(AdministrationProblem::invalid());
    }
    scopes::refresh_role_scope(&txn, scope, membership.principal_id, scope_id)
        .await
        .map_err(repository_error)?;
    if !rows::bump_membership(&txn, scope, membership_id, expected_revision)
        .await
        .map_err(repository_error)?
    {
        return refused(lost_update(membership_id, expected_revision));
    }
    rows::audit(
        &txn,
        AuditEvent {
            actor,
            scope,
            scope_id,
            action: &format!("{}_MEMBERSHIP_ROLES_REPLACED", rows::scope_name(scope)),
            reason: None,
            before_digest: Some(digest(&canonical_roles(&previous))),
            after_digest: Some(digest(&canonical_roles(&role_codes))),
            material: serde_json::json!({
                "membershipId": membership_id.to_string(),
                "roleCodes": role_codes,
            }),
        },
    )
    .await
    .map_err(repository_error)?;

    txn.commit().await.map_err(repository_error)?;
    stored_scope(db, scope, scope_id).await
}

pub async fn end_membership(
    db: &DatabaseConnection,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
    membership_id: Uuid,
    expected_revision: i64,
    reason: String,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(repository_error)?;
    let capability_code = format!("{}_MEMBERSHIP.END", rows::scope_name(scope));
    if let Err(problem) = authorized_scope(&txn, actor, &capability_code, scope, scope_id)
        .await
        .map_err(repository_error)?
    {
        return refused(problem);
    }
    let Some(membership) = rows::locked_membership(&txn, scope, scope_id, membership_id)
        .await
        .map_err(repository_error)?
    else {
        return refused(AdministrationProblem::unavailable());
    };
    if membership.revision != expected_revision {
        return refused(AdministrationProblem::conflict(
            membership_id.to_string(),
            expected_revision,
            membership.revision,
        ));
    }
    if membership.ended {
        return refused(AdministrationProblem::protected_lifecycle());
    }
    let previous = rows::current_roles(&txn, scope, membership_id)
        .await
        .map_err(repository_error)?;
    if !approval_role_transition_allowed(&txn, actor, scope, &previous, &[])
        .await
        .map_err(repository_error)?
    {
        return refused(AdministrationProblem::forbidden());
    }
    if !rows::end_membership(&txn, scope, membership_id, expected_revision)
        .await
        .map_err(repository_error)?
    {
        return refused(lost_update(membership_id, expected_revision));
    }
    scopes::refresh_membership_scope(&txn, scope, membership.principal_id, scope_id)
        .await
        .map_err(repository_error)?;
    let before = format!(
        "revision={} ended={} principalId={}",
        membership.revision, membership.ended, membership.principal_id
    );
    rows::audit(
        &txn,
        AuditEvent {
            actor,
            scope,
            scope_id,
            action: &format!("{}_MEMBERSHIP_ENDED", rows::scope_name(scope)),
            reason: Some(&reason),
            before_digest: Some(digest(&before)),
            after_digest: None,
            material: serde_json::json!({"membershipId": membership_id.to_string()}),
        },
    )
    .await
    .map_err(repository_error)?;

    txn.commit().await.map_err(repository_error)?;
    stored_scope(db, scope, scope_id).await
}

#[allow(clippy::too_many_arguments)]
pub async fn lifecycle(
    db: &DatabaseConnection,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
    expected_revision: i64,
    reason: Option<String>,
    confirmation: Option<String>,
    archive: bool,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(repository_error)?;
    let capability_code = format!(
        "{}.{}",
        rows::scope_name(scope),
        if archive { "ARCHIVE" } else { "RESTORE" }
    );
    let current = match authorized_scope(&txn, actor, &capability_code, scope, scope_id)
        .await
        .map_err(repository_error)?
    {
        Ok(current) => current,
        Err(problem) => return refused(problem),
    };
    if current.revision != expected_revision {
        return refused(AdministrationProblem::conflict(
            scope_id.to_string(),
            expected_revision,
            current.revision,
        ));
    }
    let (from, next) = if archive {
        (LifecycleStatus::Active, LifecycleStatus::Archived)
    } else {
        (LifecycleStatus::Archived, LifecycleStatus::Active)
    };
    if current.status != from {
        return refused(AdministrationProblem::protected_lifecycle());
    }
    // Archiving an organization needs its slug typed back.
    if archive
        && scope == AdministrationScope::Organization
        && confirmation.as_deref().map(str::trim) != Some(current.slug.as_str())
    {
        return refused(AdministrationProblem::protected_lifecycle());
    }

    if !rows::update_lifecycle(&txn, scope, scope_id, expected_revision, next)
        .await
        .map_err(repository_error)?
    {
        return refused(lost_update(scope_id, expected_revision));
    }
    if archive && scope == AdministrationScope::Project {
        scopes::invalidate_pending_approvals_for_archived_project(&txn, scope_id, actor)
            .await
            .map_err(repository_error)?;
        scopes::record_project_archive_event(&txn, scope_id, actor, current.revision + 1)
            .await
            .map_err(repository_error)?;
    }
    let next_name = next.to_value();
    rows::audit(
        &txn,
        AuditEvent {
            actor,
            scope,
            scope_id,
            action: &format!(
                "{}_{}",
                rows::scope_name(scope),
                if archive { "ARCHIVED" } else { "RESTORED" }
            ),
            reason: reason.as_deref(),
            before_digest: Some(digest(&current.facts())),
            after_digest: Some(digest(&format!("{next_name}{}", current.revision))),
            material: serde_json::json!({"lifecycleStatus": next_name}),
        },
    )
    .await
    .map_err(repository_error)?;

    txn.commit().await.map_err(repository_error)?;
    stored_scope(db, scope, scope_id).await
}

#[allow(clippy::too_many_arguments)]
pub async fn update_budget(
    db: &DatabaseConnection,
    actor: Uuid,
    project_id: Uuid,
    expected_revision: i64,
    currency: String,
    monthly_limit_cents: i32,
    warning_threshold_cents: i32,
    reason: String,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(repository_error)?;
    let project = match authorized_scope(
        &txn,
        actor,
        "PROJECT_BUDGET.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(repository_error)?
    {
        Ok(project) => project,
        Err(problem) => return refused(problem),
    };
    if !project.active() {
        return refused(AdministrationProblem::protected_lifecycle());
    }
    let Some(policy) = rows::locked_budget_policy(&txn, project_id)
        .await
        .map_err(repository_error)?
    else {
        return refused(AdministrationProblem::unavailable());
    };
    if policy.current_revision != expected_revision {
        return refused(AdministrationProblem::conflict(
            project_id.to_string(),
            expected_revision,
            policy.current_revision,
        ));
    }
    let prior = rows::budget_version(&txn, project_id, policy.current_revision)
        .await
        .map_err(repository_error)?;
    let unchanged = prior.as_ref().is_some_and(|prior| {
        prior.currency == currency
            && prior.monthly_limit_cents == monthly_limit_cents
            && prior.warning_threshold_cents == warning_threshold_cents
    });
    if unchanged {
        txn.commit().await.map_err(repository_error)?;
        return stored_project(db, project_id).await;
    }

    let next_revision = expected_revision + 1;
    project_budget_policy_versions::Entity::insert(project_budget_policy_versions::ActiveModel {
        currency: Set(currency.clone()),
        monthly_limit_cents: Set(monthly_limit_cents),
        warning_threshold_cents: Set(warning_threshold_cents),
        change_reason: Set(reason.clone()),
        created_at: NotSet,
        project_id: Set(project_id),
        revision: Set(next_revision),
    })
    .exec_without_returning(&txn)
    .await
    .map_err(repository_error)?;
    let moved = project_budget_policies::Entity::update_many()
        .col_expr(
            project_budget_policies::Column::CurrentRevision,
            Expr::val(next_revision),
        )
        .filter(project_budget_policies::Column::ProjectId.eq(project_id))
        .filter(project_budget_policies::Column::CurrentRevision.eq(expected_revision))
        .exec(&txn)
        .await
        .map_err(repository_error)?;
    if moved.rows_affected != 1 {
        return refused(lost_update(project_id, expected_revision));
    }
    let facts = |currency: &str, limit: i32, warning: i32| format!("{currency}|{limit}|{warning}");
    rows::audit(
        &txn,
        AuditEvent {
            actor,
            scope: AdministrationScope::Project,
            scope_id: project_id,
            action: "PROJECT_BUDGET_POLICY_UPDATED",
            reason: Some(&reason),
            before_digest: prior.as_ref().map(|prior| {
                digest(&facts(
                    &prior.currency,
                    prior.monthly_limit_cents,
                    prior.warning_threshold_cents,
                ))
            }),
            after_digest: Some(digest(&facts(
                &currency,
                monthly_limit_cents,
                warning_threshold_cents,
            ))),
            material: serde_json::json!({
                "revision": next_revision,
                "currency": currency,
                "monthlyLimitCents": monthly_limit_cents,
                "warningThresholdCents": warning_threshold_cents,
            }),
        },
    )
    .await
    .map_err(repository_error)?;

    txn.commit().await.map_err(repository_error)?;
    stored_project(db, project_id).await
}

pub async fn update_approval_policy(
    db: &DatabaseConnection,
    actor: Uuid,
    project_id: Uuid,
    expected_revision: i64,
    matrix: BTreeMap<String, ApprovalRule>,
    reason: String,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(repository_error)?;
    let project = match authorized_scope(
        &txn,
        actor,
        "PROJECT_APPROVAL_POLICY.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(repository_error)?
    {
        Ok(project) => project,
        Err(problem) => return refused(problem),
    };
    if !project.active() {
        return refused(AdministrationProblem::protected_lifecycle());
    }
    let Some(prior) = rows::locked_current_approval(&txn, project_id)
        .await
        .map_err(repository_error)?
    else {
        return refused(AdministrationProblem::unavailable());
    };
    if prior.revision != expected_revision {
        return refused(AdministrationProblem::conflict(
            project_id.to_string(),
            expected_revision,
            prior.revision,
        ));
    }
    if weakens(&prior.matrix, &matrix) {
        return refused(AdministrationProblem::weakening());
    }
    if digest(&matrix_json(&matrix)) == prior.digest {
        txn.commit().await.map_err(repository_error)?;
        return stored_project(db, project_id).await;
    }

    let next_revision = expected_revision + 1;
    let next_digest = rows::insert_policy_version(&txn, prior.id, next_revision, &matrix, &reason)
        .await
        .map_err(repository_error)?;
    let moved = project_approval_policies::Entity::update_many()
        .col_expr(
            project_approval_policies::Column::CurrentRevision,
            Expr::val(next_revision),
        )
        .filter(project_approval_policies::Column::Id.eq(prior.id))
        .filter(project_approval_policies::Column::CurrentRevision.eq(expected_revision))
        .exec(&txn)
        .await
        .map_err(repository_error)?;
    if moved.rows_affected != 1 {
        return refused(lost_update(project_id, expected_revision));
    }
    rows::audit(
        &txn,
        AuditEvent {
            actor,
            scope: AdministrationScope::Project,
            scope_id: project_id,
            action: "PROJECT_APPROVAL_POLICY_UPDATED",
            reason: Some(&reason),
            before_digest: Some(prior.digest.clone()),
            after_digest: Some(next_digest.clone()),
            material: serde_json::json!({
                "policyId": prior.id.to_string(),
                "revision": next_revision,
                "matrixDigest": next_digest,
            }),
        },
    )
    .await
    .map_err(repository_error)?;

    txn.commit().await.map_err(repository_error)?;
    stored_project(db, project_id).await
}

pub async fn update_project_general(
    db: &DatabaseConnection,
    actor: Uuid,
    project_id: Uuid,
    expected_revision: i64,
    display_name: String,
    description: String,
) -> Result<MutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(repository_error)?;
    let current = match authorized_scope(
        &txn,
        actor,
        "PROJECT.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(repository_error)?
    {
        Ok(current) => current,
        Err(problem) => return refused(problem),
    };
    if current.revision != expected_revision {
        return refused(AdministrationProblem::conflict(
            project_id.to_string(),
            expected_revision,
            current.revision,
        ));
    }
    if !current.active() {
        return refused(AdministrationProblem::protected_lifecycle());
    }
    let updated = projects::Entity::update_many()
        .col_expr(
            projects::Column::DisplayName,
            Expr::val(display_name.clone()),
        )
        .col_expr(
            projects::Column::Description,
            Expr::val(description.clone()),
        )
        .col_expr(
            projects::Column::Revision,
            Expr::col(projects::Column::Revision).add(1),
        )
        .filter(projects::Column::Id.eq(project_id))
        .filter(projects::Column::Revision.eq(expected_revision))
        .exec(&txn)
        .await
        .map_err(repository_error)?;
    if updated.rows_affected != 1 {
        return refused(lost_update(project_id, expected_revision));
    }
    rows::audit(
        &txn,
        AuditEvent {
            actor,
            scope: AdministrationScope::Project,
            scope_id: project_id,
            action: "PROJECT_GENERAL_UPDATED",
            reason: None,
            before_digest: Some(digest(&current.facts())),
            after_digest: Some(digest(&format!("{display_name}|{description}"))),
            material: serde_json::json!({
                "displayName": display_name,
                "description": description,
            }),
        },
    )
    .await
    .map_err(repository_error)?;

    txn.commit().await.map_err(repository_error)?;
    stored_project(db, project_id).await
}

/// The submitted connection values, as the stored enums.
struct ConnectionValues {
    display_name: String,
    definition_version: String,
    environment: LogicalEnvironmentClass,
    credential_status: CredentialStatus,
    lifecycle_status: ConnectionLifecycleStatus,
}

impl ConnectionValues {
    fn same_metadata(&self, stored: &project_settings_connections::Model) -> bool {
        stored.display_name == self.display_name
            && stored.definition_version == self.definition_version
            && stored.environment == self.environment
            && stored.credential_status == self.credential_status
    }

    /// The text an audit digest is taken over.
    fn facts(&self) -> String {
        connection_facts(
            &self.display_name,
            &self.definition_version,
            self.environment,
            self.credential_status,
            self.lifecycle_status,
        )
    }

    fn material(&self, connection_id: Uuid) -> serde_json::Value {
        serde_json::json!({
            "connectionId": connection_id.to_string(),
            "displayName": self.display_name,
            "definitionVersion": self.definition_version,
            "environment": self.environment.to_value(),
            "credentialStatus": self.credential_status.to_value(),
            "lifecycleStatus": self.lifecycle_status.to_value(),
        })
    }
}

fn connection_facts(
    display_name: &str,
    definition_version: &str,
    environment: LogicalEnvironmentClass,
    credential_status: CredentialStatus,
    lifecycle_status: ConnectionLifecycleStatus,
) -> String {
    format!(
        "{display_name}|{definition_version}|{}|{}|{}",
        environment.to_value(),
        credential_status.to_value(),
        lifecycle_status.to_value()
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn save_project_connection(
    db: &DatabaseConnection,
    actor: Uuid,
    project_id: Uuid,
    connection_id: Option<Uuid>,
    expected_revision: i64,
    display_name: String,
    definition_version: String,
    environment: String,
    credential_status: String,
    lifecycle_status: String,
) -> Result<MutationResult, RepositoryError> {
    let (Some(environment), Some(credential_status), Some(lifecycle_status)) = (
        approved::<LogicalEnvironmentClass>(&environment),
        approved::<CredentialStatus>(&credential_status),
        approved::<ConnectionLifecycleStatus>(&lifecycle_status),
    ) else {
        return refused(AdministrationProblem::invalid());
    };
    let values = ConnectionValues {
        display_name,
        definition_version,
        environment,
        credential_status,
        lifecycle_status,
    };

    let txn = db.begin().await.map_err(repository_error)?;
    let project = match authorized_scope(
        &txn,
        actor,
        "PROJECT.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(repository_error)?
    {
        Ok(project) => project,
        Err(problem) => return refused(problem),
    };

    let Some(connection_id) = connection_id else {
        if expected_revision != 0 || !project.active() {
            return refused(AdministrationProblem::protected_lifecycle());
        }
        let new_connection_id = Uuid::new_v4();
        project_settings_connections::Entity::insert(project_settings_connections::ActiveModel {
            project_id: Set(project_id),
            display_name: Set(values.display_name.clone()),
            definition_version: Set(values.definition_version.clone()),
            environment: Set(values.environment),
            credential_status: Set(values.credential_status),
            lifecycle_status: Set(values.lifecycle_status),
            revision: Set(1),
            id: Set(new_connection_id),
        })
        .exec_without_returning(&txn)
        .await
        .map_err(repository_error)?;
        rows::audit(
            &txn,
            AuditEvent {
                actor,
                scope: AdministrationScope::Project,
                scope_id: project_id,
                action: "PROJECT_CONNECTION_CREATED",
                reason: None,
                before_digest: None,
                after_digest: Some(digest(&values.facts())),
                material: values.material(new_connection_id),
            },
        )
        .await
        .map_err(repository_error)?;
        txn.commit().await.map_err(repository_error)?;
        return stored_project(db, project_id).await;
    };

    let Some(prior) = rows::locked_settings_connection(&txn, project_id, connection_id)
        .await
        .map_err(repository_error)?
    else {
        return refused(AdministrationProblem::unavailable());
    };
    if prior.revision != expected_revision {
        return refused(AdministrationProblem::conflict(
            connection_id.to_string(),
            expected_revision,
            prior.revision,
        ));
    }
    if values.same_metadata(&prior) && prior.lifecycle_status == values.lifecycle_status {
        txn.commit().await.map_err(repository_error)?;
        return stored_project(db, project_id).await;
    }
    // An archived project accepts only a move that makes the connection safer.
    if !project.active()
        && (!values.same_metadata(&prior)
            || !safety_reducing(
                &prior.lifecycle_status.to_value(),
                &values.lifecycle_status.to_value(),
            ))
    {
        return refused(AdministrationProblem::protected_lifecycle());
    }
    let updated = project_settings_connections::Entity::update_many()
        .col_expr(
            project_settings_connections::Column::DisplayName,
            Expr::val(values.display_name.clone()),
        )
        .col_expr(
            project_settings_connections::Column::DefinitionVersion,
            Expr::val(values.definition_version.clone()),
        )
        .col_expr(
            project_settings_connections::Column::Environment,
            Expr::value(values.environment.to_value()),
        )
        .col_expr(
            project_settings_connections::Column::CredentialStatus,
            Expr::value(values.credential_status.to_value()),
        )
        .col_expr(
            project_settings_connections::Column::LifecycleStatus,
            Expr::value(values.lifecycle_status.to_value()),
        )
        .col_expr(
            project_settings_connections::Column::Revision,
            Expr::col(project_settings_connections::Column::Revision).add(1),
        )
        .filter(project_settings_connections::Column::Id.eq(connection_id))
        .filter(project_settings_connections::Column::Revision.eq(expected_revision))
        .exec(&txn)
        .await
        .map_err(repository_error)?;
    if updated.rows_affected != 1 {
        return refused(lost_update(connection_id, expected_revision));
    }
    rows::audit(
        &txn,
        AuditEvent {
            actor,
            scope: AdministrationScope::Project,
            scope_id: project_id,
            action: "PROJECT_CONNECTION_UPDATED",
            reason: None,
            before_digest: Some(digest(&connection_facts(
                &prior.display_name,
                &prior.definition_version,
                prior.environment,
                prior.credential_status,
                prior.lifecycle_status,
            ))),
            after_digest: Some(digest(&values.facts())),
            material: values.material(connection_id),
        },
    )
    .await
    .map_err(repository_error)?;

    txn.commit().await.map_err(repository_error)?;
    stored_project(db, project_id).await
}
