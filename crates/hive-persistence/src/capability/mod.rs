//! The effective capability evaluator: every capability constant, every capability set, and the
//! rule that decides whether a principal holds a capability on a scope. "Current-assignment P-10
//! authority. Role labels are stored facts; this allow-list is the authority."
//!
//! Every read is a SeaORM statement over the entities (`queries`), and so is every lock (`locks`).
//! With `lock` set the evaluator locks the rows it reads (`FOR KEY SHARE` on the scope and the
//! principal, `FOR UPDATE` on assignments, memberships and roles), so the answer holds until the
//! caller's transaction ends. With `lock` unset the same statements run without a locking clause.
//!
//! A principal or scope id is never null here: every caller holds a verified principal and a
//! parsed `ID!` argument by the time it reaches this module.

mod locks;
/// `pub(crate)`, like `tx`: a command calls these primitives directly with `lock: true` and its own
/// transaction for a standalone business-rule check (for example "is this project active") that
/// `has_capability`'s capability-string dispatch does not expose on its own.
pub(crate) mod queries;
pub(crate) mod tx;

use crate::entity::enums::{OrganizationRoleCode, ProjectRoleCode};
use sea_orm::{ConnectionTrait, DbErr};
use std::collections::HashSet;
use uuid::Uuid;

pub const PREFERENCES_UPDATE: &str = "PREFERENCES.UPDATE";
pub const ORGANIZATION_VIEW: &str = "ORGANIZATION.VIEW";
pub const PROJECT_VIEW: &str = "PROJECT.VIEW";
pub const AUDIT_VIEW: &str = "AUDIT.VIEW";
pub const AUDIT_SENSITIVE_VIEW: &str = "AUDIT_SENSITIVE.VIEW";
pub const AGENT_VIEW: &str = "AGENT.VIEW";
pub const AGENT_DRAFT_UPDATE: &str = "AGENT_DRAFT.UPDATE";
pub const AGENT_DRAFT_CREATE: &str = "AGENT_DRAFT.CREATE";
pub const AGENT_DRAFT_PUBLISH: &str = "AGENT_DRAFT.PUBLISH";
pub const CATALOG_VIEW: &str = "CATALOG.VIEW";
pub const CONFIGURATION_VIEW: &str = "CONFIGURATION.VIEW";
pub const CONFIGURATION_AUTHOR: &str = "CONFIGURATION.AUTHOR";
pub const CONFIGURATION_PUBLISH: &str = "CONFIGURATION.PUBLISH";
pub const TOOL_CONNECTION_VIEW: &str = "TOOL_CONNECTION.VIEW";
pub const TOOL_CONNECTION_UPDATE: &str = "TOOL_CONNECTION.UPDATE";
pub const DEPLOYMENT_VIEW: &str = "DEPLOYMENT.VIEW";
pub const DEPLOYMENT_REQUEST: &str = "DEPLOYMENT.REQUEST";
pub const DEPLOYMENT_CANCEL: &str = "DEPLOYMENT.CANCEL";
pub const DEPLOYMENT_RETRY: &str = "DEPLOYMENT.RETRY";
pub const DEPLOYMENT_PROMOTE: &str = "DEPLOYMENT.PROMOTE";
pub const DEPLOYMENT_ROLLBACK: &str = "DEPLOYMENT.ROLLBACK";
pub const EVALUATION_DEFINITION_VIEW: &str = "EVALUATION_DEFINITION.VIEW";
pub const EVALUATION_DEFINITION_AUTHOR: &str = "EVALUATION_DEFINITION.AUTHOR";
pub const EVALUATION_DEFINITION_PUBLISH: &str = "EVALUATION_DEFINITION.PUBLISH";
pub const EVALUATION_RUN_VIEW: &str = "EVALUATION_RUN.VIEW";
pub const EVALUATION_RUN_RUN: &str = "EVALUATION_RUN.RUN";
pub const EVALUATION_RUN_CANCEL: &str = "EVALUATION_RUN.CANCEL";
pub const EVALUATION_RUN_RERUN: &str = "EVALUATION_RUN.RERUN";
pub const DEPLOYMENT_APPROVAL_VIEW: &str = "DEPLOYMENT_APPROVAL.VIEW";
pub const DEPLOYMENT_APPROVAL_DECIDE: &str = "DEPLOYMENT_APPROVAL.DECIDE";

