//! Transaction-scoped twins of a subset of `queries.rs`'s capability
//! primitives. `has_capability` and `queries::*` take `&PgPool`, so a caller
//! that needs `lock = true` to hold across the same transaction as the write
//! it protects — administration's and agent draft's write commands, matching
//! Java's `capabilities.hasCapability(connection, ...)` — cannot reuse them
//! directly (the pool would grab an unrelated connection for the locked
//! read). These are hand-duplicated here rather than genericizing `queries.rs`
//! over `sqlx::PgExecutor`, the same call this crate's other generic-over-
//! executor attempt (an early `exists_one` helper) made before it hit
//! lifetime friction across `.bind()`/`.fetch` chains and was abandoned for
//! inlined queries instead.
//!
//! Every function here hardcodes the locked (`lock = true`) SQL variant: it
//! is the only one a write path ever needs, since read paths call
//! `has_capability(&pool, ..., false)` directly.

use sqlx::PgConnection;
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

pub(crate) async fn lock(
    conn: &mut PgConnection,
    sql: &str,
    a: Uuid,
    b: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(sql)
        .bind(a)
        .bind(b)
        .fetch_all(&mut *conn)
        .await?;
    Ok(())
}

pub(crate) async fn has_platform_admin(
    conn: &mut PgConnection,
    principal_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("SELECT 1 FROM platform_role_assignments WHERE principal_id = $1 AND role_code = 'PLATFORM_ADMIN' FOR UPDATE")
        .bind(principal_id)
        .fetch_optional(&mut *conn)
        .await?
        .is_some())
}

pub(crate) async fn is_platform_administrator(
    conn: &mut PgConnection,
    principal_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("SELECT 1 FROM platform_role_assignments WHERE principal_id = $1 AND role_code = 'PLATFORM_ADMIN'")
        .bind(principal_id)
        .fetch_optional(&mut *conn)
        .await?
        .is_some())
}

