//! Ports the nine `AdministrationRepository` write commands (`createProject`, `addMembership`,
//! `replaceMembership`, `endMembership`, `lifecycle`, `updateBudget`, `updateApprovalPolicy`,
//! `updateProjectGeneral`, `saveProjectConnection`) and every helper they call, including the
//! deployment/approval-domain scope-cache maintenance (`refreshMembershipScope` and friends) and
//! archive cascade (`invalidatePendingApprovalsForArchivedProject`,
//! `recordProjectArchiveEvent`) Administration's own writes must keep current, even though the
//! Deployment/approval feature area itself is not yet ported.
//!
//! `GSR-PERSISTENCE`: every write command opens a `sea_orm::DatabaseTransaction` via
//! `TransactionTrait::begin`, and every locked capability re-check calls
//! `capability::has_capability`/`capability::queries::*` directly with `lock: true` and `&txn` —
//! the same pattern `configuration`'s port established (`GSR-PHASE-P6`). The old
//! `tx_has_administration_capability` local helper (a hand-duplicated, deliberately-narrowed twin
//! of `hasCapability`'s own `ADMINISTRATION_CAPABILITIES` fallback branch, omitting the
//! `ORGANIZATION.VIEW`-via-membership arm since none of these nine commands ever check that
//! specific capability) is gone: `capability::has_capability` is provably equivalent for every
//! capability string these commands actually pass, so it is called directly instead of
//! re-implementing the same branch a second time.
//!
//! An early return before `txn.commit()` relies on `DatabaseTransaction`'s `Drop` implementation
//! to roll back, rather than an explicit `txn.rollback().await` at every refusal branch — `sea_orm`
//! guarantees an uncommitted transaction rolls back when dropped, the same as `sqlx`.

use super::queries;
use super::rows;
use crate::capability::{self, Scope};
use hive_application::administration::{
    AdministrationMutationResult, AdministrationProblem,
    AdministrationRepositoryError as RepositoryError, AdministrationScope, ApprovalRule,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, TransactionTrait};
use std::collections::BTreeMap;
use std::sync::LazyLock;
use uuid::Uuid;

static SLUG_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").unwrap());

fn other(error: DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

fn as_capability_scope(scope: AdministrationScope, scope_id: Uuid) -> Scope {
    match scope {
        AdministrationScope::Organization => Scope::Organization(scope_id),
        AdministrationScope::Project => Scope::Project(scope_id),
    }
}

/// Ports the `ADMINISTRATION_CAPABILITIES` fallback branch of `hasCapability`, locked — see the
/// module doc comment: this now calls the shared, already-ported `capability::has_capability`
/// directly instead of re-implementing the branch.
async fn tx_has_administration_capability(
    txn: &impl ConnectionTrait,
    actor: Uuid,
    capability: &str,
    scope: AdministrationScope,
    scope_id: Uuid,
) -> Result<bool, DbErr> {
    capability::has_capability(
        txn,
        actor,
        capability,
        as_capability_scope(scope, scope_id),
        true,
    )
    .await
}

async fn tx_active_project(txn: &impl ConnectionTrait, project_id: Uuid) -> Result<bool, DbErr> {
    capability::queries::active_project(txn, project_id, true).await
}

/// Locked: unlike `capability::is_platform_administrator` (always `lock: false`), this check
/// guards an approval-role transition inside a write transaction and must hold the row until
/// commit, so it calls `capability::queries::has_platform_admin` directly with `lock: true`.
async fn tx_is_platform_administrator(
    txn: &impl ConnectionTrait,
    actor: Uuid,
) -> Result<bool, DbErr> {
    capability::queries::has_platform_admin(txn, actor, true).await
}

async fn replace_roles(
    txn: &impl ConnectionTrait,
    scope: AdministrationScope,
    membership_id: Uuid,
    roles: &[String],
) -> Result<(), DbErr> {
    let table = rows::role_table(rows::scope_name(scope));
    let backend = txn.get_database_backend();
    let delete_statement = Statement::from_sql_and_values(
        backend,
        format!("DELETE FROM {table} WHERE membership_id = $1"),
        [membership_id.into()],
    );
    txn.execute_raw(delete_statement).await?;
    let insert_sql = format!("INSERT INTO {table} (membership_id, role_code) VALUES ($1, $2)");
    for role in roles {
        let insert_statement = Statement::from_sql_and_values(
            backend,
            &insert_sql,
            [membership_id.into(), role.clone().into()],
        );
        txn.execute_raw(insert_statement).await?;
    }
    Ok(())
}

async fn principal_exists(txn: &impl ConnectionTrait, id: Uuid) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT 1 FROM principals WHERE id = $1",
        [id.into()],
    );
    Ok(txn.query_one_raw(statement).await?.is_some())
}