pub const DEPLOYMENT_CAPABILITIES: &[&str] = &[
    DEPLOYMENT_VIEW,
    DEPLOYMENT_REQUEST,
    DEPLOYMENT_CANCEL,
    DEPLOYMENT_RETRY,
    DEPLOYMENT_PROMOTE,
    DEPLOYMENT_ROLLBACK,
];
pub const EVALUATION_CAPABILITIES: &[&str] = &[
    EVALUATION_DEFINITION_VIEW,
    EVALUATION_DEFINITION_AUTHOR,
    EVALUATION_DEFINITION_PUBLISH,
    EVALUATION_RUN_VIEW,
    EVALUATION_RUN_RUN,
    EVALUATION_RUN_CANCEL,
    EVALUATION_RUN_RERUN,
];
pub const CONFIGURATION_CAPABILITIES: &[&str] = &[
    CATALOG_VIEW,
    CONFIGURATION_VIEW,
    CONFIGURATION_AUTHOR,
    CONFIGURATION_PUBLISH,
    TOOL_CONNECTION_VIEW,
    TOOL_CONNECTION_UPDATE,
];
pub const ADMINISTRATION_CAPABILITIES: &[&str] = &[
    "ORGANIZATION.VIEW",
    "ORGANIZATION.CREATE",
    "ORGANIZATION.UPDATE",
    "ORGANIZATION.ARCHIVE",
    "ORGANIZATION.RESTORE",
    "ORGANIZATION_MEMBERSHIP.VIEW",
    "ORGANIZATION_MEMBERSHIP.ADD",
    "ORGANIZATION_MEMBERSHIP.CHANGE_ROLES",
    "ORGANIZATION_MEMBERSHIP.END",
    "PROJECT.VIEW",
    "PROJECT.CREATE",
    "PROJECT.UPDATE",
    "PROJECT.ARCHIVE",
    "PROJECT.RESTORE",
    "PROJECT_MEMBERSHIP.VIEW",
    "PROJECT_MEMBERSHIP.ADD",
    "PROJECT_MEMBERSHIP.CHANGE_ROLES",
    "PROJECT_MEMBERSHIP.END",
    "PROJECT_BUDGET.VIEW",
    "PROJECT_BUDGET.UPDATE",
    "PROJECT_APPROVAL_POLICY.VIEW",
    "PROJECT_APPROVAL_POLICY.UPDATE",
    "DEPLOYMENT_APPROVAL.VIEW",
    "DEPLOYMENT_APPROVAL.DECIDE",
];
pub(crate) const ORGANIZATION_ADMIN: &[&str] = &[
    "ORGANIZATION.VIEW",
    "ORGANIZATION.CREATE",
    "ORGANIZATION.UPDATE",
    "ORGANIZATION.ARCHIVE",
    "ORGANIZATION.RESTORE",
    "ORGANIZATION_MEMBERSHIP.VIEW",
    "ORGANIZATION_MEMBERSHIP.ADD",
    "ORGANIZATION_MEMBERSHIP.CHANGE_ROLES",
    "ORGANIZATION_MEMBERSHIP.END",
    "PROJECT.CREATE",
];
pub(crate) const INHERITED_ORGANIZATION_ADMIN: &[&str] = &[
    "PROJECT.VIEW",
    "PROJECT.UPDATE",
    "PROJECT.ARCHIVE",
    "PROJECT.RESTORE",
    "PROJECT_MEMBERSHIP.VIEW",
    "PROJECT_MEMBERSHIP.ADD",
    "PROJECT_MEMBERSHIP.CHANGE_ROLES",
    "PROJECT_MEMBERSHIP.END",
    "PROJECT_BUDGET.VIEW",
    "PROJECT_APPROVAL_POLICY.VIEW",
    "DEPLOYMENT_APPROVAL.VIEW",
];
pub(crate) const PROJECT_ADMIN: &[&str] = &[
    "PROJECT.VIEW",
    "PROJECT.UPDATE",
    "PROJECT.ARCHIVE",
    "PROJECT.RESTORE",
    "PROJECT_MEMBERSHIP.VIEW",
    "PROJECT_MEMBERSHIP.ADD",
    "PROJECT_MEMBERSHIP.CHANGE_ROLES",
    "PROJECT_MEMBERSHIP.END",
    "PROJECT_BUDGET.VIEW",
    "PROJECT_BUDGET.UPDATE",
    "PROJECT_APPROVAL_POLICY.VIEW",
    "PROJECT_APPROVAL_POLICY.UPDATE",
    "DEPLOYMENT_APPROVAL.VIEW",
];
pub(crate) const PROJECT_AUDITOR: &[&str] = &[
    "PROJECT.VIEW",
    "PROJECT_MEMBERSHIP.VIEW",
    "PROJECT_BUDGET.VIEW",
    "PROJECT_APPROVAL_POLICY.VIEW",
    "DEPLOYMENT_APPROVAL.VIEW",
];