pub(crate) async fn has_active_organization_role(
    conn: &mut PgConnection,
    principal_id: Uuid,
    organization_id: Uuid,
    role: &str,
) -> Result<bool, sqlx::Error> {
    lock(
        conn,
        "SELECT membership.id FROM organization_memberships membership WHERE membership.principal_id = $1 \
         AND membership.organization_id = $2 FOR UPDATE",
        principal_id,
        organization_id,
    )
    .await?;
    lock(
        conn,
        "SELECT roles.membership_id FROM organization_membership_roles roles \
         JOIN organization_memberships membership ON membership.id = roles.membership_id \
         WHERE membership.principal_id = $1 AND membership.organization_id = $2 FOR UPDATE OF roles",
        principal_id,
        organization_id,
    )
    .await?;
    Ok(sqlx::query(
        "SELECT 1 FROM organization_memberships membership \
         JOIN organization_membership_roles roles ON roles.membership_id = membership.id \
         WHERE membership.principal_id = $1 AND membership.organization_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL AND roles.role_code = $3",
    )
    .bind(principal_id)
    .bind(organization_id)
    .bind(role)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

pub(crate) async fn lock_project_role_authority(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<(), sqlx::Error> {
    lock(
        conn,
        "SELECT membership.id FROM organization_memberships membership \
         JOIN projects project ON project.organization_id = membership.organization_id \
         WHERE project.id = $1 AND membership.principal_id = $2 FOR UPDATE OF membership",
        project_id,
        principal_id,
    )
    .await?;
    lock(
        conn,
        "SELECT role.membership_id FROM organization_membership_roles role \
         JOIN organization_memberships membership ON membership.id = role.membership_id \
         JOIN projects project ON project.organization_id = membership.organization_id \
         WHERE project.id = $1 AND membership.principal_id = $2 FOR UPDATE OF role",
        project_id,
        principal_id,
    )
    .await?;
    lock(
        conn,
        "SELECT membership.id FROM project_memberships membership \
         WHERE membership.project_id = $1 AND membership.principal_id = $2 FOR UPDATE",
        project_id,
        principal_id,
    )
    .await?;
    lock(
        conn,
        "SELECT role.membership_id FROM project_membership_roles role \
         JOIN project_memberships membership ON membership.id = role.membership_id \
         WHERE membership.project_id = $1 AND membership.principal_id = $2 FOR UPDATE OF role",
        project_id,
        principal_id,
    )
    .await
}

pub(crate) async fn has_active_project_role(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
    role: &str,
) -> Result<bool, sqlx::Error> {
    lock_project_role_authority(conn, principal_id, project_id).await?;
    Ok(sqlx::query(
        "SELECT 1 FROM project_memberships membership \
         JOIN project_membership_roles roles ON roles.membership_id = membership.id \
         JOIN projects project ON project.id = membership.project_id \
         JOIN organization_memberships organization_membership \
           ON organization_membership.organization_id = project.organization_id \
             AND organization_membership.principal_id = membership.principal_id \
         WHERE membership.principal_id = $1 AND membership.project_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
           AND organization_membership.started_at <= CURRENT_TIMESTAMP AND organization_membership.ended_at IS NULL \
           AND roles.role_code = $3",
    )
    .bind(principal_id)
    .bind(project_id)
    .bind(role)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

pub(crate) async fn project_organization(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT organization_id FROM projects WHERE id = $1 FOR KEY SHARE")
            .bind(project_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.map(|(id,)| id))
}

pub(crate) async fn active_project(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "SELECT 1 FROM projects WHERE id = $1 AND lifecycle_status = 'ACTIVE' FOR KEY SHARE",
    )
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

pub(crate) async fn active_organization_for_project(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "SELECT 1 FROM projects project JOIN organization_memberships membership ON membership.organization_id = project.organization_id \
         WHERE project.id = $1 AND membership.principal_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL FOR UPDATE OF membership",
    )
    .bind(project_id)
    .bind(principal_id)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

/// Ports the `AGENT.VIEW` branch of `hasCapability` (`queries::project_visible`)
/// with `lock` hardcoded `true`.
pub(crate) async fn project_visible(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(
        active_organization_for_project(conn, principal_id, project_id).await?
            || has_active_project_role(conn, principal_id, project_id, "PROJECT_ADMIN").await?
            || has_active_project_role(conn, principal_id, project_id, "AGENT_DEVELOPER").await?
            || has_active_project_role(conn, principal_id, project_id, "OPERATOR").await?
            || has_active_project_role(conn, principal_id, project_id, "DEPLOYMENT_APPROVER")
                .await?
            || has_active_project_role(conn, principal_id, project_id, "AUDITOR").await?
            || has_platform_admin(conn, principal_id).await?,
    )
}

/// Ports the `CONFIGURATION.AUTHOR` / `CONFIGURATION.PUBLISH` /
/// `TOOL_CONNECTION.UPDATE` branch of `hasCapability` with `lock` hardcoded
/// `true`: `active_project && (legacy_or_developer || PROJECT_ADMIN)`. The
/// `PROJECT_ADMIN` disjunct is not strictly redundant with
/// `legacy_or_developer` (which already ORs in a `PROJECT_ADMIN` check of its
/// own) — it is a literal transcription of Java's `hasCapability`, kept
/// verbatim rather than simplified.
pub(crate) async fn configuration_write(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(active_project(conn, project_id).await?
        && (legacy_or_developer(conn, principal_id, project_id).await?
            || has_active_project_role(conn, principal_id, project_id, "PROJECT_ADMIN").await?))
}

/// Ports the `AGENT_DRAFT.UPDATE` branch of `hasCapability`
/// (`queries::legacy_or_developer`) with `lock` hardcoded `true`.
pub(crate) async fn legacy_or_developer(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let assignment = sqlx::query(
        "SELECT 1 FROM console_role_assignments assignment \
         JOIN projects project ON project.id = assignment.project_id \
         JOIN organization_memberships membership ON membership.organization_id = project.organization_id \
           AND membership.principal_id = assignment.principal_id \
         WHERE assignment.project_id = $1 AND assignment.principal_id = $2 \
           AND assignment.role_code IN ('PROJECT_ADMIN', 'AGENT_DEVELOPER') \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL FOR UPDATE OF assignment, membership",
    )
    .bind(project_id)
    .bind(principal_id)
    .fetch_optional(&mut *conn)
    .await?
    .is_some();
    Ok(assignment
        || has_active_project_role(conn, principal_id, project_id, "PROJECT_ADMIN").await?
        || has_active_project_role(conn, principal_id, project_id, "AGENT_DEVELOPER").await?)
}

/// Unlocked companions to this module's hardcoded-locked primitives, needed only by
/// `deployment_capabilities`/`deployment_approval_capabilities`'s `lock = false` callers (Java's own
/// `lock` parameter reaches all the way down to these, unlike every other composite in this module,
/// which only a write path ever calls). Plain reads, no `FOR UPDATE`/`FOR KEY SHARE`.
pub(crate) async fn has_platform_admin_read(
    conn: &mut PgConnection,
    principal_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("SELECT 1 FROM platform_role_assignments WHERE principal_id = $1 AND role_code = 'PLATFORM_ADMIN'").bind(principal_id).fetch_optional(&mut *conn).await?.is_some())
}

async fn has_active_project_role_read(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
    role: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "SELECT 1 FROM project_memberships membership \
         JOIN project_membership_roles roles ON roles.membership_id = membership.id \
         JOIN projects project ON project.id = membership.project_id \
         JOIN organization_memberships organization_membership \
           ON organization_membership.organization_id = project.organization_id \
             AND organization_membership.principal_id = membership.principal_id \
         WHERE membership.principal_id = $1 AND membership.project_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
           AND organization_membership.started_at <= CURRENT_TIMESTAMP AND organization_membership.ended_at IS NULL \
           AND roles.role_code = $3",
    )
    .bind(principal_id)
    .bind(project_id)
    .bind(role)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

async fn has_active_organization_role_read(
    conn: &mut PgConnection,
    principal_id: Uuid,
    organization_id: Uuid,
    role: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "SELECT 1 FROM organization_memberships membership \
         JOIN organization_membership_roles roles ON roles.membership_id = membership.id \
         WHERE membership.principal_id = $1 AND membership.organization_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL AND roles.role_code = $3",
    )
    .bind(principal_id)
    .bind(organization_id)
    .bind(role)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

async fn project_organization_read(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT organization_id FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|(id,)| id))
}

async fn active_project_read(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("SELECT 1 FROM projects WHERE id = $1 AND lifecycle_status = 'ACTIVE'")
            .bind(project_id)
            .fetch_optional(&mut *conn)
            .await?
            .is_some(),
    )
}

/// Ports `hasCapability`'s `DEPLOYMENT_CAPABILITIES` handling, composed from `deploymentCapabilities`.
/// When `lock` is `true`, every read below reuses this module's own always-locked primitives directly
/// (instead of a separate `lockDeploymentAuthority` step followed by unlocked reads, as Java does):
/// each primitive already takes the same lock Java's `lockDeploymentAuthority` would, so the net lock
/// set is equivalent, just acquired inline rather than upfront.
pub(crate) async fn deployment_capabilities(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<HashSet<&'static str>, sqlx::Error> {
    let organization_id = if lock {
        project_organization(conn, project_id).await?
    } else {
        project_organization_read(conn, project_id).await?
    };
    let Some(organization_id) = organization_id else {
        return Ok(HashSet::new());
    };
    let writer = if lock {
        has_platform_admin(conn, principal_id).await?
            || has_active_project_role(conn, principal_id, project_id, "PROJECT_ADMIN").await?
            || has_active_project_role(conn, principal_id, project_id, "AGENT_DEVELOPER").await?
            || has_active_project_role(conn, principal_id, project_id, "OPERATOR").await?
    } else {
        has_platform_admin_read(conn, principal_id).await?
            || has_active_project_role_read(conn, principal_id, project_id, "PROJECT_ADMIN").await?
            || has_active_project_role_read(conn, principal_id, project_id, "AGENT_DEVELOPER")
                .await?
            || has_active_project_role_read(conn, principal_id, project_id, "OPERATOR").await?
    };
    let reader = writer
        || if lock {
            has_active_organization_role(conn, principal_id, organization_id, "ORGANIZATION_ADMIN")
                .await?
                || has_active_organization_role(conn, principal_id, organization_id, "AUDITOR")
                    .await?
                || has_active_project_role(conn, principal_id, project_id, "DEPLOYMENT_APPROVER")
                    .await?
                || has_active_project_role(conn, principal_id, project_id, "AUDITOR").await?
        } else {
            has_active_organization_role_read(
                conn,
                principal_id,
                organization_id,
                "ORGANIZATION_ADMIN",
            )
            .await?
                || has_active_organization_role_read(conn, principal_id, organization_id, "AUDITOR")
                    .await?
                || has_active_project_role_read(
                    conn,
                    principal_id,
                    project_id,
                    "DEPLOYMENT_APPROVER",
                )
                .await?
                || has_active_project_role_read(conn, principal_id, project_id, "AUDITOR").await?
        };
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
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<HashSet<&'static str>, sqlx::Error> {
    let administrator = if lock {
        has_platform_admin(conn, principal_id).await?
    } else {
        has_platform_admin_read(conn, principal_id).await?
    };
    let mut approval_view = administrator;
    let mut approval_decide = administrator;
    if !administrator {
        let organization_id = if lock {
            project_organization(conn, project_id).await?
        } else {
            project_organization_read(conn, project_id).await?
        };
        if let Some(organization_id) = organization_id {
            let org_view = if lock {
                has_active_organization_role(
                    conn,
                    principal_id,
                    organization_id,
                    "ORGANIZATION_ADMIN",
                )
                .await?
                    || has_active_organization_role(conn, principal_id, organization_id, "AUDITOR")
                        .await?
            } else {
                has_active_organization_role_read(
                    conn,
                    principal_id,
                    organization_id,
                    "ORGANIZATION_ADMIN",
                )
                .await?
                    || has_active_organization_role_read(
                        conn,
                        principal_id,
                        organization_id,
                        "AUDITOR",
                    )
                    .await?
            };
            if org_view {
                approval_view = true;
            }
        }
        let project_view = if lock {
            has_active_project_role(conn, principal_id, project_id, "PROJECT_ADMIN").await?
                || has_active_project_role(conn, principal_id, project_id, "DEPLOYMENT_APPROVER")
                    .await?
                || has_active_project_role(conn, principal_id, project_id, "AUDITOR").await?
        } else {
            has_active_project_role_read(conn, principal_id, project_id, "PROJECT_ADMIN").await?
                || has_active_project_role_read(
                    conn,
                    principal_id,
                    project_id,
                    "DEPLOYMENT_APPROVER",
                )
                .await?
                || has_active_project_role_read(conn, principal_id, project_id, "AUDITOR").await?
        };
        if project_view {
            approval_view = true;
        }
        let decide = if lock {
            has_active_project_role(conn, principal_id, project_id, "DEPLOYMENT_APPROVER").await?
        } else {
            has_active_project_role_read(conn, principal_id, project_id, "DEPLOYMENT_APPROVER")
                .await?
        };
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
    conn: &mut PgConnection,
    principal_id: Uuid,
    capability: &str,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    if matches!(
        capability,
        DEPLOYMENT_VIEW
            | DEPLOYMENT_REQUEST
            | DEPLOYMENT_CANCEL
            | DEPLOYMENT_RETRY
            | DEPLOYMENT_PROMOTE
            | DEPLOYMENT_ROLLBACK
    ) {
        let grants = deployment_capabilities(conn, principal_id, project_id, lock).await?;
        let active = if lock {
            active_project(conn, project_id).await?
        } else {
            active_project_read(conn, project_id).await?
        };
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
        let grants = deployment_approval_capabilities(conn, principal_id, project_id, lock).await?;
        let active = if lock {
            active_project(conn, project_id).await?
        } else {
            active_project_read(conn, project_id).await?
        };
        return Ok(
            grants.contains(capability) && (capability == DEPLOYMENT_APPROVAL_VIEW || active)
        );
    }
    Ok(false)
}

/// Transaction-scoped twin of `capability::evaluation_capabilities`: PROJECT_ADMIN/AGENT_DEVELOPER
/// get every capability (evaluation has no narrower writer split like deployment's
/// PROJECT_ADMIN/AGENT_DEVELOPER/OPERATOR trio), OPERATOR gets the 4 run-only capabilities, and
/// AUDITOR/DEPLOYMENT_APPROVER (project-level) or AUDITOR/ORGANIZATION_ADMIN (org-level) get both
/// VIEW capabilities — all four branches independently gated by project-active except the last.
pub(crate) async fn evaluation_capabilities(
    conn: &mut PgConnection,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<HashSet<&'static str>, sqlx::Error> {
    let organization_id = if lock {
        project_organization(conn, project_id).await?
    } else {
        project_organization_read(conn, project_id).await?
    };
    let Some(organization_id) = organization_id else {
        return Ok(HashSet::new());
    };
    if lock {
        lock_project_role_authority(conn, principal_id, project_id).await?;
    }
    let active = if lock {
        active_project(conn, project_id).await?
    } else {
        active_project_read(conn, project_id).await?
    };
    let administrator = if lock {
        has_platform_admin(conn, principal_id).await?
    } else {
        has_platform_admin_read(conn, principal_id).await?
    };
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
        && if lock {
            has_active_project_role(conn, principal_id, project_id, "PROJECT_ADMIN").await?
                || has_active_project_role(conn, principal_id, project_id, "AGENT_DEVELOPER")
                    .await?
        } else {
            has_active_project_role_read(conn, principal_id, project_id, "PROJECT_ADMIN").await?
                || has_active_project_role_read(conn, principal_id, project_id, "AGENT_DEVELOPER")
                    .await?
        };
    if full_writer {
        return Ok(EVALUATION_CAPABILITIES.iter().copied().collect());
    }
    let mut grants = HashSet::new();
    let operator = active
        && if lock {
            has_active_project_role(conn, principal_id, project_id, "OPERATOR").await?
        } else {
            has_active_project_role_read(conn, principal_id, project_id, "OPERATOR").await?
        };
    if operator {
        grants.extend([
            EVALUATION_RUN_VIEW,
            EVALUATION_RUN_RUN,
            EVALUATION_RUN_CANCEL,
            EVALUATION_RUN_RERUN,
        ]);
    }
    let view = if lock {
        has_active_project_role(conn, principal_id, project_id, "AUDITOR").await?
            || has_active_project_role(conn, principal_id, project_id, "DEPLOYMENT_APPROVER")
                .await?
            || has_active_organization_role(conn, principal_id, organization_id, "AUDITOR").await?
            || has_active_organization_role(
                conn,
                principal_id,
                organization_id,
                "ORGANIZATION_ADMIN",
            )
            .await?
    } else {
        has_active_project_role_read(conn, principal_id, project_id, "AUDITOR").await?
            || has_active_project_role_read(conn, principal_id, project_id, "DEPLOYMENT_APPROVER")
                .await?
            || has_active_organization_role_read(conn, principal_id, organization_id, "AUDITOR")
                .await?
            || has_active_organization_role_read(
                conn,
                principal_id,
                organization_id,
                "ORGANIZATION_ADMIN",
            )
            .await?
    };
    if view {
        grants.extend([EVALUATION_DEFINITION_VIEW, EVALUATION_RUN_VIEW]);
    }
    Ok(grants)
}

pub(crate) async fn has_evaluation_capability(
    conn: &mut PgConnection,
    principal_id: Uuid,
    capability: &str,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    Ok(
        evaluation_capabilities(conn, principal_id, project_id, lock)
            .await?
            .contains(capability),
    )
}