async fn project_organization_membership_exists(
    txn: &impl ConnectionTrait,
    project_id: Uuid,
    member: Uuid,
) -> Result<bool, DbErr> {
    let statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT 1 FROM projects project JOIN organization_memberships membership \
           ON membership.organization_id = project.organization_id \
         WHERE project.id = $1 AND membership.principal_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL FOR KEY SHARE",
        [project_id.into(), member.into()],
    );
    Ok(txn.query_one_raw(statement).await?.is_some())
}

async fn budget_revision_locked(
    txn: &impl ConnectionTrait,
    project_id: Uuid,
) -> Result<i64, DbErr> {
    let statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT current_revision FROM project_budget_policies WHERE project_id = $1 FOR UPDATE",
        [project_id.into()],
    );
    match txn.query_one_raw(statement).await? {
        Some(row) => row.try_get_by("current_revision"),
        None => Ok(-1),
    }
}

// --- audit, and the deployment/approval-domain scope caches administration writes maintain ---

#[allow(clippy::too_many_arguments)]
async fn audit(
    txn: &impl ConnectionTrait,
    actor: Uuid,
    scope: &str,
    scope_id: Uuid,
    action: &str,
    reason: Option<&str>,
    before_digest: Option<&str>,
    after_digest: Option<&str>,
    material: serde_json::Value,
) -> Result<(), DbErr> {
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

    let mut values: Vec<sea_orm::Value> = vec![
        Uuid::new_v4().into(),
        actor.into(),
        scope.into(),
        scope_id.into(),
        action.into(),
        reason.into(),
        before_digest.into(),
        after_digest.into(),
        facts_json.into(),
    ];
    values.extend(crate::audit::context::audit_metadata_values());
    let statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO administration_audit_events \
         (id, actor_principal_id, scope_type, scope_id, action, reason, before_digest, after_digest, facts, \
          request_id, correlation_id, graphql_operation, source_ip, user_agent) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10, $11, $12, $13, $14)",
        values,
    );
    txn.execute_raw(statement).await?;
    Ok(())
}

async fn mark_approval_role_assignment(
    txn: &impl ConnectionTrait,
    actor: Uuid,
    scope: AdministrationScope,
    previous: &[String],
    next: &[String],
) -> Result<(), DbErr> {
    let deployment_approver = "DEPLOYMENT_APPROVER".to_string();
    if scope != AdministrationScope::Project
        || previous.contains(&deployment_approver) == next.contains(&deployment_approver)
    {
        return Ok(());
    }
    let statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT set_config('hive.m14_approval_role_assignment_actor', $1, TRUE)",
        [actor.to_string().into()],
    );
    txn.query_one_raw(statement).await?;
    Ok(())
}

async fn approval_role_transition_allowed(
    txn: &impl ConnectionTrait,
    actor: Uuid,
    scope: AdministrationScope,
    previous: &[String],
    next: &[String],
) -> Result<bool, DbErr> {
    let deployment_approver = "DEPLOYMENT_APPROVER".to_string();
    if scope != AdministrationScope::Project
        || previous.contains(&deployment_approver) == next.contains(&deployment_approver)
    {
        return Ok(true);
    }
    tx_is_platform_administrator(txn, actor).await
}

async fn refresh_membership_scope(
    txn: &impl ConnectionTrait,
    scope: AdministrationScope,
    principal: Uuid,
    scope_id: Uuid,
) -> Result<(), DbErr> {
    match scope {
        AdministrationScope::Organization => {
            refresh_organization_membership_scope(txn, principal, scope_id).await?;
            refresh_organization_scope(txn, principal, scope_id).await?;
            refresh_organization_project_scopes(txn, principal, scope_id).await?;
        }
        AdministrationScope::Project => refresh_project_scope(txn, principal, scope_id).await?,
    }
    Ok(())
}

