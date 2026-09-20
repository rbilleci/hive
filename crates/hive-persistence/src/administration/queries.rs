//! Ports the read-only `AdministrationRepository` methods: `findOrganization`/`findProject` and
//! every assembly helper they call.
//!
//! `GSR-PERSISTENCE`: runs through `sea_orm::ConnectionTrait` via
//! `Statement::from_sql_and_values` + `query_one_raw`/`query_all_raw`, preserving every SQL string
//! verbatim (same idiom as `capability`/`console`/module 6/`configuration`, `GSR-PHASE-P5`/`-P6`).

use super::rows;
use chrono::{DateTime, Utc};
use hive_application::administration::{
    AdministrationMembership, AdministrationMutationResult, AdministrationPrincipal,
    AdministrationRepositoryError as RepositoryError, AdministrationScope, ApprovalPolicy,
    ApprovalPolicyVersion, BudgetPolicy, BudgetStatus, OrganizationAdministration,
    ProjectAdministration, ProjectSettingsConnection,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use uuid::Uuid;

fn other(error: sea_orm::DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

pub async fn find_organization(
    db: &DatabaseConnection,
    principal: Uuid,
    organization_id: Uuid,
) -> Result<Option<OrganizationAdministration>, RepositoryError> {
    let visible = crate::capability::has_capability(
        db,
        principal,
        crate::capability::ORGANIZATION_VIEW,
        crate::capability::Scope::Organization(organization_id),
        false,
    )
    .await
    .map_err(other)?;
    if !visible {
        return Ok(None);
    }
    organization(db, principal, organization_id).await
}

pub async fn find_project(
    db: &DatabaseConnection,
    principal: Uuid,
    project_id: Uuid,
) -> Result<Option<ProjectAdministration>, RepositoryError> {
    let visible = crate::capability::has_capability(
        db,
        principal,
        crate::capability::PROJECT_VIEW,
        crate::capability::Scope::Project(project_id),
        false,
    )
    .await
    .map_err(other)?;
    if !visible {
        return Ok(None);
    }
    project(db, principal, project_id).await
}

async fn roles(
    db: &impl ConnectionTrait,
    scope: &str,
    membership_id: Uuid,
) -> Result<Vec<String>, RepositoryError> {
    let sql = format!(
        "SELECT role_code FROM {} WHERE membership_id = $1 ORDER BY role_code",
        rows::role_table(scope)
    );
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [membership_id.into()]);
    let db_rows = db.query_all_raw(statement).await.map_err(other)?;
    db_rows
        .iter()
        .map(|row| row.try_get_by("role_code"))
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(other)
}

async fn project_access(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Uuid,
    organization_roles: &[String],
) -> Result<Vec<String>, RepositoryError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT membership.id, project.slug, project.display_name FROM project_memberships membership \
         JOIN projects project ON project.id = membership.project_id \
         WHERE membership.principal_id = $1 AND project.organization_id = $2 \
           AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL \
         ORDER BY project.display_name, project.id",
        [principal_id.into(), organization_id.into()],
    );
    let db_rows = db.query_all_raw(statement).await.map_err(other)?;

    let mut summaries = Vec::new();
    if organization_roles
        .iter()
        .any(|role| role == "ORGANIZATION_ADMIN")
    {
        summaries.push("All organization projects (ORGANIZATION_ADMIN)".to_string());
    }
    for row in db_rows {
        let display_name: String = row.try_get_by("display_name").map_err(other)?;
        let membership_id: Uuid = row.try_get_by("id").map_err(other)?;
        let project_roles = roles(db, "PROJECT", membership_id).await?;
        summaries.push(format!("{display_name} ({})", project_roles.join(", ")));
    }
    Ok(summaries)
}