/// One scope the capability evaluator recognizes. A scope type and its identifier always travel
/// together, and no fourth scope type is representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Organization(Uuid),
    Project(Uuid),
    Principal(Uuid),
}

pub async fn has_capability(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    capability: &str,
    scope: Scope,
    lock: bool,
) -> Result<bool, DbErr> {
    if capability == PREFERENCES_UPDATE {
        if let Scope::Principal(scope_id) = scope {
            return Ok(scope_id == principal_id
                && queries::known_principal(db, principal_id, lock).await?);
        }
    }
    if capability == AUDIT_VIEW || capability == AUDIT_SENSITIVE_VIEW {
        return audit_capability(db, principal_id, capability, scope, lock).await;
    }

    let (scope_type_is_project, scope_id) = match scope {
        Scope::Project(id) => (true, id),
        Scope::Organization(id) => (false, id),
        Scope::Principal(id) => (false, id),
    };

    if EVALUATION_CAPABILITIES.contains(&capability) && scope_type_is_project {
        return Ok(evaluation_capabilities(db, principal_id, scope_id, lock)
            .await?
            .contains(capability));
    }
    if capability == AGENT_VIEW && scope_type_is_project {
        return queries::project_visible(db, principal_id, scope_id, lock).await;
    }
    if capability == AGENT_DRAFT_UPDATE && scope_type_is_project {
        return queries::legacy_or_developer(db, principal_id, scope_id, lock).await;
    }
    if (capability == AGENT_DRAFT_CREATE || capability == AGENT_DRAFT_PUBLISH)
        && scope_type_is_project
    {
        return Ok(queries::active_project(db, scope_id, lock).await?
            && queries::legacy_or_developer(db, principal_id, scope_id, lock).await?);
    }
    if capability == CATALOG_VIEW {
        if let Scope::Organization(organization_id) = scope {
            return queries::organization_visible(db, principal_id, organization_id, lock).await;
        }
    }
    if (capability == CONFIGURATION_VIEW || capability == TOOL_CONNECTION_VIEW)
        && scope_type_is_project
    {
        return queries::project_visible(db, principal_id, scope_id, lock).await;
    }
    if (capability == CONFIGURATION_AUTHOR
        || capability == CONFIGURATION_PUBLISH
        || capability == TOOL_CONNECTION_UPDATE)
        && scope_type_is_project
    {
        return Ok(queries::active_project(db, scope_id, lock).await?
            && (queries::legacy_or_developer(db, principal_id, scope_id, lock).await?
                || queries::has_active_project_role(
                    db,
                    principal_id,
                    scope_id,
                    ProjectRoleCode::ProjectAdmin,
                    lock,
                )
                .await?));
    }
    if DEPLOYMENT_CAPABILITIES.contains(&capability) && scope_type_is_project {
        let grants = deployment_capabilities(db, principal_id, scope_id, lock).await?;
        return Ok(grants.contains(capability)
            && (capability == DEPLOYMENT_VIEW
                || capability == DEPLOYMENT_RETRY
                || capability == DEPLOYMENT_ROLLBACK
                || queries::active_project(db, scope_id, lock).await?));
    }
    if (capability == DEPLOYMENT_APPROVAL_VIEW || capability == DEPLOYMENT_APPROVAL_DECIDE)
        && scope_type_is_project
    {
        let grants = deployment_approval_capabilities(db, principal_id, scope_id, lock).await?;
        return Ok(grants.contains(capability)
            && (capability == DEPLOYMENT_APPROVAL_VIEW
                || queries::active_project(db, scope_id, lock).await?));
    }
    if capability == ORGANIZATION_VIEW {
        if let Scope::Organization(organization_id) = scope {
            return queries::organization_visible(db, principal_id, organization_id, lock).await;
        }
    }
    if capability == PROJECT_VIEW && scope_type_is_project {
        return queries::project_visible(db, principal_id, scope_id, lock).await;
    }

    if !ADMINISTRATION_CAPABILITIES.contains(&capability) {
        return Ok(false);
    }
    let scope_kind = match scope {
        Scope::Organization(_) => queries::ScopeKind::Organization,
        Scope::Project(_) => queries::ScopeKind::Project,
        Scope::Principal(_) => return Ok(false),
    };
    if !queries::scope_exists(db, scope_kind, scope_id, lock).await? {
        return Ok(false);
    }
    if queries::has_platform_admin(db, principal_id, lock).await? {
        return Ok(true);
    }

    if let Scope::Organization(organization_id) = scope {
        let is_admin = queries::has_active_organization_role(
            db,
            principal_id,
            organization_id,
            OrganizationRoleCode::OrganizationAdmin,
            lock,
        )
        .await?;
        let membership_visible =
            queries::active_organization_membership(db, principal_id, organization_id, lock)
                .await?;
        return Ok((is_admin && ORGANIZATION_ADMIN.contains(&capability))
            || (capability == "ORGANIZATION.VIEW" && membership_visible));
    }

    let project_id = scope_id;
    let organization_id = match queries::project_organization(db, project_id, lock).await? {
        Some(id) => id,
        None => return Ok(false),
    };
    const PROJECT_ACTIVE_REQUIRED: &[&str] = &[
        "PROJECT_MEMBERSHIP.ADD",
        "PROJECT_MEMBERSHIP.CHANGE_ROLES",
        "PROJECT_MEMBERSHIP.END",
        "PROJECT_BUDGET.UPDATE",
        "PROJECT_APPROVAL_POLICY.UPDATE",
    ];
    if PROJECT_ACTIVE_REQUIRED.contains(&capability)
        && !queries::active_project(db, project_id, lock).await?
    {
        return Ok(false);
    }
    if queries::has_active_organization_role(
        db,
        principal_id,
        organization_id,
        OrganizationRoleCode::OrganizationAdmin,
        lock,
    )
    .await?
        && INHERITED_ORGANIZATION_ADMIN.contains(&capability)
    {
        return Ok(true);
    }
    if queries::has_active_organization_role(
        db,
        principal_id,
        organization_id,
        OrganizationRoleCode::Auditor,
        lock,
    )
    .await?
        && PROJECT_AUDITOR.contains(&capability)
    {
        return Ok(true);
    }
    if queries::has_active_project_role(
        db,
        principal_id,
        project_id,
        ProjectRoleCode::ProjectAdmin,
        lock,
    )
    .await?
        && PROJECT_ADMIN.contains(&capability)
    {
        return Ok(true);
    }
    if queries::has_active_project_role(
        db,
        principal_id,
        project_id,
        ProjectRoleCode::Auditor,
        lock,
    )
    .await?
        && PROJECT_AUDITOR.contains(&capability)
    {
        return Ok(true);
    }
    if queries::has_active_project_role(
        db,
        principal_id,
        project_id,
        ProjectRoleCode::DeploymentApprover,
        lock,
    )
    .await?
        && [
            "PROJECT.VIEW",
            "PROJECT_APPROVAL_POLICY.VIEW",
            "DEPLOYMENT_APPROVAL.VIEW",
            "DEPLOYMENT_APPROVAL.DECIDE",
        ]
        .contains(&capability)
    {
        return Ok(true);
    }
    let developer_or_operator = queries::has_active_project_role(
        db,
        principal_id,
        project_id,
        ProjectRoleCode::AgentDeveloper,
        lock,
    )
    .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            ProjectRoleCode::Operator,
            lock,
        )
        .await?;
    Ok(developer_or_operator && capability == "PROJECT.VIEW")
}