async fn refresh_role_scope(
    txn: &impl ConnectionTrait,
    scope: AdministrationScope,
    principal: Uuid,
    scope_id: Uuid,
) -> Result<(), DbErr> {
    match scope {
        AdministrationScope::Organization => {
            refresh_organization_scope(txn, principal, scope_id).await
        }
        AdministrationScope::Project => refresh_project_scope(txn, principal, scope_id).await,
    }
}

async fn refresh_organization_membership_scope(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    organization: Uuid,
) -> Result<(), DbErr> {
    let backend = txn.get_database_backend();
    let delete_statement = Statement::from_sql_and_values(
        backend,
        "DELETE FROM deployment_approval_principal_organization_membership_scopes WHERE principal_id = $1 AND organization_id = $2",
        [principal.into(), organization.into()],
    );
    txn.execute_raw(delete_statement).await?;
    let insert_statement = Statement::from_sql_and_values(
        backend,
        "INSERT INTO deployment_approval_principal_organization_membership_scopes (principal_id, organization_id, valid_after) \
         SELECT $1, $2, membership.started_at FROM organization_memberships membership \
         WHERE membership.principal_id = $1 AND membership.organization_id = $2 \
           AND membership.ended_at IS NULL AND membership.started_at <= CURRENT_TIMESTAMP",
        [principal.into(), organization.into()],
    );
    txn.execute_raw(insert_statement).await?;
    Ok(())
}

async fn refresh_organization_scope(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    organization: Uuid,
) -> Result<(), DbErr> {
    let backend = txn.get_database_backend();
    let delete_statement = Statement::from_sql_and_values(
        backend,
        "DELETE FROM deployment_approval_principal_organization_scopes WHERE principal_id = $1 AND organization_id = $2",
        [principal.into(), organization.into()],
    );
    txn.execute_raw(delete_statement).await?;
    let insert_statement = Statement::from_sql_and_values(
        backend,
        "INSERT INTO deployment_approval_principal_organization_scopes (principal_id, organization_id, valid_after) \
         SELECT $1, $2, MIN(membership.started_at) FROM organization_memberships membership \
           JOIN organization_membership_roles role ON role.membership_id = membership.id \
         WHERE membership.principal_id = $1 AND membership.organization_id = $2 AND membership.ended_at IS NULL \
           AND role.role_code IN ('ORGANIZATION_ADMIN', 'AUDITOR') \
         GROUP BY membership.principal_id",
        [principal.into(), organization.into()],
    );
    txn.execute_raw(insert_statement).await?;
    Ok(())
}

async fn refresh_project_scope(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    project: Uuid,
) -> Result<(), DbErr> {
    let backend = txn.get_database_backend();
    let delete_statement = Statement::from_sql_and_values(
        backend,
        "DELETE FROM deployment_approval_principal_project_scopes WHERE principal_id = $1 AND project_id = $2",
        [principal.into(), project.into()],
    );
    txn.execute_raw(delete_statement).await?;
    let insert_statement = Statement::from_sql_and_values(
        backend,
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
        [principal.into(), project.into()],
    );
    txn.execute_raw(insert_statement).await?;
    Ok(())
}

async fn refresh_organization_project_scopes(
    txn: &impl ConnectionTrait,
    principal: Uuid,
    organization: Uuid,
) -> Result<(), DbErr> {
    let backend = txn.get_database_backend();
    let delete_statement = Statement::from_sql_and_values(
        backend,
        "DELETE FROM deployment_approval_principal_project_scopes scope USING projects project \
         WHERE scope.principal_id = $1 AND project.id = scope.project_id AND project.organization_id = $2",
        [principal.into(), organization.into()],
    );
    txn.execute_raw(delete_statement).await?;
    let insert_statement = Statement::from_sql_and_values(
        backend,
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
        [principal.into(), organization.into()],
    );
    txn.execute_raw(insert_statement).await?;
    Ok(())
}

