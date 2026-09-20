use sea_orm::{ConnectionTrait, DbErr, Statement, Value};
use uuid::Uuid;

async fn exists(db: &impl ConnectionTrait, sql: &str, values: Vec<Value>) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(db.get_database_backend(), sql, values);
    Ok(db.query_one_raw(statement).await?.is_some())
}

#[derive(Debug, Clone, Copy)]
pub enum ScopeKind {
    Organization,
    Project,
}

pub async fn known_principal(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let sql = if lock {
        "SELECT 1 FROM principals WHERE id = $1 FOR KEY SHARE"
    } else {
        "SELECT 1 FROM principals WHERE id = $1"
    };
    exists(db, sql, vec![principal_id.into()]).await
}

pub async fn scope_exists(
    db: &impl ConnectionTrait,
    kind: ScopeKind,
    id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let table = match kind {
        ScopeKind::Organization => "organizations",
        ScopeKind::Project => "projects",
    };
    let sql = format!(
        "SELECT 1 FROM {table} WHERE id = $1{}",
        if lock { " FOR KEY SHARE" } else { "" }
    );
    exists(db, &sql, vec![id.into()]).await
}

pub async fn has_platform_admin(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let sql = format!(
        "SELECT 1 FROM platform_role_assignments WHERE principal_id = $1 AND role_code = 'PLATFORM_ADMIN'{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    exists(db, &sql, vec![principal_id.into()]).await
}

pub async fn active_organization_membership(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let sql = format!(
        "SELECT 1 FROM organization_memberships WHERE principal_id = $1 AND organization_id = $2 \
         AND started_at <= CURRENT_TIMESTAMP AND ended_at IS NULL{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    exists(db, &sql, vec![principal_id.into(), organization_id.into()]).await
}

pub async fn has_active_organization_role(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Uuid,
    role: &str,
    lock: bool,
) -> Result<bool, DbErr> {
    if lock {
        super::locks::lock(
            db,
            "SELECT membership.id FROM organization_memberships membership WHERE membership.principal_id = $1 \
             AND membership.organization_id = $2 FOR UPDATE",
            vec![principal_id.into(), organization_id.into()],
        )
        .await?;
        super::locks::lock(
            db,
            "SELECT roles.membership_id FROM organization_membership_roles roles \
             JOIN organization_memberships membership ON membership.id = roles.membership_id \
             WHERE membership.principal_id = $1 AND membership.organization_id = $2 FOR UPDATE OF roles",
            vec![principal_id.into(), organization_id.into()],
        )
        .await?;
    }
    exists(
        db,
        "SELECT 1 FROM organization_memberships membership \
         JOIN organization_membership_roles roles ON roles.membership_id = membership.id \
         WHERE membership.principal_id = $1 AND membership.organization_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL AND roles.role_code = $3",
        vec![principal_id.into(), organization_id.into(), role.into()],
    )
    .await
}

pub async fn has_active_project_role(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    role: &str,
    lock: bool,
) -> Result<bool, DbErr> {
    if lock {
        super::locks::lock_project_role_authority(db, principal_id, project_id).await?;
    }
    exists(
        db,
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
        vec![principal_id.into(), project_id.into(), role.into()],
    )
    .await
}

pub async fn project_visible(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    Ok(
        active_organization_for_project(db, principal_id, project_id, lock).await?
            || has_active_project_role(db, principal_id, project_id, "PROJECT_ADMIN", lock).await?
            || has_active_project_role(db, principal_id, project_id, "AGENT_DEVELOPER", lock)
                .await?
            || has_active_project_role(db, principal_id, project_id, "OPERATOR", lock).await?
            || has_active_project_role(db, principal_id, project_id, "DEPLOYMENT_APPROVER", lock)
                .await?
            || has_active_project_role(db, principal_id, project_id, "AUDITOR", lock).await?
            || has_platform_admin(db, principal_id, lock).await?,
    )
}

pub async fn organization_visible(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    Ok(
        active_organization_membership(db, principal_id, organization_id, lock).await?
            || has_platform_admin(db, principal_id, lock).await?,
    )
}

async fn active_organization_for_project(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let sql = format!(
        "SELECT 1 FROM projects project JOIN organization_memberships membership ON membership.organization_id = project.organization_id \
         WHERE project.id = $1 AND membership.principal_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL{}",
        if lock { " FOR UPDATE OF membership" } else { "" }
    );
    exists(db, &sql, vec![project_id.into(), principal_id.into()]).await
}

pub async fn legacy_or_developer(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
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
    Ok(
        exists(db, &sql, vec![project_id.into(), principal_id.into()]).await?
            || has_active_project_role(db, principal_id, project_id, "PROJECT_ADMIN", lock).await?
            || has_active_project_role(db, principal_id, project_id, "AGENT_DEVELOPER", lock)
                .await?,
    )
}

pub async fn project_organization(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    lock: bool,
) -> Result<Option<Uuid>, DbErr> {
    let sql = format!(
        "SELECT organization_id FROM projects WHERE id = $1{}",
        if lock { " FOR KEY SHARE" } else { "" }
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, vec![project_id.into()]);
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(row.try_get_by::<Uuid, _>("organization_id")?)),
        None => Ok(None),
    }
}

pub async fn active_project(
    db: &impl ConnectionTrait,
    project_id: Uuid,
    lock: bool,
) -> Result<bool, DbErr> {
    let sql = format!(
        "SELECT 1 FROM projects WHERE id = $1 AND lifecycle_status = 'ACTIVE'{}",
        if lock { " FOR KEY SHARE" } else { "" }
    );
    exists(db, &sql, vec![project_id.into()]).await
}

pub async fn authority_assignments(
    db: &impl ConnectionTrait,
    principal: Uuid,
    scoped_organization: Option<Uuid>,
    scoped_project: Option<Uuid>,
) -> Result<Vec<super::AuthorityAssignment>, DbErr> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
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
         SELECT organization_id, NULL::UUID AS project_id, TRUE AS approval_view, FALSE AS approval_decide \
         FROM assignments WHERE assignment_scope = 'ORGANIZATION' AND role_code IN ('ORGANIZATION_ADMIN', 'AUDITOR') \
         UNION ALL \
         SELECT organization_id, project_id, TRUE, role_code = 'DEPLOYMENT_APPROVER' \
         FROM assignments WHERE assignment_scope = 'PROJECT' AND role_code IN ('PROJECT_ADMIN', 'DEPLOYMENT_APPROVER', 'AUDITOR')",
        vec![principal.into(), scoped_organization.into(), scoped_project.into()],
    );
    let rows = db.query_all_raw(statement).await?;
    rows.into_iter()
        .map(|row| {
            Ok(super::AuthorityAssignment {
                organization_id: row.try_get_by::<Uuid, _>("organization_id")?,
                project_id: row.try_get_by::<Option<Uuid>, _>("project_id")?,
                approval_view: row.try_get_by::<bool, _>("approval_view")?,
                approval_decide: row.try_get_by::<bool, _>("approval_decide")?,
            })
        })
        .collect()
}
