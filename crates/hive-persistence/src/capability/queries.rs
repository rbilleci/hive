use super::locks;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub enum ScopeKind {
    Organization,
    Project,
}

pub async fn known_principal(
    pool: &PgPool,
    principal_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    let sql = if lock {
        "SELECT 1 FROM principals WHERE id = $1 FOR KEY SHARE"
    } else {
        "SELECT 1 FROM principals WHERE id = $1"
    };
    Ok(sqlx::query(sql)
        .bind(principal_id)
        .fetch_optional(pool)
        .await?
        .is_some())
}

pub async fn scope_exists(
    pool: &PgPool,
    kind: ScopeKind,
    id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    let table = match kind {
        ScopeKind::Organization => "organizations",
        ScopeKind::Project => "projects",
    };
    let sql = format!(
        "SELECT 1 FROM {table} WHERE id = $1{}",
        if lock { " FOR KEY SHARE" } else { "" }
    );
    Ok(sqlx::query(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?
        .is_some())
}

pub async fn has_platform_admin(
    pool: &PgPool,
    principal_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    let sql = format!(
        "SELECT 1 FROM platform_role_assignments WHERE principal_id = $1 AND role_code = 'PLATFORM_ADMIN'{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    Ok(sqlx::query(&sql)
        .bind(principal_id)
        .fetch_optional(pool)
        .await?
        .is_some())
}

pub async fn active_organization_membership(
    pool: &PgPool,
    principal_id: Uuid,
    organization_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    let sql = format!(
        "SELECT 1 FROM organization_memberships WHERE principal_id = $1 AND organization_id = $2 \
         AND started_at <= CURRENT_TIMESTAMP AND ended_at IS NULL{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    Ok(sqlx::query(&sql)
        .bind(principal_id)
        .bind(organization_id)
        .fetch_optional(pool)
        .await?
        .is_some())
}

pub async fn has_active_organization_role(
    pool: &PgPool,
    principal_id: Uuid,
    organization_id: Uuid,
    role: &str,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    if lock {
        locks::lock(
            pool,
            "SELECT membership.id FROM organization_memberships membership WHERE membership.principal_id = $1 \
             AND membership.organization_id = $2 FOR UPDATE",
            &[&principal_id, &organization_id],
        )
        .await?;
        locks::lock(
            pool,
            "SELECT roles.membership_id FROM organization_membership_roles roles \
             JOIN organization_memberships membership ON membership.id = roles.membership_id \
             WHERE membership.principal_id = $1 AND membership.organization_id = $2 FOR UPDATE OF roles",
            &[&principal_id, &organization_id],
        )
        .await?;
    }
    Ok(sqlx::query(
        "SELECT 1 FROM organization_memberships membership \
         JOIN organization_membership_roles roles ON roles.membership_id = membership.id \
         WHERE membership.principal_id = $1 AND membership.organization_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL AND roles.role_code = $3",
    )
    .bind(principal_id)
    .bind(organization_id)
    .bind(role)
    .fetch_optional(pool)
    .await?
    .is_some())
}

pub async fn has_active_project_role(
    pool: &PgPool,
    principal_id: Uuid,
    project_id: Uuid,
    role: &str,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    if lock {
        locks::lock_project_role_authority(pool, principal_id, project_id).await?;
    }
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
    .fetch_optional(pool)
    .await?
    .is_some())
}

pub async fn project_visible(
    pool: &PgPool,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    Ok(
        active_organization_for_project(pool, principal_id, project_id, lock).await?
            || has_active_project_role(pool, principal_id, project_id, "PROJECT_ADMIN", lock)
                .await?
            || has_active_project_role(pool, principal_id, project_id, "AGENT_DEVELOPER", lock)
                .await?
            || has_active_project_role(pool, principal_id, project_id, "OPERATOR", lock).await?
            || has_active_project_role(pool, principal_id, project_id, "DEPLOYMENT_APPROVER", lock)
                .await?
            || has_active_project_role(pool, principal_id, project_id, "AUDITOR", lock).await?
            || has_platform_admin(pool, principal_id, lock).await?,
    )
}

pub async fn organization_visible(
    pool: &PgPool,
    principal_id: Uuid,
    organization_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    Ok(
        active_organization_membership(pool, principal_id, organization_id, lock).await?
            || has_platform_admin(pool, principal_id, lock).await?,
    )
}

async fn active_organization_for_project(
    pool: &PgPool,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    let sql = format!(
        "SELECT 1 FROM projects project JOIN organization_memberships membership ON membership.organization_id = project.organization_id \
         WHERE project.id = $1 AND membership.principal_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL{}",
        if lock { " FOR UPDATE OF membership" } else { "" }
    );
    Ok(sqlx::query(&sql)
        .bind(project_id)
        .bind(principal_id)
        .fetch_optional(pool)
        .await?
        .is_some())
}