async fn invalidate_pending_approvals_for_archived_project(
    txn: &impl ConnectionTrait,
    project_id: Uuid,
    actor: Uuid,
) -> Result<(), DbErr> {
    let backend = txn.get_database_backend();
    let select_statement = Statement::from_sql_and_values(
        backend,
        "SELECT requirement.id AS requirement_id, deployment.id AS deployment_id \
         FROM deployments deployment JOIN deployment_approval_requirements requirement ON requirement.deployment_id = deployment.id \
         WHERE deployment.project_id = $1 AND deployment.lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED') AND requirement.status = 'PENDING' \
         ORDER BY deployment.id ASC FOR UPDATE OF deployment, requirement",
        [project_id.into()],
    );
    let candidate_rows = txn.query_all_raw(select_statement).await?;
    let candidates: Vec<(Uuid, Uuid)> = candidate_rows
        .iter()
        .map(|row| {
            Ok((
                row.try_get_by("requirement_id")?,
                row.try_get_by("deployment_id")?,
            ))
        })
        .collect::<Result<_, DbErr>>()?;

    for (requirement_id, deployment_id) in candidates {
        let invalidate_statement = Statement::from_sql_and_values(
            backend,
            "UPDATE deployment_approval_requirements SET status = 'INVALIDATED', revision = revision + 1, \
               invalidated_at = CURRENT_TIMESTAMP, invalidation_code = 'PROJECT_ARCHIVED' WHERE id = $1 AND status = 'PENDING'",
            [requirement_id.into()],
        );
        let invalidated = txn.execute_raw(invalidate_statement).await?;
        if invalidated.rows_affected() == 0 {
            continue;
        }

        let sequence_statement = Statement::from_sql_and_values(
            backend,
            "INSERT INTO deployment_timeline_counters (deployment_id, attempt_number, next_sequence) VALUES ($1, 0, 2) \
             ON CONFLICT (deployment_id, attempt_number) DO UPDATE SET next_sequence = deployment_timeline_counters.next_sequence + 1 \
             RETURNING next_sequence - 1 AS next_sequence",
            [deployment_id.into()],
        );
        let sequence: i64 = txn
            .query_one_raw(sequence_statement)
            .await?
            .expect("the ON CONFLICT DO UPDATE always returns exactly one row")
            .try_get_by("next_sequence")?;

        let mut audit_values: Vec<sea_orm::Value> = vec![
            Uuid::new_v4().into(),
            deployment_id.into(),
            actor.into(),
            requirement_id.to_string().into(),
            sequence.into(),
        ];
        audit_values.extend(crate::audit::context::audit_metadata_values());
        let audit_statement = Statement::from_sql_and_values(
            backend,
            "INSERT INTO deployment_audit_events \
               (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence, \
                request_id, correlation_id, graphql_operation, source_ip, user_agent) \
             VALUES ($1, $2, $3, 'APPROVAL_INVALIDATED', jsonb_build_object('requirementId', $4, 'code', 'PROJECT_ARCHIVED'), NULL, 0, $5, $6, $7, $8, $9, $10)",
            audit_values,
        );
        txn.execute_raw(audit_statement).await?;

        let health_statement = Statement::from_sql_and_values(
            backend,
            "UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Project archive terminalized this pending approval cycle.', \
               observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1",
            [deployment_id.into()],
        );
        txn.execute_raw(health_statement).await?;

        let deployment_status_statement = Statement::from_sql_and_values(
            backend,
            "UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, projection_revision = projection_revision + 1, \
               updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')",
            [deployment_id.into()],
        );
        txn.execute_raw(deployment_status_statement).await?;

        let projection_statement = Statement::from_sql_and_values(
            backend,
            "UPDATE deployments SET projection_revision = projection_revision + 1 WHERE id = $1",
            [deployment_id.into()],
        );
        txn.execute_raw(projection_statement).await?;
    }
    Ok(())
}

async fn record_project_archive_event(
    txn: &impl ConnectionTrait,
    project_id: Uuid,
    actor: Uuid,
    archived_project_revision: i64,
) -> Result<(), DbErr> {
    let statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO deployment_approval_project_archive_events (id, project_id, actor_principal_id, archived_project_revision) VALUES ($1, $2, $3, $4)",
        [
            Uuid::new_v4().into(),
            project_id.into(),
            actor.into(),
            archived_project_revision.into(),
        ],
    );
    txn.execute_raw(statement).await?;
    Ok(())
}

