//! Ports the nine `AdministrationRepository` write commands (`createProject`, `addMembership`,
//! `replaceMembership`, `endMembership`, `lifecycle`, `updateBudget`, `updateApprovalPolicy`,
//! `updateProjectGeneral`, `saveProjectConnection`) and every helper they call, including the
//! deployment/approval-domain scope-cache maintenance (`refreshMembershipScope` and friends) and
//! archive cascade (`invalidatePendingApprovalsForArchivedProject`,
//! `recordProjectArchiveEvent`) Administration's own writes must keep current, even though the
//! Deployment/approval feature area itself is not yet ported.
//!
//! Every write command re-evaluates the current server capability inside the same transaction it
//! mutates in (`tx_has_administration_capability` and its helpers below), matching Java's
//! `capabilities.hasCapability(connection, ..., true)` calls exactly: the capability check and the
//! resource lock share one transaction, so a role revoked concurrently cannot slip a mutation
//! through. `tx_has_administration_capability` covers exactly the capability codes these nine
//! commands check (every `ADMINISTRATION_CAPABILITIES` branch of `hasCapability` except the
//! `ORGANIZATION.VIEW` membership-visibility arm, which none of them ever queries) — it is not a
//! general replacement for `capability::has_capability`.
//!
//! An early return before `tx.commit()` relies on `sqlx::Transaction`'s `Drop` implementation to
//! roll back, rather than an explicit `tx.rollback().await` at every refusal branch — sqlx
//! guarantees an uncommitted transaction rolls back when dropped.

use super::queries;
use super::rows;
use hive_application::administration::{
    AdministrationMutationResult, AdministrationProblem,
    AdministrationRepositoryError as RepositoryError, AdministrationScope, ApprovalRule,
};
use sqlx::{PgConnection, PgPool};
use std::collections::BTreeMap;
use std::sync::LazyLock;
use uuid::Uuid;

static SLUG_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").unwrap());

async fn replace_roles(
    conn: &mut PgConnection,
    scope: AdministrationScope,
    membership_id: Uuid,
    roles: &[String],
) -> Result<(), sqlx::Error> {
    let table = rows::role_table(rows::scope_name(scope));
    sqlx::query(&format!("DELETE FROM {table} WHERE membership_id = $1"))
        .bind(membership_id)
        .execute(&mut *conn)
        .await?;
    let insert_sql = format!("INSERT INTO {table} (membership_id, role_code) VALUES ($1, $2)");
    for role in roles {
        sqlx::query(&insert_sql)
            .bind(membership_id)
            .bind(role)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

async fn principal_exists(conn: &mut PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("SELECT 1 FROM principals WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .is_some())
}

async fn project_organization_membership_exists(
    conn: &mut PgConnection,
    project_id: Uuid,
    member: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "SELECT 1 FROM projects project JOIN organization_memberships membership \
           ON membership.organization_id = project.organization_id \
         WHERE project.id = $1 AND membership.principal_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL FOR KEY SHARE",
    )
    .bind(project_id)
    .bind(member)
    .fetch_optional(&mut *conn)
    .await?
    .is_some())
}

async fn budget_revision_locked(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT current_revision FROM project_budget_policies WHERE project_id = $1 FOR UPDATE",
    )
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|(revision,)| revision).unwrap_or(-1))
}

// --- transaction-scoped capability re-check (see the module doc comment) ---
//
// `has_platform_admin`/`is_platform_administrator`/`has_active_organization_role`/
// `has_active_project_role`/`project_organization`/`active_project` are shared with
// `agent::draft`, which needs the identical `AGENT.VIEW`/`AGENT_DRAFT.*` primitives under its own
// write transaction; see `capability::tx`'s doc comment for why these can't just call
// `capability::has_capability(&pool, ...)`.
use crate::capability::tx::{
    active_project as tx_active_project,
    has_active_organization_role as tx_has_active_organization_role,
    has_active_project_role as tx_has_active_project_role,
    has_platform_admin as tx_has_platform_admin,
    is_platform_administrator as tx_is_platform_administrator,
    project_organization as tx_project_organization,
};

