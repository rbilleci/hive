use sqlx::PgPool;
use uuid::Uuid;

/// Ports the private `lock` helper: runs a `FOR UPDATE`/`FOR KEY SHARE` query and
/// drains every row, matching Java's `while (ignored.next()) {}` — the lock is
/// acquired as each row is fetched, so the whole result set must be consumed.
pub async fn lock(pool: &PgPool, sql: &str, params: &[&Uuid]) -> Result<(), sqlx::Error> {
    let mut query = sqlx::query(sql);
    for param in params {
        query = query.bind(**param);
    }
    query.fetch_all(pool).await?;
    Ok(())
}

/// Ports `lockProjectRoleAuthority`.
pub async fn lock_project_role_authority(
    pool: &PgPool,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<(), sqlx::Error> {
    lock(
        pool,
        "SELECT membership.id FROM organization_memberships membership \
         JOIN projects project ON project.organization_id = membership.organization_id \
         WHERE project.id = $1 AND membership.principal_id = $2 FOR UPDATE OF membership",
        &[&project_id, &principal_id],
    )
    .await?;
    lock(
        pool,
        "SELECT role.membership_id FROM organization_membership_roles role \
         JOIN organization_memberships membership ON membership.id = role.membership_id \
         JOIN projects project ON project.organization_id = membership.organization_id \
         WHERE project.id = $1 AND membership.principal_id = $2 FOR UPDATE OF role",
        &[&project_id, &principal_id],
    )
    .await?;
    lock(
        pool,
        "SELECT membership.id FROM project_memberships membership \
         WHERE membership.project_id = $1 AND membership.principal_id = $2 FOR UPDATE",
        &[&project_id, &principal_id],
    )
    .await?;
    lock(
        pool,
        "SELECT role.membership_id FROM project_membership_roles role \
         JOIN project_memberships membership ON membership.id = role.membership_id \
         WHERE membership.project_id = $1 AND membership.principal_id = $2 FOR UPDATE OF role",
        &[&project_id, &principal_id],
    )
    .await
}

/// Ports `lockDeploymentAuthority`.
pub async fn lock_deployment_authority(
    pool: &PgPool,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<(), sqlx::Error> {
    lock(
        pool,
        "SELECT platform.principal_id FROM platform_role_assignments platform WHERE platform.principal_id = $1 FOR UPDATE",
        &[&principal_id],
    )
    .await?;
    lock_project_role_authority(pool, principal_id, project_id).await
}

/// Ports `lockDeploymentApprovalAuthorityPage`.
pub async fn lock_deployment_approval_authority_page(
    pool: &PgPool,
    principal_id: Uuid,
    project_ids: &[Uuid],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT principal_id FROM platform_role_assignments WHERE principal_id = $1 FOR UPDATE",
    )
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    sqlx::query(
        "SELECT membership.id FROM organization_memberships membership \
         JOIN projects project ON project.organization_id = membership.organization_id \
         WHERE project.id = ANY($1) AND membership.principal_id = $2 \
         ORDER BY membership.organization_id, membership.id FOR UPDATE OF membership",
    )
    .bind(project_ids)
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    sqlx::query(
        "SELECT role.membership_id FROM organization_membership_roles role \
         JOIN organization_memberships membership ON membership.id = role.membership_id \
         JOIN projects project ON project.organization_id = membership.organization_id \
         WHERE project.id = ANY($1) AND membership.principal_id = $2 \
         ORDER BY role.membership_id, role.role_code FOR UPDATE OF role",
    )
    .bind(project_ids)
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    sqlx::query(
        "SELECT membership.id FROM project_memberships membership \
         WHERE membership.project_id = ANY($1) AND membership.principal_id = $2 \
         ORDER BY membership.project_id, membership.id FOR UPDATE",
    )
    .bind(project_ids)
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    sqlx::query(
        "SELECT role.membership_id FROM project_membership_roles role \
         JOIN project_memberships membership ON membership.id = role.membership_id \
         WHERE membership.project_id = ANY($1) AND membership.principal_id = $2 \
         ORDER BY membership.project_id, role.membership_id, role.role_code FOR UPDATE OF role",
    )
    .bind(project_ids)
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    Ok(())
}