async fn insert_policy_version(
    txn: &impl ConnectionTrait,
    policy_id: Uuid,
    revision: i64,
    matrix: &BTreeMap<String, ApprovalRule>,
    reason: &str,
) -> Result<(), DbErr> {
    let canonical = rows::matrix_json(matrix);
    let canonical_digest = rows::digest(&canonical);
    let statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO project_approval_policy_versions (policy_id, revision, digest, matrix, change_reason) VALUES ($1, $2, $3, $4::jsonb, $5)",
        [
            policy_id.into(),
            revision.into(),
            canonical_digest.into(),
            canonical.into(),
            reason.into(),
        ],
    );
    txn.execute_raw(statement).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn create_project(
    db: &DatabaseConnection,
    actor: Uuid,
    organization_id: Uuid,
    expected_revision: i64,
    slug: String,
    display_name: String,
    description: Option<String>,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(other)?;

    if !tx_has_administration_capability(
        &txn,
        actor,
        "PROJECT.CREATE",
        AdministrationScope::Organization,
        organization_id,
    )
    .await
    .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(organization_row) =
        rows::locked_scope_row(&txn, AdministrationScope::Organization, organization_id)
            .await
            .map_err(other)?
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
    let insert_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO projects (id, organization_id, slug, display_name, description, lifecycle_status, revision) \
         VALUES ($1, $2, $3, $4, $5, 'ACTIVE', 1)",
        [
            project_id.into(),
            organization_id.into(),
            slug.clone().into(),
            display_name.trim().into(),
            description.trim().into(),
        ],
    );
    if let Err(error) = txn.execute_raw(insert_statement).await {
        if rows::is_unique_violation(&error) {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        return Err(other(error));
    }
    let budget_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO project_budget_policies (project_id) VALUES ($1)",
        [project_id.into()],
    );
    txn.execute_raw(budget_statement).await.map_err(other)?;
    let approval_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO project_approval_policies (id, project_id, current_revision) VALUES ($1, $2, 1)",
        [project_id.into(), project_id.into()],
    );
    txn.execute_raw(approval_statement).await.map_err(other)?;
    let matrix = rows::default_matrix();
    insert_policy_version(
        &txn,
        project_id,
        1,
        &matrix,
        "Initial fixed local P-05 policy",
    )
    .await
    .map_err(other)?;
    audit(
        &txn,
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
    .map_err(other)?;
    audit(
        &txn,
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
    .map_err(other)?;

    txn.commit().await.map_err(other)?;
    let value = queries::project(db, actor, project_id).await?;
    Ok(AdministrationMutationResult::project(
        value.expect("project visible to its own creator"),
    ))
}

#[allow(clippy::too_many_arguments)]
pub async fn add_membership(
    db: &DatabaseConnection,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
    member: Uuid,
    role_codes: Vec<String>,
    expected_scope_revision: i64,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(other)?;
    let capability_code = format!("{}_MEMBERSHIP.ADD", rows::scope_name(scope));
    if !tx_has_administration_capability(&txn, actor, &capability_code, scope, scope_id)
        .await
        .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(owner) = rows::locked_scope_row(&txn, scope, scope_id)
        .await
        .map_err(other)?
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
    if !principal_exists(&txn, member).await.map_err(other)? {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    }
    if scope == AdministrationScope::Project
        && !project_organization_membership_exists(&txn, scope_id, member)
            .await
            .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::invalid(),
        ));
    }
    if !approval_role_transition_allowed(&txn, actor, scope, &[], &role_codes)
        .await
        .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }

    let membership_id = Uuid::new_v4();
    let table = rows::membership_table(rows::scope_name(scope));
    let column = rows::scope_id_column(scope);
    let insert_sql = format!("INSERT INTO {table} (id, {column}, principal_id, started_at, ended_at, revision, active_marker) VALUES ($1, $2, $3, CURRENT_TIMESTAMP, NULL, 1, TRUE)");
    let insert_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        &insert_sql,
        [membership_id.into(), scope_id.into(), member.into()],
    );
    if let Err(error) = txn.execute_raw(insert_statement).await {
        if rows::is_unique_violation(&error) {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        return Err(other(error));
    }
    mark_approval_role_assignment(&txn, actor, scope, &[], &role_codes)
        .await
        .map_err(other)?;
    replace_roles(&txn, scope, membership_id, &role_codes)
        .await
        .map_err(other)?;
    refresh_membership_scope(&txn, scope, member, scope_id)
        .await
        .map_err(other)?;
    audit(
        &txn,
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
    .map_err(other)?;

    txn.commit().await.map_err(other)?;
    queries::result_for(db, actor, scope, scope_id).await
}

#[allow(clippy::too_many_arguments)]
pub async fn replace_membership(
    db: &DatabaseConnection,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
    membership_id: Uuid,
    role_codes: Vec<String>,
    expected_revision: i64,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(other)?;
    let capability_code = format!("{}_MEMBERSHIP.CHANGE_ROLES", rows::scope_name(scope));
    if !tx_has_administration_capability(&txn, actor, &capability_code, scope, scope_id)
        .await
        .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(membership) = rows::locked_membership(&txn, scope, scope_id, membership_id)
        .await
        .map_err(other)?
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
        && !tx_active_project(&txn, scope_id).await.map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }

    let previous = rows::current_roles_tx(&txn, scope, membership_id)
        .await
        .map_err(other)?;
    if previous == role_codes {
        txn.commit().await.map_err(other)?;
        return queries::result_for(db, actor, scope, scope_id).await;
    }
    if !approval_role_transition_allowed(&txn, actor, scope, &previous, &role_codes)
        .await
        .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    mark_approval_role_assignment(&txn, actor, scope, &previous, &role_codes)
        .await
        .map_err(other)?;
    replace_roles(&txn, scope, membership_id, &role_codes)
        .await
        .map_err(other)?;
    refresh_role_scope(&txn, scope, membership.principal_id, scope_id)
        .await
        .map_err(other)?;
    let table = rows::membership_table(rows::scope_name(scope));
    let update_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        format!("UPDATE {table} SET revision = revision + 1 WHERE id = $1"),
        [membership_id.into()],
    );
    txn.execute_raw(update_statement).await.map_err(other)?;
    audit(
        &txn,
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
    .map_err(other)?;

    txn.commit().await.map_err(other)?;
    queries::result_for(db, actor, scope, scope_id).await
}

