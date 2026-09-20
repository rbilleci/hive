//! The deployment/evaluation capability-composition logic `hasCapability` needs for the
//! `DEPLOYMENT.*`/`DEPLOYMENT_APPROVAL.*`/`EVALUATION_*.*` capability families — the one part of the
//! original locked/unlocked twin split (see the design doc's `GSR-PHASE-P5` note) with no equivalent
//! already in `capability::queries.rs`: every primitive check here (`has_platform_admin`,
//! `has_active_organization_role`, `has_active_project_role`, `project_organization`,
//! `active_project`) is delegated straight to `capability::queries::*`, which already takes `lock:
//! bool` as a runtime parameter — the hand-duplicated hardcoded-locked/hardcoded-unlocked primitive
//! pairs this module used to carry (`has_platform_admin`/`has_platform_admin_read`, etc.) are gone.

use crate::capability::queries;
use sea_orm::{ConnectionTrait, DbErr};
use std::collections::HashSet;
use uuid::Uuid;

pub(crate) const DEPLOYMENT_VIEW: &str = "DEPLOYMENT.VIEW";
pub(crate) const DEPLOYMENT_REQUEST: &str = "DEPLOYMENT.REQUEST";
pub(crate) const DEPLOYMENT_CANCEL: &str = "DEPLOYMENT.CANCEL";
pub(crate) const DEPLOYMENT_RETRY: &str = "DEPLOYMENT.RETRY";
pub(crate) const DEPLOYMENT_PROMOTE: &str = "DEPLOYMENT.PROMOTE";
pub(crate) const DEPLOYMENT_ROLLBACK: &str = "DEPLOYMENT.ROLLBACK";
pub(crate) const DEPLOYMENT_APPROVAL_VIEW: &str = "DEPLOYMENT_APPROVAL.VIEW";
pub(crate) const DEPLOYMENT_APPROVAL_DECIDE: &str = "DEPLOYMENT_APPROVAL.DECIDE";
pub(crate) const EVALUATION_DEFINITION_VIEW: &str = "EVALUATION_DEFINITION.VIEW";
pub(crate) const EVALUATION_DEFINITION_AUTHOR: &str = "EVALUATION_DEFINITION.AUTHOR";
pub(crate) const EVALUATION_DEFINITION_PUBLISH: &str = "EVALUATION_DEFINITION.PUBLISH";
pub(crate) const EVALUATION_RUN_VIEW: &str = "EVALUATION_RUN.VIEW";
pub(crate) const EVALUATION_RUN_RUN: &str = "EVALUATION_RUN.RUN";
pub(crate) const EVALUATION_RUN_CANCEL: &str = "EVALUATION_RUN.CANCEL";
pub(crate) const EVALUATION_RUN_RERUN: &str = "EVALUATION_RUN.RERUN";
const EVALUATION_CAPABILITIES: &[&str] = &[
    EVALUATION_DEFINITION_VIEW,
    EVALUATION_DEFINITION_AUTHOR,
    EVALUATION_DEFINITION_PUBLISH,
    EVALUATION_RUN_VIEW,
    EVALUATION_RUN_RUN,
    EVALUATION_RUN_CANCEL,
    EVALUATION_RUN_RERUN,
];

/// Ports `hasCapability`'s `DEPLOYMENT_CAPABILITIES` handling, composed from `deploymentCapabilities`.
pub(crate) async fn deployment_capabilities(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<HashSet<&'static str>, DbErr> {
    let Some(organization_id) = queries::project_organization(db, project_id, lock).await? else {
        return Ok(HashSet::new());
    };
    let writer = queries::has_platform_admin(db, principal_id, lock).await?
        || queries::has_active_project_role(db, principal_id, project_id, "PROJECT_ADMIN", lock)
            .await?
        || queries::has_active_project_role(db, principal_id, project_id, "AGENT_DEVELOPER", lock)
            .await?
        || queries::has_active_project_role(db, principal_id, project_id, "OPERATOR", lock).await?;
    let reader = writer
        || queries::has_active_organization_role(
            db,
            principal_id,
            organization_id,
            "ORGANIZATION_ADMIN",
            lock,
        )
        .await?
        || queries::has_active_organization_role(
            db,
            principal_id,
            organization_id,
            "AUDITOR",
            lock,
        )
        .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            "DEPLOYMENT_APPROVER",
            lock,
        )
        .await?
        || queries::has_active_project_role(db, principal_id, project_id, "AUDITOR", lock).await?;
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