async fn audit_capability(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    capability: &str,
    scope: Scope,
    lock: bool,
) -> Result<bool, DbErr> {
    let (scope_kind, scope_id) = match scope {
        Scope::Organization(id) => (queries::ScopeKind::Organization, id),
        Scope::Project(id) => (queries::ScopeKind::Project, id),
        Scope::Principal(_) => return Ok(false),
    };
    if !queries::scope_exists(db, scope_kind, scope_id, lock).await? {
        return Ok(false);
    }
    if queries::has_platform_admin(db, principal_id, lock).await? {
        return Ok(true);
    }
    if capability == AUDIT_SENSITIVE_VIEW {
        return Ok(false);
    }
    if let queries::ScopeKind::Organization = scope_kind {
        return Ok(queries::has_active_organization_role(
            db,
            principal_id,
            scope_id,
            OrganizationRoleCode::OrganizationAdmin,
            lock,
        )
        .await?
            || queries::has_active_organization_role(
                db,
                principal_id,
                scope_id,
                OrganizationRoleCode::Auditor,
                lock,
            )
            .await?);
    }
    let organization_id = match queries::project_organization(db, scope_id, lock).await? {
        Some(id) => id,
        None => return Ok(false),
    };
    if !queries::active_organization_membership(db, principal_id, organization_id, lock).await? {
        return Ok(false);
    }
    Ok(queries::has_active_organization_role(
        db,
        principal_id,
        organization_id,
        OrganizationRoleCode::OrganizationAdmin,
        lock,
    )
    .await?
        || queries::has_active_organization_role(
            db,
            principal_id,
            organization_id,
            OrganizationRoleCode::Auditor,
            lock,
        )
        .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            scope_id,
            ProjectRoleCode::ProjectAdmin,
            lock,
        )
        .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            scope_id,
            ProjectRoleCode::AgentDeveloper,
            lock,
        )
        .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            scope_id,
            ProjectRoleCode::Operator,
            lock,
        )
        .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            scope_id,
            ProjectRoleCode::DeploymentApprover,
            lock,
        )
        .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            scope_id,
            ProjectRoleCode::Auditor,
            lock,
        )
        .await?)
}