async fn memberships(
    db: &impl ConnectionTrait,
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
    let statement =
        Statement::from_sql_and_values(db.get_database_backend(), &sql, [scope_id.into()]);
    let db_rows = db.query_all_raw(statement).await.map_err(other)?;

    let mut result = Vec::with_capacity(db_rows.len());
    for row in db_rows {
        let membership_id: Uuid = row.try_get_by("id").map_err(other)?;
        let principal_id: Uuid = row.try_get_by("principal_id").map_err(other)?;
        let membership_roles = roles(db, scope, membership_id).await?;
        let project_access_summary = if scope == "ORGANIZATION" {
            project_access(db, principal_id, scope_id, &membership_roles).await?
        } else {
            Vec::new()
        };
        result.push(AdministrationMembership {
            id: membership_id,
            principal_id,
            display_name: row.try_get_by("display_name").map_err(other)?,
            email: row.try_get_by("email").map_err(other)?,
            role_codes: membership_roles,
            project_access_summary,
            started_at: row.try_get_by("started_at").map_err(other)?,
            ended_at: row.try_get_by("ended_at").map_err(other)?,
            last_seen_at: row.try_get_by("last_seen_at").map_err(other)?,
            revision: row.try_get_by("revision").map_err(other)?,
        });
    }
    Ok(result)
}

async fn organization_principals(
    db: &impl ConnectionTrait,
    organization_id: Uuid,
) -> Result<Vec<AdministrationPrincipal>, RepositoryError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT DISTINCT principal.id, principal.display_name, principal.email \
         FROM principals principal JOIN organization_memberships membership ON membership.principal_id = principal.id \
         WHERE membership.organization_id = $1 ORDER BY principal.display_name, principal.id",
        [organization_id.into()],
    );
    let db_rows = db.query_all_raw(statement).await.map_err(other)?;
    db_rows
        .iter()
        .map(|row| {
            Ok(AdministrationPrincipal {
                id: row.try_get_by("id")?,
                display_name: row.try_get_by("display_name")?,
                email: row.try_get_by("email")?,
            })
        })
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(other)
}

async fn budget(
    db: &impl ConnectionTrait,
    project_id: Uuid,
) -> Result<Option<BudgetPolicy>, RepositoryError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT version.revision, version.currency, version.monthly_limit_cents, version.warning_threshold_cents, \
                version.change_reason, version.created_at \
         FROM project_budget_policies policy \
         JOIN project_budget_policy_versions version ON version.project_id = policy.project_id AND version.revision = policy.current_revision \
         WHERE policy.project_id = $1",
        [project_id.into()],
    );
    let Some(row) = db.query_one_raw(statement).await.map_err(other)? else {
        return Ok(None);
    };
    Ok(Some(BudgetPolicy {
        revision: row.try_get_by("revision").map_err(other)?,
        currency: row.try_get_by("currency").map_err(other)?,
        monthly_limit_cents: row.try_get_by("monthly_limit_cents").map_err(other)?,
        warning_threshold_cents: row.try_get_by("warning_threshold_cents").map_err(other)?,
        change_reason: row.try_get_by("change_reason").map_err(other)?,
        created_at: row.try_get_by("created_at").map_err(other)?,
    }))
}

async fn budget_history(
    db: &impl ConnectionTrait,
    project_id: Uuid,
) -> Result<Vec<BudgetPolicy>, RepositoryError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT revision, currency, monthly_limit_cents, warning_threshold_cents, change_reason, created_at \
         FROM project_budget_policy_versions WHERE project_id = $1 ORDER BY revision DESC",
        [project_id.into()],
    );
    let db_rows = db.query_all_raw(statement).await.map_err(other)?;
    db_rows
        .iter()
        .map(|row| {
            Ok(BudgetPolicy {
                revision: row.try_get_by("revision")?,
                currency: row.try_get_by("currency")?,
                monthly_limit_cents: row.try_get_by("monthly_limit_cents")?,
                warning_threshold_cents: row.try_get_by("warning_threshold_cents")?,
                change_reason: row.try_get_by("change_reason")?,
                created_at: row.try_get_by("created_at")?,
            })
        })
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(other)
}

