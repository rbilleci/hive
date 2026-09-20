//! Ports the read-only `AdministrationRepository` methods: `findOrganization`/`findProject` and
//! every assembly helper they call.

use super::rows;
use chrono::{DateTime, Utc};
use hive_application::administration::{
    AdministrationMembership, AdministrationMutationResult, AdministrationPrincipal,
    AdministrationRepositoryError as RepositoryError, AdministrationScope, ApprovalPolicy,
    ApprovalPolicyVersion, BudgetPolicy, BudgetStatus, OrganizationAdministration,
    ProjectAdministration, ProjectSettingsConnection,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub async fn find_organization(
    pool: &PgPool,
    principal: Uuid,
    organization_id: Uuid,
) -> Result<Option<OrganizationAdministration>, RepositoryError> {
    let visible = crate::capability::has_capability(
        pool,
        principal,
        crate::capability::ORGANIZATION_VIEW,
        crate::capability::Scope::Organization(organization_id),
        false,
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    if !visible {
        return Ok(None);
    }
    organization(pool, principal, organization_id).await
}

pub async fn find_project(
    pool: &PgPool,
    principal: Uuid,
    project_id: Uuid,
) -> Result<Option<ProjectAdministration>, RepositoryError> {
    let visible = crate::capability::has_capability(
        pool,
        principal,
        crate::capability::PROJECT_VIEW,
        crate::capability::Scope::Project(project_id),
        false,
    )
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    if !visible {
        return Ok(None);
    }
    project(pool, principal, project_id).await
}

async fn roles(
    pool: &PgPool,
    scope: &str,
    membership_id: Uuid,
) -> Result<Vec<String>, RepositoryError> {
    let sql = format!(
        "SELECT role_code FROM {} WHERE membership_id = $1 ORDER BY role_code",
        rows::role_table(scope)
    );
    let db_rows = sqlx::query(&sql)
        .bind(membership_id)
        .fetch_all(pool)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    Ok(db_rows.into_iter().map(|row| row.get(0)).collect())
}

async fn project_access(
    pool: &PgPool,
    principal_id: Uuid,
    organization_id: Uuid,
    organization_roles: &[String],
) -> Result<Vec<String>, RepositoryError> {
    let db_rows = sqlx::query(
        "SELECT membership.id, project.slug, project.display_name FROM project_memberships membership \
         JOIN projects project ON project.id = membership.project_id \
         WHERE membership.principal_id = $1 AND project.organization_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
         ORDER BY project.display_name, project.id",
    )
    .bind(principal_id)
    .bind(organization_id)
    .fetch_all(pool)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;

    let mut summaries = Vec::new();
    if organization_roles
        .iter()
        .any(|role| role == "ORGANIZATION_ADMIN")
    {
        summaries.push("All organization projects (ORGANIZATION_ADMIN)".to_string());
    }
    for row in db_rows {
        let membership_id: Uuid = row.get(0);
        let display_name: String = row.get(2);
        let project_roles = roles(pool, "PROJECT", membership_id).await?;
        summaries.push(format!("{display_name} ({})", project_roles.join(", ")));
    }
    Ok(summaries)
}

async fn memberships(
    pool: &PgPool,
    scope: &str,
    scope_id: Uuid,
) -> Result<Vec<AdministrationMembership>, RepositoryError> {
    let table = rows::membership_table(scope);
    let column = if scope == "ORGANIZATION" {
        "organization_id"
    } else {
        "project_id"
    };
    let sql = format!(
        "SELECT membership.id, membership.principal_id, principal.display_name, principal.email, membership.started_at, \
                membership.ended_at, principal.last_seen_at, membership.revision \
         FROM {table} membership JOIN principals principal ON principal.id = membership.principal_id \
         WHERE membership.{column} = $1 \
         ORDER BY membership.ended_at NULLS FIRST, membership.started_at DESC"
    );
    let db_rows = sqlx::query(&sql)
        .bind(scope_id)
        .fetch_all(pool)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;

    let mut result = Vec::with_capacity(db_rows.len());
    for row in db_rows {
        let membership_id: Uuid = row.get(0);
        let principal_id: Uuid = row.get(1);
        let membership_roles = roles(pool, scope, membership_id).await?;
        let project_access_summary = if scope == "ORGANIZATION" {
            project_access(pool, principal_id, scope_id, &membership_roles).await?
        } else {
            Vec::new()
        };
        result.push(AdministrationMembership {
            id: membership_id,
            principal_id,
            display_name: row.get(2),
            email: row.get(3),
            role_codes: membership_roles,
            project_access_summary,
            started_at: row.get(4),
            ended_at: row.get(5),
            last_seen_at: row.get(6),
            revision: row.get(7),
        });
    }
    Ok(result)
}

async fn organization_principals(
    pool: &PgPool,
    organization_id: Uuid,
) -> Result<Vec<AdministrationPrincipal>, RepositoryError> {
    let db_rows = sqlx::query(
        "SELECT DISTINCT principal.id, principal.display_name, principal.email \
         FROM principals principal JOIN organization_memberships membership ON membership.principal_id = principal.id \
         WHERE membership.organization_id = $1 ORDER BY principal.display_name, principal.id",
    )
    .bind(organization_id)
    .fetch_all(pool)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    Ok(db_rows
        .into_iter()
        .map(|row| AdministrationPrincipal {
            id: row.get(0),
            display_name: row.get(1),
            email: row.get(2),
        })
        .collect())
}

async fn assignable_project_roles(
    pool: &PgPool,
    actor: Uuid,
) -> Result<Vec<String>, RepositoryError> {
    let ordinary = ["AGENT_DEVELOPER", "AUDITOR", "OPERATOR", "PROJECT_ADMIN"];
    let is_platform_administrator = crate::capability::is_platform_administrator(pool, actor)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    Ok(if is_platform_administrator {
        vec![
            "AGENT_DEVELOPER".to_string(),
            "AUDITOR".to_string(),
            "DEPLOYMENT_APPROVER".to_string(),
            "OPERATOR".to_string(),
            "PROJECT_ADMIN".to_string(),
        ]
    } else {
        ordinary.iter().map(|role| role.to_string()).collect()
    })
}

async fn scope_capabilities(
    pool: &PgPool,
    actor: Uuid,
    scope: crate::capability::Scope,
) -> Result<Vec<String>, RepositoryError> {
    let mut available = Vec::new();
    for &code in crate::capability::ADMINISTRATION_CAPABILITIES {
        if crate::capability::has_capability(pool, actor, code, scope, false)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?
        {
            available.push(code.to_string());
        }
    }
    Ok(available)
}

async fn budget(pool: &PgPool, project_id: Uuid) -> Result<Option<BudgetPolicy>, RepositoryError> {
    let row = sqlx::query(
        "SELECT version.revision, version.currency, version.monthly_limit_cents, version.warning_threshold_cents, \
                version.change_reason, version.created_at \
         FROM project_budget_policies policy \
         JOIN project_budget_policy_versions version ON version.project_id = policy.project_id AND version.revision = policy.current_revision \
         WHERE policy.project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    Ok(row.map(|row| BudgetPolicy {
        revision: row.get(0),
        currency: row.get(1),
        monthly_limit_cents: row.get(2),
        warning_threshold_cents: row.get(3),
        change_reason: row.get(4),
        created_at: row.get(5),
    }))
}

