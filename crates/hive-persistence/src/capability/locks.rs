use sea_orm::{ConnectionTrait, DbErr, Statement, Value};
use uuid::Uuid;

/// Ports the private `lock` helper: runs a `FOR UPDATE`/`FOR KEY SHARE` query and
/// drains every row, matching Java's `while (ignored.next()) {}` — the lock is
/// acquired as each row is fetched, so the whole result set must be consumed.
pub async fn lock(db: &impl ConnectionTrait, sql: &str, values: Vec<Value>) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(db.get_database_backend(), sql, values);
    db.query_all_raw(statement).await?;
    Ok(())
}

/// Ports `lockProjectRoleAuthority`.
pub async fn lock_project_role_authority(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<(), DbErr> {
    lock(
        db,
        "SELECT membership.id FROM organization_memberships membership \
         JOIN projects project ON project.organization_id = membership.organization_id \
         WHERE project.id = $1 AND membership.principal_id = $2 FOR UPDATE OF membership",
        vec![project_id.into(), principal_id.into()],
    )
    .await?;
    lock(
        db,
        "SELECT role.membership_id FROM organization_membership_roles role \
         JOIN organization_memberships membership ON membership.id = role.membership_id \
         JOIN projects project ON project.organization_id = membership.organization_id \
         WHERE project.id = $1 AND membership.principal_id = $2 FOR UPDATE OF role",
        vec![project_id.into(), principal_id.into()],
    )
    .await?;
    lock(
        db,
        "SELECT membership.id FROM project_memberships membership \
         WHERE membership.project_id = $1 AND membership.principal_id = $2 FOR UPDATE",
        vec![project_id.into(), principal_id.into()],
    )
    .await?;
    lock(
        db,
        "SELECT role.membership_id FROM project_membership_roles role \
         JOIN project_memberships membership ON membership.id = role.membership_id \
         WHERE membership.project_id = $1 AND membership.principal_id = $2 FOR UPDATE OF role",
        vec![project_id.into(), principal_id.into()],
    )
    .await
}

/// Ports `lockDeploymentAuthority`.
pub async fn lock_deployment_authority(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_id: Uuid,
) -> Result<(), DbErr> {
    lock(
        db,
        "SELECT platform.principal_id FROM platform_role_assignments platform WHERE platform.principal_id = $1 FOR UPDATE",
        vec![principal_id.into()],
    )
    .await?;
    lock_project_role_authority(db, principal_id, project_id).await
}

/// Ports `lockDeploymentApprovalAuthorityPage`.
pub async fn lock_deployment_approval_authority_page(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project_ids: &[Uuid],
) -> Result<(), DbErr> {
    let ids = Value::Array(
        sea_orm::sea_query::ArrayType::Uuid,
        Some(Box::new(
            project_ids.iter().map(|id| Value::from(*id)).collect(),
        )),
    );
    lock(
        db,
        "SELECT principal_id FROM platform_role_assignments WHERE principal_id = $1 FOR UPDATE",
        vec![principal_id.into()],
    )
    .await?;
    lock(
        db,
        "SELECT membership.id FROM organization_memberships membership \
         JOIN projects project ON project.organization_id = membership.organization_id \
         WHERE project.id = ANY($1) AND membership.principal_id = $2 \
         ORDER BY membership.organization_id, membership.id FOR UPDATE OF membership",
        vec![ids.clone(), principal_id.into()],
    )
    .await?;
    lock(
        db,
        "SELECT role.membership_id FROM organization_membership_roles role \
         JOIN organization_memberships membership ON membership.id = role.membership_id \
         JOIN projects project ON project.organization_id = membership.organization_id \
         WHERE project.id = ANY($1) AND membership.principal_id = $2 \
         ORDER BY role.membership_id, role.role_code FOR UPDATE OF role",
        vec![ids.clone(), principal_id.into()],
    )
    .await?;
    lock(
        db,
        "SELECT membership.id FROM project_memberships membership \
         WHERE membership.project_id = ANY($1) AND membership.principal_id = $2 \
         ORDER BY membership.project_id, membership.id FOR UPDATE",
        vec![ids.clone(), principal_id.into()],
    )
    .await?;
    lock(
        db,
        "SELECT role.membership_id FROM project_membership_roles role \
         JOIN project_memberships membership ON membership.id = role.membership_id \
         WHERE membership.project_id = ANY($1) AND membership.principal_id = $2 \
         ORDER BY membership.project_id, role.membership_id, role.role_code FOR UPDATE OF role",
        vec![ids, principal_id.into()],
    )
    .await
}