pub async fn evaluation_capabilities(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<HashSet<&'static str>, DbErr> {
    if !queries::scope_exists(db, queries::ScopeKind::Project, project_id, lock).await? {
        return Ok(HashSet::new());
    }
    if lock {
        locks::lock_project_role_authority(db, principal_id, project_id).await?;
    }
    let active = queries::active_project(db, project_id, lock).await?;
    if queries::has_platform_admin(db, principal_id, lock).await? {
        return Ok(if active {
            EVALUATION_CAPABILITIES.iter().copied().collect()
        } else {
            [EVALUATION_DEFINITION_VIEW, EVALUATION_RUN_VIEW]
                .into_iter()
                .collect()
        });
    }
    if active
        && (queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            ProjectRoleCode::ProjectAdmin,
            lock,
        )
        .await?
            || queries::has_active_project_role(
                db,
                principal_id,
                project_id,
                ProjectRoleCode::AgentDeveloper,
                lock,
            )
            .await?)
    {
        return Ok(EVALUATION_CAPABILITIES.iter().copied().collect());
    }
    let mut result = HashSet::new();
    if active
        && queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            ProjectRoleCode::Operator,
            lock,
        )
        .await?
    {
        result.extend([
            EVALUATION_RUN_VIEW,
            EVALUATION_RUN_RUN,
            EVALUATION_RUN_CANCEL,
            EVALUATION_RUN_RERUN,
        ]);
    }
    let organization = queries::project_organization(db, project_id, lock).await?;
    let mut view = queries::has_active_project_role(
        db,
        principal_id,
        project_id,
        ProjectRoleCode::Auditor,
        lock,
    )
    .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            ProjectRoleCode::DeploymentApprover,
            lock,
        )
        .await?;
    if !view {
        if let Some(organization_id) = organization {
            view = queries::has_active_organization_role(
                db,
                principal_id,
                organization_id,
                OrganizationRoleCode::Auditor,
                lock,
            )
            .await?
                || queries::has_active_organization_role(
                    db,
                    principal_id,
                    organization_id,
                    OrganizationRoleCode::OrganizationAdmin,
                    lock,
                )
                .await?;
        }
    }
    if view {
        result.extend([EVALUATION_DEFINITION_VIEW, EVALUATION_RUN_VIEW]);
    }
    Ok(result)
}