#[allow(clippy::too_many_arguments)]
pub async fn end_membership(
    db: &DatabaseConnection,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
    membership_id: Uuid,
    expected_revision: i64,
    reason: String,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(other)?;
    let capability_code = format!("{}_MEMBERSHIP.END", rows::scope_name(scope));
    if !tx_has_administration_capability(&txn, actor, &capability_code, scope, scope_id)
        .await
        .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(membership) = rows::locked_membership(&txn, scope, scope_id, membership_id)
        .await
        .map_err(other)?
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
    let previous = rows::current_roles_tx(&txn, scope, membership_id)
        .await
        .map_err(other)?;
    if !approval_role_transition_allowed(&txn, actor, scope, &previous, &[])
        .await
        .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    mark_approval_role_assignment(&txn, actor, scope, &previous, &[])
        .await
        .map_err(other)?;
    let table = rows::membership_table(rows::scope_name(scope));
    let update_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        format!("UPDATE {table} SET ended_at = CURRENT_TIMESTAMP, revision = revision + 1, active_marker = NULL WHERE id = $1"),
        [membership_id.into()],
    );
    txn.execute_raw(update_statement).await.map_err(other)?;
    refresh_membership_scope(&txn, scope, membership.principal_id, scope_id)
        .await
        .map_err(other)?;
    let before = format!(
        "revision={} ended={} principalId={}",
        membership.revision, membership.ended, membership.principal_id
    );
    audit(
        &txn,
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
    .map_err(other)?;

    txn.commit().await.map_err(other)?;
    queries::result_for(db, actor, scope, scope_id).await
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
) -> Result<AdministrationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(other)?;
    let action = format!(
        "{}.{}",
        rows::scope_name(scope),
        if archive { "ARCHIVE" } else { "RESTORE" }
    );
    if !tx_has_administration_capability(&txn, actor, &action, scope, scope_id)
        .await
        .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(current) = rows::locked_scope_row(&txn, scope, scope_id)
        .await
        .map_err(other)?
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
    let update_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        format!("UPDATE {table} SET lifecycle_status = $1, revision = revision + 1 WHERE id = $2"),
        [next.into(), scope_id.into()],
    );
    txn.execute_raw(update_statement).await.map_err(other)?;
    if archive && scope == AdministrationScope::Project {
        invalidate_pending_approvals_for_archived_project(&txn, scope_id, actor)
            .await
            .map_err(other)?;
        record_project_archive_event(&txn, scope_id, actor, current.revision + 1)
            .await
            .map_err(other)?;
    }
    let before = format!(
        "slug={} status={} revision={}",
        current.slug, current.status, current.revision
    );
    let after = format!("{next}{}", current.revision);
    audit(
        &txn,
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
    .map_err(other)?;

    txn.commit().await.map_err(other)?;
    queries::result_for(db, actor, scope, scope_id).await
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
) -> Result<AdministrationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(other)?;
    if !tx_has_administration_capability(
        &txn,
        actor,
        "PROJECT_BUDGET.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    if !tx_active_project(&txn, project_id).await.map_err(other)? {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }
    let current_revision = budget_revision_locked(&txn, project_id)
        .await
        .map_err(other)?;
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
    let prior = rows::current_budget_tx(&txn, project_id)
        .await
        .map_err(other)?;
    let unchanged = prior.as_ref().is_some_and(|prior| {
        prior.currency == currency
            && prior.monthly_limit_cents == monthly_limit_cents
            && prior.warning_threshold_cents == warning_threshold_cents
    });
    if unchanged {
        txn.commit().await.map_err(other)?;
        let value = queries::project(db, actor, project_id).await?;
        return Ok(AdministrationMutationResult::project(
            value.expect("project visible inside its own budget update"),
        ));
    }
    let next_revision = current_revision + 1;
    let insert_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO project_budget_policy_versions (project_id, revision, currency, monthly_limit_cents, warning_threshold_cents, change_reason) \
         VALUES ($1, $2, $3, $4, $5, $6)",
        [
            project_id.into(),
            next_revision.into(),
            currency.clone().into(),
            monthly_limit_cents.into(),
            warning_threshold_cents.into(),
            reason.clone().into(),
        ],
    );
    txn.execute_raw(insert_statement).await.map_err(other)?;
    let update_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE project_budget_policies SET current_revision = $1 WHERE project_id = $2",
        [next_revision.into(), project_id.into()],
    );
    txn.execute_raw(update_statement).await.map_err(other)?;
    let before_digest = prior.as_ref().map(|prior| {
        rows::digest(&format!(
            "{}|{}|{}",
            prior.currency, prior.monthly_limit_cents, prior.warning_threshold_cents
        ))
    });
    audit(
        &txn,
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
    .map_err(other)?;

    txn.commit().await.map_err(other)?;
    let value = queries::project(db, actor, project_id).await?;
    Ok(AdministrationMutationResult::project(
        value.expect("project visible inside its own budget update"),
    ))
}