/// Ports `hasCapability`'s `DEPLOYMENT_APPROVAL_VIEW`/`DEPLOYMENT_APPROVAL_DECIDE` handling, composed
/// from `deploymentApprovalCapabilities`/`authorityAssignments`. `approval_view` and `approval_decide`
/// are granted by different role sets (see Java's `authorityAssignments` row shapes): an org-level
/// `ORGANIZATION_ADMIN`/`AUDITOR` grants only `approval_view` for every project under that org;
/// `approval_decide` is granted only by platform admin or a project-level `DEPLOYMENT_APPROVER`.
pub(crate) async fn deployment_approval_capabilities(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<HashSet<&'static str>, DbErr> {
    let administrator = queries::has_platform_admin(db, principal_id, lock).await?;
    let mut approval_view = administrator;
    let mut approval_decide = administrator;
    if !administrator {
        if let Some(organization_id) = queries::project_organization(db, project_id, lock).await? {
            let org_view = queries::has_active_organization_role(
                db,
                principal_id,
                organization_id,
                "ORGANIZATION_ADMIN",
                lock,
            )
            .await?
                || queries::has_active_organization_role(
                    db,
                    principal_id,
                    organization_id,
                    "AUDITOR",
                    lock,
                )
                .await?;
            if org_view {
                approval_view = true;
            }
        }
        let project_view =
            queries::has_active_project_role(db, principal_id, project_id, "PROJECT_ADMIN", lock)
                .await?
                || queries::has_active_project_role(
                    db,
                    principal_id,
                    project_id,
                    "DEPLOYMENT_APPROVER",
                    lock,
                )
                .await?
                || queries::has_active_project_role(db, principal_id, project_id, "AUDITOR", lock)
                    .await?;
        if project_view {
            approval_view = true;
        }
        let decide = queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            "DEPLOYMENT_APPROVER",
            lock,
        )
        .await?;
        if decide {
            approval_decide = true;
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

/// Ports the `hasCapability` dispatch for the 8 deployment/deployment-approval capability strings.
pub(crate) async fn has_deployment_capability(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    capability: &str,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    if matches!(
        capability,
        DEPLOYMENT_VIEW
            | DEPLOYMENT_REQUEST
            | DEPLOYMENT_CANCEL
            | DEPLOYMENT_RETRY
            | DEPLOYMENT_PROMOTE
            | DEPLOYMENT_ROLLBACK
    ) {
        let grants = deployment_capabilities(db, principal_id, project_id, lock).await?;
        let active = queries::active_project(db, project_id, lock).await?;
        return Ok(grants.contains(capability)
            && (capability == DEPLOYMENT_VIEW
                || capability == DEPLOYMENT_RETRY
                || capability == DEPLOYMENT_ROLLBACK
                || active));
    }
    if matches!(
        capability,
        DEPLOYMENT_APPROVAL_VIEW | DEPLOYMENT_APPROVAL_DECIDE
    ) {
        let grants = deployment_approval_capabilities(db, principal_id, project_id, lock).await?;
        let active = queries::active_project(db, project_id, lock).await?;
        return Ok(
            grants.contains(capability) && (capability == DEPLOYMENT_APPROVAL_VIEW || active)
        );
    }
    Ok(false)
}

/// Ports `capability::evaluation_capabilities`: PROJECT_ADMIN/AGENT_DEVELOPER get every capability
/// (evaluation has no narrower writer split like deployment's PROJECT_ADMIN/AGENT_DEVELOPER/OPERATOR
/// trio), OPERATOR gets the 4 run-only capabilities, and AUDITOR/DEPLOYMENT_APPROVER (project-level)
/// or AUDITOR/ORGANIZATION_ADMIN (org-level) get both VIEW capabilities — all four branches
/// independently gated by project-active except the last.
pub(crate) async fn evaluation_capabilities(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<HashSet<&'static str>, DbErr> {
    let Some(organization_id) = queries::project_organization(db, project_id, lock).await? else {
        return Ok(HashSet::new());
    };
    if lock {
        crate::capability::locks::lock_project_role_authority(db, principal_id, project_id).await?;
    }
    let active = queries::active_project(db, project_id, lock).await?;
    let administrator = queries::has_platform_admin(db, principal_id, lock).await?;
    if administrator {
        return Ok(if active {
            EVALUATION_CAPABILITIES.iter().copied().collect()
        } else {
            [EVALUATION_DEFINITION_VIEW, EVALUATION_RUN_VIEW]
                .into_iter()
                .collect()
        });
    }
    let full_writer = active
        && (queries::has_active_project_role(db, principal_id, project_id, "PROJECT_ADMIN", lock)
            .await?
            || queries::has_active_project_role(
                db,
                principal_id,
                project_id,
                "AGENT_DEVELOPER",
                lock,
            )
            .await?);
    if full_writer {
        return Ok(EVALUATION_CAPABILITIES.iter().copied().collect());
    }
    let mut grants = HashSet::new();
    let operator = active
        && queries::has_active_project_role(db, principal_id, project_id, "OPERATOR", lock).await?;
    if operator {
        grants.extend([
            EVALUATION_RUN_VIEW,
            EVALUATION_RUN_RUN,
            EVALUATION_RUN_CANCEL,
            EVALUATION_RUN_RERUN,
        ]);
    }
    let view = queries::has_active_project_role(db, principal_id, project_id, "AUDITOR", lock)
        .await?
        || queries::has_active_project_role(
            db,
            principal_id,
            project_id,
            "DEPLOYMENT_APPROVER",
            lock,
        )
        .await?
        || queries::has_active_organization_role(
            db,
            principal_id,
            organization_id,
            "AUDITOR",
            lock,
        )
        .await?
        || queries::has_active_organization_role(
            db,
            principal_id,
            organization_id,
            "ORGANIZATION_ADMIN",
            lock,
        )
        .await?;
    if view {
        grants.extend([EVALUATION_DEFINITION_VIEW, EVALUATION_RUN_VIEW]);
    }
    Ok(grants)
}

pub(crate) async fn has_evaluation_capability(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    capability: &str,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    Ok(evaluation_capabilities(db, principal_id, project_id, lock)
        .await?
        .contains(capability))
}