pub async fn legacy_or_developer(
    pool: &PgPool,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    let sql = format!(
        "SELECT 1 FROM console_role_assignments assignment \
         JOIN projects project ON project.id = assignment.project_id \
         JOIN organization_memberships membership ON membership.organization_id = project.organization_id \
           AND membership.principal_id = assignment.principal_id \
         WHERE assignment.project_id = $1 AND assignment.principal_id = $2 \
           AND assignment.role_code IN ('PROJECT_ADMIN', 'AGENT_DEVELOPER') \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL{}",
        if lock { " FOR UPDATE OF assignment, membership" } else { "" }
    );
    Ok(sqlx::query(&sql)
        .bind(project_id)
        .bind(principal_id)
        .fetch_optional(pool)
        .await?
        .is_some()
        || has_active_project_role(pool, principal_id, project_id, "PROJECT_ADMIN", lock).await?
        || has_active_project_role(pool, principal_id, project_id, "AGENT_DEVELOPER", lock).await?)
}

pub async fn project_organization(
    pool: &PgPool,
    project_id: Uuid,
    lock: bool,
) -> Result<Option<Uuid>, sqlx::Error> {
    let sql = format!(
        "SELECT organization_id FROM projects WHERE id = $1{}",
        if lock { " FOR KEY SHARE" } else { "" }
    );
    let row: Option<(Uuid,)> = sqlx::query_as(&sql)
        .bind(project_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(organization_id,)| organization_id))
}

pub async fn active_project(
    pool: &PgPool,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, sqlx::Error> {
    let sql = format!(
        "SELECT 1 FROM projects WHERE id = $1 AND lifecycle_status = 'ACTIVE'{}",
        if lock { " FOR KEY SHARE" } else { "" }
    );
    Ok(sqlx::query(&sql)
        .bind(project_id)
        .fetch_optional(pool)
        .await?
        .is_some())
}

pub async fn authority_assignments(
    pool: &PgPool,
    principal: Uuid,
    scoped_organization: Option<Uuid>,
    scoped_project: Option<Uuid>,
) -> Result<Vec<super::AuthorityAssignment>, sqlx::Error> {
    let rows: Vec<(Uuid, Option<Uuid>, bool, bool)> = sqlx::query_as(
        "WITH assignments AS (\
           SELECT membership.organization_id, NULL::UUID AS project_id, 'ORGANIZATION' AS assignment_scope, role.role_code \
           FROM organization_memberships membership \
             JOIN organization_membership_roles role ON role.membership_id = membership.id \
           WHERE membership.principal_id = $1 \
             AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
             AND ($2::uuid IS NULL OR membership.organization_id = $2) \
             AND ($3::uuid IS NULL OR EXISTS (SELECT 1 FROM projects project \
                                               WHERE project.id = $3 AND project.organization_id = membership.organization_id)) \
           UNION ALL \
           SELECT project.organization_id, membership.project_id, 'PROJECT', role.role_code \
           FROM project_memberships membership \
             JOIN project_membership_roles role ON role.membership_id = membership.id \
             JOIN projects project ON project.id = membership.project_id \
             JOIN organization_memberships organization_membership \
               ON organization_membership.organization_id = project.organization_id \
                 AND organization_membership.principal_id = membership.principal_id \
           WHERE membership.principal_id = $1 \
             AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
             AND organization_membership.started_at <= CURRENT_TIMESTAMP AND organization_membership.ended_at IS NULL \
             AND ($2::uuid IS NULL OR project.organization_id = $2) \
             AND ($3::uuid IS NULL OR membership.project_id = $3) \
         ) \
         SELECT organization_id, NULL::UUID, TRUE, FALSE \
         FROM assignments WHERE assignment_scope = 'ORGANIZATION' AND role_code IN ('ORGANIZATION_ADMIN', 'AUDITOR') \
         UNION ALL \
         SELECT organization_id, project_id, TRUE, role_code = 'DEPLOYMENT_APPROVER' \
         FROM assignments WHERE assignment_scope = 'PROJECT' AND role_code IN ('PROJECT_ADMIN', 'DEPLOYMENT_APPROVER', 'AUDITOR')",
    )
    .bind(principal)
    .bind(scoped_organization)
    .bind(scoped_project)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(organization_id, project_id, approval_view, approval_decide)| {
                super::AuthorityAssignment {
                    organization_id,
                    project_id,
                    approval_view,
                    approval_decide,
                }
            },
        )
        .collect())
}