pub async fn update_approval_policy(
    db: &DatabaseConnection,
    actor: Uuid,
    project_id: Uuid,
    expected_revision: i64,
    matrix: BTreeMap<String, ApprovalRule>,
    reason: String,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(other)?;
    if !tx_has_administration_capability(
        &txn,
        actor,
        "PROJECT_APPROVAL_POLICY.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    if !tx_active_project(&txn, project_id).await.map_err(other)? {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::protected_lifecycle(),
        ));
    }
    let Some(prior) = rows::current_approval_tx(&txn, project_id)
        .await
        .map_err(other)?
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
        txn.commit().await.map_err(other)?;
        let value = queries::project(db, actor, project_id).await?;
        return Ok(AdministrationMutationResult::project(
            value.expect("project visible inside its own approval policy update"),
        ));
    }

    let next_revision = expected_revision + 1;
    insert_policy_version(&txn, prior.id, next_revision, &matrix, &reason)
        .await
        .map_err(other)?;
    let update_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE project_approval_policies SET current_revision = $1 WHERE id = $2",
        [next_revision.into(), prior.id.into()],
    );
    txn.execute_raw(update_statement).await.map_err(other)?;
    audit(
        &txn,
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
    .map_err(other)?;

    txn.commit().await.map_err(other)?;
    let value = queries::project(db, actor, project_id).await?;
    Ok(AdministrationMutationResult::project(value.expect(
        "project visible inside its own approval policy update",
    )))
}