async fn budget_history(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<BudgetPolicy>, RepositoryError> {
    let db_rows = sqlx::query(
        "SELECT revision, currency, monthly_limit_cents, warning_threshold_cents, change_reason, created_at \
         FROM project_budget_policy_versions WHERE project_id = $1 ORDER BY revision DESC",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    Ok(db_rows
        .into_iter()
        .map(|row| BudgetPolicy {
            revision: row.get(0),
            currency: row.get(1),
            monthly_limit_cents: row.get(2),
            warning_threshold_cents: row.get(3),
            change_reason: row.get(4),
            created_at: row.get(5),
        })
        .collect())
}

async fn budget_status(
    pool: &PgPool,
    project_id: Uuid,
    budget: Option<&BudgetPolicy>,
) -> Result<BudgetStatus, RepositoryError> {
    let Some(budget) = budget else {
        return Ok(BudgetStatus {
            state: "NOT_CONFIGURED".to_string(),
            reason: Some("NO_POLICY".to_string()),
            amount_cents: None,
            includes_estimates: false,
            currency: None,
            period_start: None,
            period_end: None,
            data_as_of: None,
            last_successful_import_at: None,
        });
    };

    let row = sqlx::query(
        "SELECT period_start, period_end, currency, state, amount_cents, includes_estimates, data_as_of, completed_at \
         FROM frozen_spend_import_batches WHERE project_id = $1 \
           AND period_start <= date_trunc('month', CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' \
           AND period_end > date_trunc('month', CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' \
         ORDER BY imported_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;

    let Some(row) = row else {
        return Ok(BudgetStatus {
            state: "UNKNOWN".to_string(),
            reason: Some("NO_DATA".to_string()),
            amount_cents: None,
            includes_estimates: false,
            currency: Some(budget.currency.clone()),
            period_start: None,
            period_end: None,
            data_as_of: None,
            last_successful_import_at: None,
        });
    };

    let period_start: Option<DateTime<Utc>> = row.get(0);
    let period_end: Option<DateTime<Utc>> = row.get(1);
    let currency: String = row.get(2);
    let import_state: String = row.get(3);
    let amount: Option<i32> = row.get(4);
    let includes_estimates: bool = row.get(5);
    let data_as_of: Option<DateTime<Utc>> = row.get(6);
    let completed_at: Option<DateTime<Utc>> = row.get(7);

    if budget.currency != currency {
        return Ok(BudgetStatus {
            state: "UNKNOWN".to_string(),
            reason: Some("CURRENCY_MISMATCH".to_string()),
            amount_cents: None,
            includes_estimates,
            currency: Some(budget.currency.clone()),
            period_start,
            period_end,
            data_as_of,
            last_successful_import_at: completed_at,
        });
    }
    if import_state != "COMPLETE" {
        return Ok(BudgetStatus {
            state: "UNKNOWN".to_string(),
            reason: Some(if import_state == "FAILED" {
                "IMPORT_FAILED".to_string()
            } else {
                "INCOMPLETE_DATA".to_string()
            }),
            amount_cents: None,
            includes_estimates,
            currency: Some(currency),
            period_start,
            period_end,
            data_as_of,
            last_successful_import_at: completed_at,
        });
    }
    if completed_at.is_none() || completed_at.unwrap() < Utc::now() - chrono::Duration::hours(24) {
        return Ok(BudgetStatus {
            state: "UNKNOWN".to_string(),
            reason: Some("STALE_DATA".to_string()),
            amount_cents: None,
            includes_estimates,
            currency: Some(currency),
            period_start,
            period_end,
            data_as_of,
            last_successful_import_at: completed_at,
        });
    }

    let amount = amount.unwrap_or(0);
    let state = if amount >= budget.monthly_limit_cents {
        "EXCEEDED"
    } else if amount >= budget.warning_threshold_cents {
        "WARNING"
    } else {
        "NORMAL"
    };
    Ok(BudgetStatus {
        state: state.to_string(),
        reason: None,
        amount_cents: Some(amount),
        includes_estimates,
        currency: Some(currency),
        period_start,
        period_end,
        data_as_of,
        last_successful_import_at: completed_at,
    })
}

async fn approval_history(
    pool: &PgPool,
    policy_id: Uuid,
) -> Result<Vec<ApprovalPolicyVersion>, RepositoryError> {
    let db_rows = sqlx::query(
        "SELECT revision, digest, matrix::text, change_reason, created_at \
         FROM project_approval_policy_versions WHERE policy_id = $1 ORDER BY revision DESC",
    )
    .bind(policy_id)
    .fetch_all(pool)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    Ok(db_rows
        .into_iter()
        .map(|row| {
            let matrix_json: String = row.get(2);
            ApprovalPolicyVersion {
                revision: row.get(0),
                digest: row.get(1),
                matrix: rows::parse_matrix(&matrix_json),
                change_reason: row.get(3),
                created_at: row.get(4),
            }
        })
        .collect())
}

async fn approval(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Option<ApprovalPolicy>, RepositoryError> {
    let row = sqlx::query(
        "SELECT policy.id, version.revision, version.digest, version.matrix::text, version.change_reason, version.created_at \
         FROM project_approval_policies policy \
         JOIN project_approval_policy_versions version ON version.policy_id = policy.id AND version.revision = policy.current_revision \
         WHERE policy.project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let policy_id: Uuid = row.get(0);
    let matrix_json: String = row.get(3);
    Ok(Some(ApprovalPolicy {
        id: policy_id,
        revision: row.get(1),
        digest: row.get(2),
        matrix: rows::parse_matrix(&matrix_json),
        change_reason: row.get(4),
        created_at: row.get(5),
        history: approval_history(pool, policy_id).await?,
    }))
}

async fn settings_connections(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<ProjectSettingsConnection>, RepositoryError> {
    let db_rows = sqlx::query(
        "SELECT id, display_name, definition_version, environment, credential_status, lifecycle_status, revision \
         FROM project_settings_connections WHERE project_id = $1 ORDER BY display_name, id",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|error| RepositoryError::Other(error.into()))?;
    Ok(db_rows
        .into_iter()
        .map(|row| ProjectSettingsConnection {
            id: row.get(0),
            display_name: row.get(1),
            definition_version: row.get(2),
            environment: row.get(3),
            credential_status: row.get(4),
            lifecycle_status: row.get(5),
            agent_count: 0,
            revision: row.get(6),
        })
        .collect())
}

pub async fn organization(
    pool: &PgPool,
    actor: Uuid,
    organization_id: Uuid,
) -> Result<Option<OrganizationAdministration>, RepositoryError> {
    let row = sqlx::query("SELECT id, slug, display_name, lifecycle_status, revision FROM organizations WHERE id = $1")
        .bind(organization_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(OrganizationAdministration {
        id: organization_id,
        slug: row.get(1),
        display_name: row.get(2),
        lifecycle_status: row.get(3),
        revision: row.get(4),
        memberships: memberships(pool, "ORGANIZATION", organization_id).await?,
        available_principals: organization_principals(pool, organization_id).await?,
        assignable_roles: vec![
            "AUDITOR".to_string(),
            "ORGANIZATION_ADMIN".to_string(),
            "ORGANIZATION_MEMBER".to_string(),
        ],
        capabilities: scope_capabilities(
            pool,
            actor,
            crate::capability::Scope::Organization(organization_id),
        )
        .await?,
    }))
}

pub async fn project(
    pool: &PgPool,
    actor: Uuid,
    project_id: Uuid,
) -> Result<Option<ProjectAdministration>, RepositoryError> {
    let row = sqlx::query("SELECT id, organization_id, slug, display_name, description, lifecycle_status, revision FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| RepositoryError::Other(error.into()))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let organization_id: Uuid = row.get(1);
    let budget_policy = budget(pool, project_id).await?;
    Ok(Some(ProjectAdministration {
        id: project_id,
        organization_id,
        slug: row.get(2),
        display_name: row.get(3),
        description: row.get(4),
        lifecycle_status: row.get(5),
        revision: row.get(6),
        memberships: memberships(pool, "PROJECT", project_id).await?,
        available_principals: organization_principals(pool, organization_id).await?,
        assignable_roles: assignable_project_roles(pool, actor).await?,
        budget_history: budget_history(pool, project_id).await?,
        budget_status: budget_status(pool, project_id, budget_policy.as_ref()).await?,
        budget_policy,
        approval_policy: approval(pool, project_id).await?,
        connections: settings_connections(pool, project_id).await?,
        capabilities: scope_capabilities(
            pool,
            actor,
            crate::capability::Scope::Project(project_id),
        )
        .await?,
    }))
}

/// Ports `resultFor`: the refreshed projection every write command returns, read after
/// `tx.commit()` via the pool rather than the just-committed transaction. The commit has already
/// made the write durable and visible, so this is only an extra round trip versus Java's
/// same-transaction read, not a correctness difference.
pub async fn result_for(
    pool: &PgPool,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
) -> Result<AdministrationMutationResult, RepositoryError> {
    match scope {
        AdministrationScope::Organization => {
            let value = organization(pool, actor, scope_id).await?;
            Ok(AdministrationMutationResult::organization(value.expect(
                "organization row existed inside the just-committed transaction",
            )))
        }
        AdministrationScope::Project => {
            let value = project(pool, actor, scope_id).await?;
            Ok(AdministrationMutationResult::project(value.expect(
                "project row existed inside the just-committed transaction",
            )))
        }
    }
}