async fn budget_status(
    db: &impl ConnectionTrait,
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

    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT period_start, period_end, currency, state, amount_cents, includes_estimates, data_as_of, completed_at \
         FROM frozen_spend_import_batches WHERE project_id = $1 \
           AND period_start <= date_trunc('month', CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' \
           AND period_end > date_trunc('month', CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' \
         ORDER BY imported_at DESC LIMIT 1",
        [project_id.into()],
    );
    let row = db.query_one_raw(statement).await.map_err(other)?;

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

    let period_start: Option<DateTime<Utc>> = row.try_get_by("period_start").map_err(other)?;
    let period_end: Option<DateTime<Utc>> = row.try_get_by("period_end").map_err(other)?;
    let currency: String = row.try_get_by("currency").map_err(other)?;
    let import_state: String = row.try_get_by("state").map_err(other)?;
    let amount: Option<i32> = row.try_get_by("amount_cents").map_err(other)?;
    let includes_estimates: bool = row.try_get_by("includes_estimates").map_err(other)?;
    let data_as_of: Option<DateTime<Utc>> = row.try_get_by("data_as_of").map_err(other)?;
    let completed_at: Option<DateTime<Utc>> = row.try_get_by("completed_at").map_err(other)?;

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
    db: &impl ConnectionTrait,
    policy_id: Uuid,
) -> Result<Vec<ApprovalPolicyVersion>, RepositoryError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT revision, digest, matrix::text, change_reason, created_at \
         FROM project_approval_policy_versions WHERE policy_id = $1 ORDER BY revision DESC",
        [policy_id.into()],
    );
    let db_rows = db.query_all_raw(statement).await.map_err(other)?;
    db_rows
        .iter()
        .map(|row| {
            let matrix_json: String = row.try_get_by("matrix")?;
            Ok(ApprovalPolicyVersion {
                revision: row.try_get_by("revision")?,
                digest: row.try_get_by("digest")?,
                matrix: rows::parse_matrix(&matrix_json),
                change_reason: row.try_get_by("change_reason")?,
                created_at: row.try_get_by("created_at")?,
            })
        })
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(other)
}

async fn approval(
    db: &impl ConnectionTrait,
    project_id: Uuid,
) -> Result<Option<ApprovalPolicy>, RepositoryError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT policy.id, version.revision, version.digest, version.matrix::text, version.change_reason, version.created_at \
         FROM project_approval_policies policy \
         JOIN project_approval_policy_versions version ON version.policy_id = policy.id AND version.revision = policy.current_revision \
         WHERE policy.project_id = $1",
        [project_id.into()],
    );
    let Some(row) = db.query_one_raw(statement).await.map_err(other)? else {
        return Ok(None);
    };
    let policy_id: Uuid = row.try_get_by("id").map_err(other)?;
    let matrix_json: String = row.try_get_by("matrix").map_err(other)?;
    Ok(Some(ApprovalPolicy {
        id: policy_id,
        revision: row.try_get_by("revision").map_err(other)?,
        digest: row.try_get_by("digest").map_err(other)?,
        matrix: rows::parse_matrix(&matrix_json),
        change_reason: row.try_get_by("change_reason").map_err(other)?,
        created_at: row.try_get_by("created_at").map_err(other)?,
        history: approval_history(db, policy_id).await?,
    }))
}

async fn settings_connections(
    db: &impl ConnectionTrait,
    project_id: Uuid,
) -> Result<Vec<ProjectSettingsConnection>, RepositoryError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, display_name, definition_version, environment, credential_status, lifecycle_status, revision \
         FROM project_settings_connections WHERE project_id = $1 ORDER BY display_name, id",
        [project_id.into()],
    );
    let db_rows = db.query_all_raw(statement).await.map_err(other)?;
    db_rows
        .iter()
        .map(|row| {
            Ok(ProjectSettingsConnection {
                id: row.try_get_by("id")?,
                display_name: row.try_get_by("display_name")?,
                definition_version: row.try_get_by("definition_version")?,
                environment: row.try_get_by("environment")?,
                credential_status: row.try_get_by("credential_status")?,
                lifecycle_status: row.try_get_by("lifecycle_status")?,
                agent_count: 0,
                revision: row.try_get_by("revision")?,
            })
        })
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(other)
}