pub async fn is_platform_administrator(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
) -> Result<bool, DbErr> {
    queries::has_platform_admin(db, principal_id, false).await
}

pub async fn deployment_capabilities(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<HashSet<&'static str>, DbErr> {
    if lock {
        // The project row first, then the principal's assignments and memberships: the one lock
        // order every locked evaluation and every administration command follows.
        queries::scope_exists(db, queries::ScopeKind::Project, project_id, true).await?;
        locks::lock_deployment_authority(db, principal_id, project_id).await?;
    }
    let organization_id = match queries::project_organization(db, project_id, false).await? {
        Some(id) => id,
        None => return Ok(HashSet::new()),
    };
    let writer = queries::has_platform_admin(db, principal_id, false).await?
        || queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            ProjectRoleCode::ProjectAdmin,
            false,
        )
        .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            ProjectRoleCode::AgentDeveloper,
            false,
        )
        .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            ProjectRoleCode::Operator,
            false,
        )
        .await?;
    let reader = writer
        || queries::has_active_organization_role(
            db,
            principal_id,
            organization_id,
            OrganizationRoleCode::OrganizationAdmin,
            false,
        )
        .await?
        || queries::has_active_organization_role(
            db,
            principal_id,
            organization_id,
            OrganizationRoleCode::Auditor,
            false,
        )
        .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            ProjectRoleCode::DeploymentApprover,
            false,
        )
        .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            ProjectRoleCode::Auditor,
            false,
        )
        .await?;
    let mut grants = HashSet::new();
    if reader {
        grants.insert(DEPLOYMENT_VIEW);
    }
    if writer {
        grants.extend([
            DEPLOYMENT_REQUEST,
            DEPLOYMENT_CANCEL,
            DEPLOYMENT_RETRY,
            DEPLOYMENT_PROMOTE,
            DEPLOYMENT_ROLLBACK,
        ]);
    }
    Ok(grants)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityAssignment {
    pub organization_id: Uuid,
    pub project_id: Option<Uuid>,
    pub approval_view: bool,
    pub approval_decide: bool,
}

pub async fn authority_assignments(
    db: &impl ConnectionTrait,
    principal: Uuid,
    scoped_organization: Option<Uuid>,
    scoped_project: Option<Uuid>,
) -> Result<Vec<AuthorityAssignment>, DbErr> {
    queries::authority_assignments(db, principal, scoped_organization, scoped_project).await
}

pub async fn deployment_approval_capabilities(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<HashSet<&'static str>, DbErr> {
    if lock {
        // The project row first; see `deployment_capabilities`.
        queries::scope_exists(db, queries::ScopeKind::Project, project_id, true).await?;
        locks::lock_deployment_authority(db, principal_id, project_id).await?;
    }
    let administrator = is_platform_administrator(db, principal_id).await?;
    let mut approval_view = administrator;
    let mut approval_decide = administrator;
    if !administrator {
        for assignment in authority_assignments(db, principal_id, None, Some(project_id)).await? {
            if assignment.approval_view
                && (assignment.project_id == Some(project_id) || assignment.project_id.is_none())
            {
                approval_view = true;
            }
            if assignment.approval_decide && assignment.project_id == Some(project_id) {
                approval_decide = true;
            }
        }
    }
    let mut grants = HashSet::new();
    if approval_view {
        grants.insert(DEPLOYMENT_APPROVAL_VIEW);
    }
    if approval_decide {
        grants.insert(DEPLOYMENT_APPROVAL_DECIDE);
    }
    Ok(grants)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_sets_have_no_accidental_duplicates() {
        for set in [
            DEPLOYMENT_CAPABILITIES,
            EVALUATION_CAPABILITIES,
            CONFIGURATION_CAPABILITIES,
            ADMINISTRATION_CAPABILITIES,
            ORGANIZATION_ADMIN,
            INHERITED_ORGANIZATION_ADMIN,
            PROJECT_ADMIN,
            PROJECT_AUDITOR,
        ] {
            let unique: HashSet<&str> = set.iter().copied().collect();
            assert_eq!(unique.len(), set.len(), "{set:?} has a duplicate");
        }
    }
}