async fn tx_scope_exists(
    conn: &mut PgConnection,
    scope: AdministrationScope,
    id: Uuid,
) -> Result<bool, sqlx::Error> {
    let sql = format!(
        "SELECT 1 FROM {} WHERE id = $1 FOR KEY SHARE",
        rows::lifecycle_table(scope)
    );
    Ok(sqlx::query(&sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .is_some())
}

/// Ports the `ADMINISTRATION_CAPABILITIES` fallback branch of `hasCapability` with `lock`
/// hardcoded `true`, the only value the nine write commands ever pass. See the module doc comment
/// for its exact, narrower scope.
async fn tx_has_administration_capability(
    conn: &mut PgConnection,
    actor: Uuid,
    capability: &str,
    scope: AdministrationScope,
    scope_id: Uuid,
) -> Result<bool, sqlx::Error> {
    if !crate::capability::ADMINISTRATION_CAPABILITIES.contains(&capability) {
        return Ok(false);
    }
    if !tx_scope_exists(conn, scope, scope_id).await? {
        return Ok(false);
    }
    if tx_has_platform_admin(conn, actor).await? {
        return Ok(true);
    }
    match scope {
        AdministrationScope::Organization => {
            let is_admin =
                tx_has_active_organization_role(conn, actor, scope_id, "ORGANIZATION_ADMIN")
                    .await?;
            Ok(is_admin && crate::capability::ORGANIZATION_ADMIN.contains(&capability))
        }
        AdministrationScope::Project => {
            let project_id = scope_id;
            let organization_id = match tx_project_organization(conn, project_id).await? {
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
                && !tx_active_project(conn, project_id).await?
            {
                return Ok(false);
            }
            if tx_has_active_organization_role(conn, actor, organization_id, "ORGANIZATION_ADMIN")
                .await?
                && crate::capability::INHERITED_ORGANIZATION_ADMIN.contains(&capability)
            {
                return Ok(true);
            }
            if tx_has_active_organization_role(conn, actor, organization_id, "AUDITOR").await?
                && crate::capability::PROJECT_AUDITOR.contains(&capability)
            {
                return Ok(true);
            }
            if tx_has_active_project_role(conn, actor, project_id, "PROJECT_ADMIN").await?
                && crate::capability::PROJECT_ADMIN.contains(&capability)
            {
                return Ok(true);
            }
            if tx_has_active_project_role(conn, actor, project_id, "AUDITOR").await?
                && crate::capability::PROJECT_AUDITOR.contains(&capability)
            {
                return Ok(true);
            }
            if tx_has_active_project_role(conn, actor, project_id, "DEPLOYMENT_APPROVER").await?
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
            let developer_or_operator =
                tx_has_active_project_role(conn, actor, project_id, "AGENT_DEVELOPER").await?
                    || tx_has_active_project_role(conn, actor, project_id, "OPERATOR").await?;
            Ok(developer_or_operator && capability == "PROJECT.VIEW")
        }
    }
}

// --- audit, and the deployment/approval-domain scope caches administration writes maintain ---

#[allow(clippy::too_many_arguments)]
async fn audit(
    conn: &mut PgConnection,
    actor: Uuid,
    scope: &str,
    scope_id: Uuid,
    action: &str,
    reason: Option<&str>,
    before_digest: Option<&str>,
    after_digest: Option<&str>,
    material: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let mut facts: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    facts.insert(
        "action".to_string(),
        serde_json::Value::String(action.to_string()),
    );
    facts.insert(
        "actorPrincipalId".to_string(),
        serde_json::Value::String(actor.to_string()),
    );
    if let Some(after) = after_digest {
        facts.insert(
            "afterDigest".to_string(),
            serde_json::Value::String(after.to_string()),
        );
    }
    if let Some(before) = before_digest {
        facts.insert(
            "beforeDigest".to_string(),
            serde_json::Value::String(before.to_string()),
        );
    }
    facts.insert("material".to_string(), material);
    if let Some(reason) = reason {
        facts.insert(
            "reason".to_string(),
            serde_json::Value::String(reason.to_string()),
        );
    }
    facts.insert(
        "scopeId".to_string(),
        serde_json::Value::String(scope_id.to_string()),
    );
    facts.insert(
        "scopeType".to_string(),
        serde_json::Value::String(scope.to_string()),
    );
    let facts_json =
        serde_json::to_string(&facts).expect("administration audit facts always serialize");

    crate::audit::bind_audit_metadata(
        sqlx::query(
            "INSERT INTO administration_audit_events \
             (id, actor_principal_id, scope_type, scope_id, action, reason, before_digest, after_digest, facts, \
              request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10, $11, $12, $13, $14)",
        )
        .bind(Uuid::new_v4())
        .bind(actor)
        .bind(scope)
        .bind(scope_id)
        .bind(action)
        .bind(reason)
        .bind(before_digest)
        .bind(after_digest)
        .bind(facts_json),
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn mark_approval_role_assignment(
    conn: &mut PgConnection,
    actor: Uuid,
    scope: AdministrationScope,
    previous: &[String],
    next: &[String],
) -> Result<(), sqlx::Error> {
    let deployment_approver = "DEPLOYMENT_APPROVER".to_string();
    if scope != AdministrationScope::Project
        || previous.contains(&deployment_approver) == next.contains(&deployment_approver)
    {
        return Ok(());
    }
    sqlx::query("SELECT set_config('hive.m14_approval_role_assignment_actor', $1, TRUE)")
        .bind(actor.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn approval_role_transition_allowed(
    conn: &mut PgConnection,
    actor: Uuid,
    scope: AdministrationScope,
    previous: &[String],
    next: &[String],
) -> Result<bool, sqlx::Error> {
    let deployment_approver = "DEPLOYMENT_APPROVER".to_string();
    if scope != AdministrationScope::Project
        || previous.contains(&deployment_approver) == next.contains(&deployment_approver)
    {
        return Ok(true);
    }
    tx_is_platform_administrator(conn, actor).await
}

async fn refresh_membership_scope(
    conn: &mut PgConnection,
    scope: AdministrationScope,
    principal: Uuid,
    scope_id: Uuid,
) -> Result<(), sqlx::Error> {
    match scope {
        AdministrationScope::Organization => {
            refresh_organization_membership_scope(conn, principal, scope_id).await?;
            refresh_organization_scope(conn, principal, scope_id).await?;
            refresh_organization_project_scopes(conn, principal, scope_id).await?;
        }
        AdministrationScope::Project => refresh_project_scope(conn, principal, scope_id).await?,
    }
    Ok(())
}

async fn refresh_role_scope(
    conn: &mut PgConnection,
    scope: AdministrationScope,
    principal: Uuid,
    scope_id: Uuid,
) -> Result<(), sqlx::Error> {
    match scope {
        AdministrationScope::Organization => {
            refresh_organization_scope(conn, principal, scope_id).await
        }
        AdministrationScope::Project => refresh_project_scope(conn, principal, scope_id).await,
    }
}

async fn refresh_organization_membership_scope(
    conn: &mut PgConnection,
    principal: Uuid,
    organization: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM deployment_approval_principal_organization_membership_scopes WHERE principal_id = $1 AND organization_id = $2")
        .bind(principal)
        .bind(organization)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO deployment_approval_principal_organization_membership_scopes (principal_id, organization_id, valid_after) \
         SELECT $1, $2, membership.started_at FROM organization_memberships membership \
         WHERE membership.principal_id = $1 AND membership.organization_id = $2 \
           AND membership.ended_at IS NULL AND membership.started_at <= CURRENT_TIMESTAMP",
    )
    .bind(principal)
    .bind(organization)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn refresh_organization_scope(
    conn: &mut PgConnection,
    principal: Uuid,
    organization: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM deployment_approval_principal_organization_scopes WHERE principal_id = $1 AND organization_id = $2")
        .bind(principal)
        .bind(organization)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO deployment_approval_principal_organization_scopes (principal_id, organization_id, valid_after) \
         SELECT $1, $2, MIN(membership.started_at) FROM organization_memberships membership \
           JOIN organization_membership_roles role ON role.membership_id = membership.id \
         WHERE membership.principal_id = $1 AND membership.organization_id = $2 AND membership.ended_at IS NULL \
           AND role.role_code IN ('ORGANIZATION_ADMIN', 'AUDITOR') \
         GROUP BY membership.principal_id",
    )
    .bind(principal)
    .bind(organization)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn refresh_project_scope(
    conn: &mut PgConnection,
    principal: Uuid,
    project: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM deployment_approval_principal_project_scopes WHERE principal_id = $1 AND project_id = $2")
        .bind(principal)
        .bind(project)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO deployment_approval_principal_project_scopes (principal_id, project_id, valid_after) \
         SELECT $1, $2, MIN(GREATEST(membership.started_at, organization_membership.started_at)) \
         FROM project_memberships membership \
           JOIN project_membership_roles role ON role.membership_id = membership.id \
           JOIN projects project ON project.id = membership.project_id \
           JOIN organization_memberships organization_membership \
             ON organization_membership.organization_id = project.organization_id AND organization_membership.principal_id = $1 \
         WHERE membership.principal_id = $1 AND membership.project_id = $2 AND membership.ended_at IS NULL \
           AND organization_membership.ended_at IS NULL AND role.role_code IN ('PROJECT_ADMIN', 'DEPLOYMENT_APPROVER', 'AUDITOR') \
         GROUP BY membership.principal_id, membership.project_id",
    )
    .bind(principal)
    .bind(project)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn refresh_organization_project_scopes(
    conn: &mut PgConnection,
    principal: Uuid,
    organization: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM deployment_approval_principal_project_scopes scope USING projects project \
         WHERE scope.principal_id = $1 AND project.id = scope.project_id AND project.organization_id = $2",
    )
    .bind(principal)
    .bind(organization)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO deployment_approval_principal_project_scopes (principal_id, project_id, valid_after) \
         SELECT $1, membership.project_id, MIN(GREATEST(membership.started_at, organization_membership.started_at)) \
         FROM project_memberships membership \
           JOIN project_membership_roles role ON role.membership_id = membership.id \
           JOIN projects project ON project.id = membership.project_id \
           JOIN organization_memberships organization_membership \
             ON organization_membership.organization_id = project.organization_id AND organization_membership.principal_id = $1 \
         WHERE membership.principal_id = $1 AND project.organization_id = $2 AND membership.ended_at IS NULL \
           AND organization_membership.ended_at IS NULL AND role.role_code IN ('PROJECT_ADMIN', 'DEPLOYMENT_APPROVER', 'AUDITOR') \
         GROUP BY membership.project_id",
    )
    .bind(principal)
    .bind(organization)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn invalidate_pending_approvals_for_archived_project(
    conn: &mut PgConnection,
    project_id: Uuid,
    actor: Uuid,
) -> Result<(), sqlx::Error> {
    let candidates: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT requirement.id AS requirement_id, deployment.id AS deployment_id \
         FROM deployments deployment JOIN deployment_approval_requirements requirement ON requirement.deployment_id = deployment.id \
         WHERE deployment.project_id = $1 AND deployment.lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED') AND requirement.status = 'PENDING' \
         ORDER BY deployment.id ASC FOR UPDATE OF deployment, requirement",
    )
    .bind(project_id)
    .fetch_all(&mut *conn)
    .await?;

    for (requirement_id, deployment_id) in candidates {
        let invalidated = sqlx::query(
            "UPDATE deployment_approval_requirements SET status = 'INVALIDATED', revision = revision + 1, \
               invalidated_at = CURRENT_TIMESTAMP, invalidation_code = 'PROJECT_ARCHIVED' WHERE id = $1 AND status = 'PENDING'",
        )
        .bind(requirement_id)
        .execute(&mut *conn)
        .await?;
        if invalidated.rows_affected() == 0 {
            continue;
        }

        let sequence: (i64,) = sqlx::query_as(
            "INSERT INTO deployment_timeline_counters (deployment_id, attempt_number, next_sequence) VALUES ($1, 0, 2) \
             ON CONFLICT (deployment_id, attempt_number) DO UPDATE SET next_sequence = deployment_timeline_counters.next_sequence + 1 \
             RETURNING next_sequence - 1",
        )
        .bind(deployment_id)
        .fetch_one(&mut *conn)
        .await?;

        crate::audit::bind_audit_metadata(
            sqlx::query(
                "INSERT INTO deployment_audit_events \
                   (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
                    request_id, correlation_id, graphql_operation, source_ip, user_agent) \
                 VALUES ($1, $2, $3, 'APPROVAL_INVALIDATED', jsonb_build_object('requirementId', $4, 'code', 'PROJECT_ARCHIVED'), NULL, 0, $5, $6, $7, $8, $9, $10)",
            )
            .bind(Uuid::new_v4())
            .bind(deployment_id)
            .bind(actor)
            .bind(requirement_id.to_string())
            .bind(sequence.0),
        )
        .execute(&mut *conn)
        .await?;

        sqlx::query(
            "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Project archive terminalized this pending approval cycle.', \
               observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
        )
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;

        sqlx::query(
            "UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, projection_revision = projection_revision + 1, \
               updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')",
        )
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;

        sqlx::query(
            "UPDATE deployments SET projection_revision = projection_revision + 1 WHERE id = $1",
        )
        .bind(deployment_id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

async fn record_project_archive_event(
    conn: &mut PgConnection,
    project_id: Uuid,
    actor: Uuid,
    archived_project_revision: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO deployment_approval_project_archive_events (id, project_id, actor_principal_id, archived_project_revision) VALUES ($1, $2, $3, $4)")
        .bind(Uuid::new_v4())
        .bind(project_id)
        .bind(actor)
        .bind(archived_project_revision)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn insert_policy_version(
    conn: &mut PgConnection,
    policy_id: Uuid,
    revision: i64,
    matrix: &BTreeMap<String, ApprovalRule>,
    reason: &str,
) -> Result<(), sqlx::Error> {
    let canonical = rows::matrix_json(matrix);
    let canonical_digest = rows::digest(&canonical);
    sqlx::query("INSERT INTO project_approval_policy_versions (policy_id, revision, digest, matrix, change_reason) VALUES ($1, $2, $3, $4::jsonb, $5)")
        .bind(policy_id)
        .bind(revision)
        .bind(canonical_digest)
        .bind(canonical)
        .bind(reason)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn create_project(
    pool: &PgPool,
    actor: Uuid,
    organization_id: Uuid,
    expected_revision: i64,
    slug: String,
    display_name: String,
    description: Option<String>,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;

    if !tx_has_administration_capability(
        &mut tx,
        actor,
        "PROJECT.CREATE",
        AdministrationScope::Organization,
        organization_id,
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(organization_row) =
        rows::locked_scope_row(&mut tx, AdministrationScope::Organization, organization_id)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?
    else {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    };
    if organization_row.revision != expected_revision {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::conflict(
                organization_id.to_string(),
                expected_revision,
                organization_row.revision,
            ),
        ));
    }
    if organization_row.status != "ACTIVE" {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }
    if slug.trim().is_empty() || display_name.trim().is_empty() || !SLUG_PATTERN.is_match(&slug) {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::invalid(),
        ));
    }

    let project_id = Uuid::new_v4();
    let description = description.unwrap_or_default();
    let insert = sqlx::query(
        "INSERT INTO projects (id, organization_id, slug, display_name, description, lifecycle_status, revision) \
         VALUES ($1, $2, $3, $4, $5, 'ACTIVE', 1)",
    )
    .bind(project_id)
    .bind(organization_id)
    .bind(&slug)
    .bind(display_name.trim())
    .bind(description.trim())
    .execute(&mut *tx)
    .await;
    if let Err(error) = insert {
        if rows::is_unique_violation(&error) {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        return Err(RepositoryError::Other(error.into()));
    }
    sqlx::query("INSERT INTO project_budget_policies (project_id) VALUES ($1)")
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    sqlx::query("INSERT INTO project_approval_policies (id, project_id, current_revision) VALUES ($1, $2, 1)")
        .bind(project_id)
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let matrix = rows::default_matrix();
    insert_policy_version(
        &mut tx,
        project_id,
        1,
        &matrix,
        "Initial fixed local P-05 policy",
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    audit(
        &mut tx,
        actor,
        "PROJECT",
        project_id,
        "PROJECT_APPROVAL_POLICY_CREATED",
        Some("Initial fixed local P-05 policy"),
        None,
        Some(&rows::digest(&rows::matrix_json(&matrix))),
        serde_json::json!({"revision": 1, "policyId": project_id.to_string()}),
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    audit(
        &mut tx,
        actor,
        "PROJECT",
        project_id,
        "PROJECT_CREATED",
        None,
        None,
        Some(&rows::digest(&project_id.to_string())),
        serde_json::json!({"slug": slug, "displayName": display_name.trim()}),
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;

    tx.commit()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let value = queries::project(pool, actor, project_id).await?;
    Ok(AdministrationMutationResult::project(
        value.expect("project visible to its own creator"),
    ))
}

pub async fn add_membership(
    pool: &PgPool,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
    member: Uuid,
    role_codes: Vec<String>,
    expected_scope_revision: i64,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let capability = format!("{}_MEMBERSHIP.ADD", rows::scope_name(scope));
    if !tx_has_administration_capability(&mut tx, actor, &capability, scope, scope_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(owner) = rows::locked_scope_row(&mut tx, scope, scope_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    else {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    };
    if owner.revision != expected_scope_revision {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::conflict(
                scope_id.to_string(),
                expected_scope_revision,
                owner.revision,
            ),
        ));
    }
    if scope != AdministrationScope::Organization && owner.status != "ACTIVE" {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }
    if !principal_exists(&mut tx, member)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    }
    if scope == AdministrationScope::Project
        && !project_organization_membership_exists(&mut tx, scope_id, member)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::invalid(),
        ));
    }
    if !approval_role_transition_allowed(&mut tx, actor, scope, &[], &role_codes)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }

    let membership_id = Uuid::new_v4();
    let table = rows::membership_table(rows::scope_name(scope));
    let column = rows::scope_id_column(scope);
    let insert_sql = format!("INSERT INTO {table} (id, {column}, principal_id, started_at, ended_at, revision, active_marker) VALUES ($1, $2, $3, CURRENT_TIMESTAMP, NULL, 1, TRUE)");
    let insert = sqlx::query(&insert_sql)
        .bind(membership_id)
        .bind(scope_id)
        .bind(member)
        .execute(&mut *tx)
        .await;
    if let Err(error) = insert {
        if rows::is_unique_violation(&error) {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        return Err(RepositoryError::Other(error.into()));
    }
    mark_approval_role_assignment(&mut tx, actor, scope, &[], &role_codes)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    replace_roles(&mut tx, scope, membership_id, &role_codes)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    refresh_membership_scope(&mut tx, scope, member, scope_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    audit(
        &mut tx,
        actor,
        rows::scope_name(scope),
        scope_id,
        &format!("{}_MEMBERSHIP_ADDED", rows::scope_name(scope)),
        None,
        None,
        Some(&rows::digest(&rows::canonical_roles(&role_codes))),
        serde_json::json!({"membershipId": membership_id.to_string(), "memberPrincipalId": member.to_string(), "roleCodes": role_codes}),
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;

    tx.commit()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    queries::result_for(pool, actor, scope, scope_id).await
}

pub async fn replace_membership(
    pool: &PgPool,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
    membership_id: Uuid,
    role_codes: Vec<String>,
    expected_revision: i64,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let capability = format!("{}_MEMBERSHIP.CHANGE_ROLES", rows::scope_name(scope));
    if !tx_has_administration_capability(&mut tx, actor, &capability, scope, scope_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(membership) = rows::locked_membership(&mut tx, scope, scope_id, membership_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    else {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    };
    if membership.revision != expected_revision {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::conflict(
                membership_id.to_string(),
                expected_revision,
                membership.revision,
            ),
        ));
    }
    if membership.ended {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }
    if scope == AdministrationScope::Project
        && !tx_active_project(&mut tx, scope_id)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }

    let previous = rows::current_roles_tx(&mut tx, scope, membership_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    if previous == role_codes {
        tx.commit()
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;
        return queries::result_for(pool, actor, scope, scope_id).await;
    }
    if !approval_role_transition_allowed(&mut tx, actor, scope, &previous, &role_codes)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    mark_approval_role_assignment(&mut tx, actor, scope, &previous, &role_codes)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    replace_roles(&mut tx, scope, membership_id, &role_codes)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    refresh_role_scope(&mut tx, scope, membership.principal_id, scope_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let table = rows::membership_table(rows::scope_name(scope));
    sqlx::query(&format!(
        "UPDATE {table} SET revision = revision + 1 WHERE id = $1"
    ))
    .bind(membership_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    audit(
        &mut tx,
        actor,
        rows::scope_name(scope),
        scope_id,
        &format!("{}_MEMBERSHIP_ROLES_REPLACED", rows::scope_name(scope)),
        None,
        Some(&rows::digest(&rows::canonical_roles(&previous))),
        Some(&rows::digest(&rows::canonical_roles(&role_codes))),
        serde_json::json!({"membershipId": membership_id.to_string(), "roleCodes": role_codes}),
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;

    tx.commit()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    queries::result_for(pool, actor, scope, scope_id).await
}

pub async fn end_membership(
    pool: &PgPool,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
    membership_id: Uuid,
    expected_revision: i64,
    reason: String,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let capability = format!("{}_MEMBERSHIP.END", rows::scope_name(scope));
    if !tx_has_administration_capability(&mut tx, actor, &capability, scope, scope_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(membership) = rows::locked_membership(&mut tx, scope, scope_id, membership_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    else {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    };
    if membership.revision != expected_revision {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::conflict(
                membership_id.to_string(),
                expected_revision,
                membership.revision,
            ),
        ));
    }
    if membership.ended {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }
    let previous = rows::current_roles_tx(&mut tx, scope, membership_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    if !approval_role_transition_allowed(&mut tx, actor, scope, &previous, &[])
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    mark_approval_role_assignment(&mut tx, actor, scope, &previous, &[])
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let table = rows::membership_table(rows::scope_name(scope));
    sqlx::query(&format!("UPDATE {table} SET ended_at = CURRENT_TIMESTAMP, revision = revision + 1, active_marker = NULL WHERE id = $1"))
        .bind(membership_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    refresh_membership_scope(&mut tx, scope, membership.principal_id, scope_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let before = format!(
        "revision={} ended={} principalId={}",
        membership.revision, membership.ended, membership.principal_id
    );
    audit(
        &mut tx,
        actor,
        rows::scope_name(scope),
        scope_id,
        &format!("{}_MEMBERSHIP_ENDED", rows::scope_name(scope)),
        Some(&reason),
        Some(&rows::digest(&before)),
        None,
        serde_json::json!({"membershipId": membership_id.to_string()}),
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;

    tx.commit()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    queries::result_for(pool, actor, scope, scope_id).await
}

#[allow(clippy::too_many_arguments)]
pub async fn lifecycle(
    pool: &PgPool,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
    expected_revision: i64,
    reason: Option<String>,
    confirmation: Option<String>,
    archive: bool,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let action = format!(
        "{}.{}",
        rows::scope_name(scope),
        if archive { "ARCHIVE" } else { "RESTORE" }
    );
    if !tx_has_administration_capability(&mut tx, actor, &action, scope, scope_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(current) = rows::locked_scope_row(&mut tx, scope, scope_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    else {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    };
    if current.revision != expected_revision {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::conflict(
                scope_id.to_string(),
                expected_revision,
                current.revision,
            ),
        ));
    }
    if (archive && current.status != "ACTIVE") || (!archive && current.status != "ARCHIVED") {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }
    if archive && scope == AdministrationScope::Organization {
        let confirmed = confirmation.as_deref().map(str::trim) == Some(current.slug.as_str());
        if !confirmed {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::protected_lifecycle(),
            ));
        }
    }

    let next = if archive { "ARCHIVED" } else { "ACTIVE" };
    let table = rows::lifecycle_table(scope);
    sqlx::query(&format!(
        "UPDATE {table} SET lifecycle_status = $1, revision = revision + 1 WHERE id = $2"
    ))
    .bind(next)
    .bind(scope_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    if archive && scope == AdministrationScope::Project {
        invalidate_pending_approvals_for_archived_project(&mut tx, scope_id, actor)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;
        record_project_archive_event(&mut tx, scope_id, actor, current.revision + 1)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;
    }
    let before = format!(
        "slug={} status={} revision={}",
        current.slug, current.status, current.revision
    );
    let after = format!("{next}{}", current.revision);
    audit(
        &mut tx,
        actor,
        rows::scope_name(scope),
        scope_id,
        &format!(
            "{}_{}",
            rows::scope_name(scope),
            if archive { "ARCHIVED" } else { "RESTORED" }
        ),
        reason.as_deref(),
        Some(&rows::digest(&before)),
        Some(&rows::digest(&after)),
        serde_json::json!({"lifecycleStatus": next}),
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;

    tx.commit()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    queries::result_for(pool, actor, scope, scope_id).await
}

#[allow(clippy::too_many_arguments)]
pub async fn update_budget(
    pool: &PgPool,
    actor: Uuid,
    project_id: Uuid,
    expected_revision: i64,
    currency: String,
    monthly_limit_cents: i32,
    warning_threshold_cents: i32,
    reason: String,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    if !tx_has_administration_capability(
        &mut tx,
        actor,
        "PROJECT_BUDGET.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    if !tx_active_project(&mut tx, project_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }
    let current_revision = budget_revision_locked(&mut tx, project_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    if current_revision < 0 {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    }
    if current_revision != expected_revision {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::conflict(
                project_id.to_string(),
                expected_revision,
                current_revision,
            ),
        ));
    }
    let prior = rows::current_budget_tx(&mut tx, project_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let unchanged = prior.as_ref().is_some_and(|prior| {
        prior.currency == currency
            && prior.monthly_limit_cents == monthly_limit_cents
            && prior.warning_threshold_cents == warning_threshold_cents
    });
    if unchanged {
        tx.commit()
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;
        let value = queries::project(pool, actor, project_id).await?;
        return Ok(AdministrationMutationResult::project(
            value.expect("project visible inside its own budget update"),
        ));
    }
    let next_revision = current_revision + 1;
    sqlx::query(
        "INSERT INTO project_budget_policy_versions (project_id, revision, currency, monthly_limit_cents, warning_threshold_cents, change_reason) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(project_id)
    .bind(next_revision)
    .bind(&currency)
    .bind(monthly_limit_cents)
    .bind(warning_threshold_cents)
    .bind(&reason)
    .execute(&mut *tx)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    sqlx::query("UPDATE project_budget_policies SET current_revision = $1 WHERE project_id = $2")
        .bind(next_revision)
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let before_digest = prior.as_ref().map(|prior| {
        rows::digest(&format!(
            "{}|{}|{}",
            prior.currency, prior.monthly_limit_cents, prior.warning_threshold_cents
        ))
    });
    audit(
        &mut tx,
        actor,
        "PROJECT",
        project_id,
        "PROJECT_BUDGET_POLICY_UPDATED",
        Some(&reason),
        before_digest.as_deref(),
        Some(&rows::digest(&format!("{currency}|{monthly_limit_cents}|{warning_threshold_cents}"))),
        serde_json::json!({"revision": next_revision, "currency": currency, "monthlyLimitCents": monthly_limit_cents, "warningThresholdCents": warning_threshold_cents}),
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;

    tx.commit()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let value = queries::project(pool, actor, project_id).await?;
    Ok(AdministrationMutationResult::project(
        value.expect("project visible inside its own budget update"),
    ))
}

pub async fn update_approval_policy(
    pool: &PgPool,
    actor: Uuid,
    project_id: Uuid,
    expected_revision: i64,
    matrix: BTreeMap<String, ApprovalRule>,
    reason: String,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    if !tx_has_administration_capability(
        &mut tx,
        actor,
        "PROJECT_APPROVAL_POLICY.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    if !tx_active_project(&mut tx, project_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }
    let Some(prior) = rows::current_approval_tx(&mut tx, project_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    else {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    };
    if prior.revision != expected_revision {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::conflict(
                project_id.to_string(),
                expected_revision,
                prior.revision,
            ),
        ));
    }
    if rows::weakens(&prior.matrix, &matrix) {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::weakening(),
        ));
    }
    let canonical = rows::matrix_json(&matrix);
    let next_digest = rows::digest(&canonical);
    if next_digest == prior.digest {
        tx.commit()
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;
        let value = queries::project(pool, actor, project_id).await?;
        return Ok(AdministrationMutationResult::project(
            value.expect("project visible inside its own approval policy update"),
        ));
    }

    let next_revision = expected_revision + 1;
    insert_policy_version(&mut tx, prior.id, next_revision, &matrix, &reason)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    sqlx::query("UPDATE project_approval_policies SET current_revision = $1 WHERE id = $2")
        .bind(next_revision)
        .bind(prior.id)
        .execute(&mut *tx)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    audit(
        &mut tx,
        actor,
        "PROJECT",
        project_id,
        "PROJECT_APPROVAL_POLICY_UPDATED",
        Some(&reason),
        Some(&prior.digest),
        Some(&next_digest),
        serde_json::json!({"policyId": prior.id.to_string(), "revision": next_revision, "matrixDigest": next_digest}),
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;

    tx.commit()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let value = queries::project(pool, actor, project_id).await?;
    Ok(AdministrationMutationResult::project(value.expect(
        "project visible inside its own approval policy update",
    )))
}

pub async fn update_project_general(
    pool: &PgPool,
    actor: Uuid,
    project_id: Uuid,
    expected_revision: i64,
    display_name: String,
    description: String,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    if !tx_has_administration_capability(
        &mut tx,
        actor,
        "PROJECT.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(current) = rows::locked_scope_row(&mut tx, AdministrationScope::Project, project_id)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?
    else {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    };
    if current.revision != expected_revision {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::conflict(
                project_id.to_string(),
                expected_revision,
                current.revision,
            ),
        ));
    }
    if current.status != "ACTIVE" {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }
    sqlx::query("UPDATE projects SET display_name = $1, description = $2, revision = revision + 1 WHERE id = $3")
        .bind(&display_name)
        .bind(&description)
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let before = format!(
        "slug={} status={} revision={}",
        current.slug, current.status, current.revision
    );
    audit(
        &mut tx,
        actor,
        "PROJECT",
        project_id,
        "PROJECT_GENERAL_UPDATED",
        None,
        Some(&rows::digest(&before)),
        Some(&rows::digest(&format!("{display_name}|{description}"))),
        serde_json::json!({"displayName": display_name, "description": description}),
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;

    tx.commit()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let value = queries::project(pool, actor, project_id).await?;
    Ok(AdministrationMutationResult::project(
        value.expect("project visible inside its own general update"),
    ))
}

#[allow(clippy::too_many_arguments)]
pub async fn save_project_connection(
    pool: &PgPool,
    actor: Uuid,
    project_id: Uuid,
    connection_id: Option<Uuid>,
    expected_revision: i64,
    display_name: String,
    definition_version: String,
    environment: String,
    credential_status: String,
    lifecycle_status: String,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    if !tx_has_administration_capability(
        &mut tx,
        actor,
        "PROJECT.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(project_row) =
        rows::locked_scope_row(&mut tx, AdministrationScope::Project, project_id)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?
    else {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    };
    let prior = match connection_id {
        None => None,
        Some(id) => {
            let Some(prior) = rows::locked_settings_connection(&mut tx, project_id, id)
                .await
                .map_err(|error| RepositoryError::Other(error.into()))?
            else {
                return Ok(AdministrationMutationResult::refused(
                    AdministrationProblem::unavailable(),
                ));
            };
            Some(prior)
        }
    };

    match prior {
        None => {
            if expected_revision != 0 || project_row.status != "ACTIVE" {
                return Ok(AdministrationMutationResult::refused(
                    AdministrationProblem::protected_lifecycle(),
                ));
            }
            let new_connection_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO project_settings_connections \
                 (id, project_id, display_name, definition_version, environment, credential_status, lifecycle_status, revision) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 1)",
            )
            .bind(new_connection_id)
            .bind(project_id)
            .bind(&display_name)
            .bind(&definition_version)
            .bind(&environment)
            .bind(&credential_status)
            .bind(&lifecycle_status)
            .execute(&mut *tx)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;
            audit(
                &mut tx,
                actor,
                "PROJECT",
                project_id,
                "PROJECT_CONNECTION_CREATED",
                None,
                None,
                Some(&rows::digest(&rows::connection_facts(&display_name, &definition_version, &environment, &credential_status, &lifecycle_status))),
                serde_json::json!({
                    "connectionId": new_connection_id.to_string(), "displayName": display_name, "definitionVersion": definition_version,
                    "environment": environment, "credentialStatus": credential_status, "lifecycleStatus": lifecycle_status,
                }),
            )
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;
            tx.commit()
                .await
                .map_err(|error| RepositoryError::Other(error.into()))?;
        }
        Some(prior) => {
            let connection_id =
                connection_id.expect("prior is Some only when connection_id was Some");
            if prior.revision != expected_revision {
                return Ok(AdministrationMutationResult::refused(
                    AdministrationProblem::conflict(
                        connection_id.to_string(),
                        expected_revision,
                        prior.revision,
                    ),
                ));
            }
            if rows::same_connection(
                &prior,
                &display_name,
                &definition_version,
                &environment,
                &credential_status,
                &lifecycle_status,
            ) {
                tx.commit()
                    .await
                    .map_err(|error| RepositoryError::Other(error.into()))?;
                let value = queries::project(pool, actor, project_id).await?;
                return Ok(AdministrationMutationResult::project(
                    value.expect("project visible inside its own connection save"),
                ));
            }
            if project_row.status != "ACTIVE"
                && (!rows::same_metadata(
                    &prior,
                    &display_name,
                    &definition_version,
                    &environment,
                    &credential_status,
                ) || !rows::safety_reducing(&prior.lifecycle_status, &lifecycle_status))
            {
                return Ok(AdministrationMutationResult::refused(
                    AdministrationProblem::protected_lifecycle(),
                ));
            }
            sqlx::query(
                "UPDATE project_settings_connections SET display_name = $1, definition_version = $2, environment = $3, \
                   credential_status = $4, lifecycle_status = $5, revision = revision + 1 WHERE id = $6",
            )
            .bind(&display_name)
            .bind(&definition_version)
            .bind(&environment)
            .bind(&credential_status)
            .bind(&lifecycle_status)
            .bind(connection_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;
            let before = format!(
                "{}|{}|{}|{}|{}",
                prior.display_name,
                prior.definition_version,
                prior.environment,
                prior.credential_status,
                prior.lifecycle_status
            );
            audit(
                &mut tx,
                actor,
                "PROJECT",
                project_id,
                "PROJECT_CONNECTION_UPDATED",
                None,
                Some(&rows::digest(&before)),
                Some(&rows::digest(&rows::connection_facts(&display_name, &definition_version, &environment, &credential_status, &lifecycle_status))),
                serde_json::json!({
                    "connectionId": connection_id.to_string(), "displayName": display_name, "definitionVersion": definition_version,
                    "environment": environment, "credentialStatus": credential_status, "lifecycleStatus": lifecycle_status,
                }),
            )
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;
            tx.commit()
                .await
                .map_err(|error| RepositoryError::Other(error.into()))?;
        }
    }

    let value = queries::project(pool, actor, project_id).await?;
    Ok(AdministrationMutationResult::project(
        value.expect("project visible inside its own connection save"),
    ))
}