pub async fn organization(
    db: &impl ConnectionTrait,
    actor: Uuid,
    organization_id: Uuid,
) -> Result<Option<OrganizationAdministration>, RepositoryError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, slug, display_name, lifecycle_status, revision FROM organizations WHERE id = $1",
        [organization_id.into()],
    );
    let Some(row) = db.query_one_raw(statement).await.map_err(other)? else {
        return Ok(None);
    };
    Ok(Some(OrganizationAdministration {
        id: organization_id,
        slug: row.try_get_by("slug").map_err(other)?,
        display_name: row.try_get_by("display_name").map_err(other)?,
        lifecycle_status: row.try_get_by("lifecycle_status").map_err(other)?,
        revision: row.try_get_by("revision").map_err(other)?,
        memberships: memberships(db, "ORGANIZATION", organization_id).await?,
        available_principals: organization_principals(db, organization_id).await?,
        assignable_roles: vec![
            "AUDITOR".to_string(),
            "ORGANIZATION_ADMIN".to_string(),
            "ORGANIZATION_MEMBER".to_string(),
        ],
        capabilities: scope_capabilities(
            db,
            actor,
            crate::capability::Scope::Organization(organization_id),
        )
        .await?,
    }))
}

pub async fn project(
    db: &impl ConnectionTrait,
    actor: Uuid,
    project_id: Uuid,
) -> Result<Option<ProjectAdministration>, RepositoryError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id, organization_id, slug, display_name, description, lifecycle_status, revision FROM projects WHERE id = $1",
        [project_id.into()],
    );
    let Some(row) = db.query_one_raw(statement).await.map_err(other)? else {
        return Ok(None);
    };
    let organization_id: Uuid = row.try_get_by("organization_id").map_err(other)?;
    let budget_policy = budget(db, project_id).await?;
    Ok(Some(ProjectAdministration {
        id: project_id,
        organization_id,
        slug: row.try_get_by("slug").map_err(other)?,
        display_name: row.try_get_by("display_name").map_err(other)?,
        description: row.try_get_by("description").map_err(other)?,
        lifecycle_status: row.try_get_by("lifecycle_status").map_err(other)?,
        revision: row.try_get_by("revision").map_err(other)?,
        memberships: memberships(db, "PROJECT", project_id).await?,
        available_principals: organization_principals(db, organization_id).await?,
        assignable_roles: assignable_project_roles(db, actor).await?,
        budget_history: budget_history(db, project_id).await?,
        budget_status: budget_status(db, project_id, budget_policy.as_ref()).await?,
        budget_policy,
        approval_policy: approval(db, project_id).await?,
        connections: settings_connections(db, project_id).await?,
        capabilities: scope_capabilities(db, actor, crate::capability::Scope::Project(project_id))
            .await?,
    }))
}

async fn scope_capabilities(
    db: &impl ConnectionTrait,
    actor: Uuid,
    scope: crate::capability::Scope,
) -> Result<Vec<String>, RepositoryError> {
    let mut available = Vec::new();
    for &code in crate::capability::ADMINISTRATION_CAPABILITIES {
        if crate::capability::has_capability(db, actor, code, scope, false)
            .await
            .map_err(other)?
        {
            available.push(code.to_string());
        }
    }
    Ok(available)
}

async fn assignable_project_roles(
    db: &impl ConnectionTrait,
    actor: Uuid,
) -> Result<Vec<String>, RepositoryError> {
    let ordinary = ["AGENT_DEVELOPER", "AUDITOR", "OPERATOR", "PROJECT_ADMIN"];
    let is_platform_administrator = crate::capability::is_platform_administrator(db, actor)
        .await
        .map_err(other)?;
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

/// Ports `resultFor`: the refreshed projection every write command returns, read after
/// `txn.commit()` via the pool rather than the just-committed transaction. The commit has already
/// made the write durable and visible, so this is only an extra round trip versus Java's
/// same-transaction read, not a correctness difference.
pub async fn result_for(
    db: &DatabaseConnection,
    actor: Uuid,
    scope: AdministrationScope,
    scope_id: Uuid,
) -> Result<AdministrationMutationResult, RepositoryError> {
    match scope {
        AdministrationScope::Organization => {
            let value = organization(db, actor, scope_id).await?;
            Ok(AdministrationMutationResult::organization(value.expect(
                "organization row existed inside the just-committed transaction",
            )))
        }
        AdministrationScope::Project => {
            let value = project(db, actor, scope_id).await?;
            Ok(AdministrationMutationResult::project(value.expect(
                "project row existed inside the just-committed transaction",
            )))
        }
    }
}