pub async fn update_project_general(
    db: &DatabaseConnection,
    actor: Uuid,
    project_id: Uuid,
    expected_revision: i64,
    display_name: String,
    description: String,
) -> Result<AdministrationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(other)?;
    if !tx_has_administration_capability(
        &txn,
        actor,
        "PROJECT.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(current) = rows::locked_scope_row(&txn, AdministrationScope::Project, project_id)
        .await
        .map_err(other)?
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
    let update_statement = Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE projects SET display_name = $1, description = $2, revision = revision + 1 WHERE id = $3",
        [
            display_name.clone().into(),
            description.clone().into(),
            project_id.into(),
        ],
    );
    txn.execute_raw(update_statement).await.map_err(other)?;
    let before = format!(
        "slug={} status={} revision={}",
        current.slug, current.status, current.revision
    );
    audit(
        &txn,
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
    .map_err(other)?;

    txn.commit().await.map_err(other)?;
    let value = queries::project(db, actor, project_id).await?;
    Ok(AdministrationMutationResult::project(
        value.expect("project visible inside its own general update"),
    ))
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
) -> Result<AdministrationMutationResult, RepositoryError> {
    let txn = db.begin().await.map_err(other)?;
    if !tx_has_administration_capability(
        &txn,
        actor,
        "PROJECT.UPDATE",
        AdministrationScope::Project,
        project_id,
    )
    .await
    .map_err(other)?
    {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::forbidden(),
        ));
    }
    let Some(project_row) = rows::locked_scope_row(&txn, AdministrationScope::Project, project_id)
        .await
        .map_err(other)?
    else {
        return Ok(AdministrationMutationResult::refused(
            AdministrationProblem::unavailable(),
        ));
    };
    let prior = match connection_id {
        None => None,
        Some(id) => {
            let Some(prior) = rows::locked_settings_connection(&txn, project_id, id)
                .await
                .map_err(other)?
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
            let insert_statement = Statement::from_sql_and_values(
                txn.get_database_backend(),
                "INSERT INTO project_settings_connections \
                 (id, project_id, display_name, definition_version, environment, credential_status, lifecycle_status, revision) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 1)",
                [
                    new_connection_id.into(),
                    project_id.into(),
                    display_name.clone().into(),
                    definition_version.clone().into(),
                    environment.clone().into(),
                    credential_status.clone().into(),
                    lifecycle_status.clone().into(),
                ],
            );
            txn.execute_raw(insert_statement).await.map_err(other)?;
            audit(
                &txn,
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
            .map_err(other)?;
            txn.commit().await.map_err(other)?;
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
                txn.commit().await.map_err(other)?;
                let value = queries::project(db, actor, project_id).await?;
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
            let update_statement = Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE project_settings_connections SET display_name = $1, definition_version = $2, environment = $3, \
                   credential_status = $4, lifecycle_status = $5, revision = revision + 1 WHERE id = $6",
                [
                    display_name.clone().into(),
                    definition_version.clone().into(),
                    environment.clone().into(),
                    credential_status.clone().into(),
                    lifecycle_status.clone().into(),
                    connection_id.into(),
                ],
            );
            txn.execute_raw(update_statement).await.map_err(other)?;
            let before = format!(
                "{}|{}|{}|{}|{}",
                prior.display_name,
                prior.definition_version,
                prior.environment,
                prior.credential_status,
                prior.lifecycle_status
            );
            audit(
                &txn,
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
            .map_err(other)?;
            txn.commit().await.map_err(other)?;
        }
    }

    let value = queries::project(db, actor, project_id).await?;
    Ok(AdministrationMutationResult::project(
        value.expect("project visible inside its own connection save"),
    ))
}
