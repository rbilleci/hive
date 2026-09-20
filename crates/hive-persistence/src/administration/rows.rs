//! Row-mapping helpers shared by `queries`/`mutations`: scope-to-table-name encodings, the
//! approval policy matrix's JSON/digest encoding, locked single-row fetchers, and the pure
//! predicates that compare a locked row against a proposed next value.

use chrono::{DateTime, Utc};
use hive_application::administration::{ApprovalRule, BudgetPolicy};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use uuid::Uuid;

use hive_application::administration::AdministrationScope;

pub fn membership_table(scope: &str) -> &'static str {
    if scope == "ORGANIZATION" {
        "organization_memberships"
    } else {
        "project_memberships"
    }
}

pub fn role_table(scope: &str) -> &'static str {
    if scope == "ORGANIZATION" {
        "organization_membership_roles"
    } else {
        "project_membership_roles"
    }
}

pub fn scope_name(scope: AdministrationScope) -> &'static str {
    match scope {
        AdministrationScope::Organization => "ORGANIZATION",
        AdministrationScope::Project => "PROJECT",
    }
}

pub fn lifecycle_table(scope: AdministrationScope) -> &'static str {
    match scope {
        AdministrationScope::Organization => "organizations",
        AdministrationScope::Project => "projects",
    }
}

pub fn scope_id_column(scope: AdministrationScope) -> &'static str {
    match scope {
        AdministrationScope::Organization => "organization_id",
        AdministrationScope::Project => "project_id",
    }
}

#[derive(Deserialize)]
struct RawApprovalRule {
    #[serde(rename = "requiredEvidence")]
    required_evidence: Vec<String>,
    #[serde(rename = "requiredApprovers")]
    required_approvers: i32,
}

pub fn parse_matrix(json: &str) -> BTreeMap<String, ApprovalRule> {
    let raw: BTreeMap<String, RawApprovalRule> =
        serde_json::from_str(json).expect("approval policy matrix column is always valid JSON");
    raw.into_iter()
        .map(|(cell, rule)| {
            let mut evidence = rule.required_evidence;
            evidence.sort();
            (
                cell,
                ApprovalRule {
                    required_evidence: evidence,
                    required_approvers: rule.required_approvers,
                },
            )
        })
        .collect()
}

pub fn matrix_json(matrix: &BTreeMap<String, ApprovalRule>) -> String {
    let mut encoded = String::from("{");
    let mut first_cell = true;
    for (cell, rule) in matrix {
        if !first_cell {
            encoded.push(',');
        }
        first_cell = false;
        encoded.push('"');
        encoded.push_str(cell);
        encoded.push_str("\":{\"requiredApprovers\":");
        encoded.push_str(&rule.required_approvers.to_string());
        encoded.push_str(",\"requiredEvidence\":[");
        let mut sorted_evidence = rule.required_evidence.clone();
        sorted_evidence.sort();
        let mut first_evidence = true;
        for evidence in &sorted_evidence {
            if !first_evidence {
                encoded.push(',');
            }
            first_evidence = false;
            encoded.push('"');
            encoded.push_str(evidence);
            encoded.push('"');
        }
        encoded.push_str("]}");
    }
    encoded.push('}');
    encoded
}

pub fn default_matrix() -> BTreeMap<String, ApprovalRule> {
    let rule = |evidence: &[&str], approvers: i32| ApprovalRule {
        required_evidence: evidence.iter().map(|value| value.to_string()).collect(),
        required_approvers: approvers,
    };
    BTreeMap::from([
        ("DEVELOPMENT_LOW".to_string(), rule(&["PLAN_VALIDATED"], 0)),
        (
            "DEVELOPMENT_MEDIUM".to_string(),
            rule(&["PLAN_VALIDATED", "CHANGE_SUMMARY_READY"], 0),
        ),
        (
            "DEVELOPMENT_HIGH".to_string(),
            rule(
                &[
                    "PLAN_VALIDATED",
                    "CHANGE_SUMMARY_READY",
                    "EVALUATION_PASSED",
                ],
                1,
            ),
        ),
        (
            "STAGING_LOW".to_string(),
            rule(&["PLAN_VALIDATED", "CHANGE_SUMMARY_READY"], 0),
        ),
        (
            "STAGING_MEDIUM".to_string(),
            rule(
                &[
                    "PLAN_VALIDATED",
                    "CHANGE_SUMMARY_READY",
                    "EVALUATION_PASSED",
                ],
                1,
            ),
        ),
        (
            "STAGING_HIGH".to_string(),
            rule(
                &[
                    "PLAN_VALIDATED",
                    "CHANGE_SUMMARY_READY",
                    "EVALUATION_PASSED",
                ],
                1,
            ),
        ),
        (
            "PRODUCTION_LOW".to_string(),
            rule(
                &[
                    "PLAN_VALIDATED",
                    "CHANGE_SUMMARY_READY",
                    "EVALUATION_PASSED",
                ],
                1,
            ),
        ),
        (
            "PRODUCTION_MEDIUM".to_string(),
            rule(
                &[
                    "PLAN_VALIDATED",
                    "CHANGE_SUMMARY_READY",
                    "EVALUATION_PASSED",
                ],
                1,
            ),
        ),
        (
            "PRODUCTION_HIGH".to_string(),
            rule(
                &[
                    "PLAN_VALIDATED",
                    "CHANGE_SUMMARY_READY",
                    "EVALUATION_PASSED",
                ],
                2,
            ),
        ),
    ])
}

pub fn digest(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

pub struct ScopeRow {
    pub slug: String,
    pub status: String,
    pub revision: i64,
}

pub async fn locked_scope_row(
    conn: &mut PgConnection,
    scope: AdministrationScope,
    id: Uuid,
) -> Result<Option<ScopeRow>, sqlx::Error> {
    let sql = format!(
        "SELECT slug, lifecycle_status, revision FROM {} WHERE id = $1 FOR UPDATE",
        lifecycle_table(scope)
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|row| ScopeRow {
        slug: row.get(0),
        status: row.get(1),
        revision: row.get(2),
    }))
}

pub struct LockedMembership {
    pub revision: i64,
    pub ended: bool,
    pub principal_id: Uuid,
}

pub async fn locked_membership(
    conn: &mut PgConnection,
    scope: AdministrationScope,
    scope_id: Uuid,
    membership_id: Uuid,
) -> Result<Option<LockedMembership>, sqlx::Error> {
    let sql = format!(
        "SELECT revision, ended_at, principal_id FROM {} WHERE id = $1 AND {} = $2 FOR UPDATE",
        membership_table(scope_name(scope)),
        scope_id_column(scope)
    );
    let row = sqlx::query(&sql)
        .bind(membership_id)
        .bind(scope_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|row| {
        let ended_at: Option<DateTime<Utc>> = row.get(1);
        LockedMembership {
            revision: row.get(0),
            ended: ended_at.is_some(),
            principal_id: row.get(2),
        }
    }))
}

pub async fn current_roles_tx(
    conn: &mut PgConnection,
    scope: AdministrationScope,
    membership_id: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    let sql = format!(
        "SELECT role_code FROM {} WHERE membership_id = $1 ORDER BY role_code",
        role_table(scope_name(scope))
    );
    let rows = sqlx::query(&sql)
        .bind(membership_id)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.into_iter().map(|row| row.get(0)).collect())
}

pub struct LockedConnection {
    pub display_name: String,
    pub definition_version: String,
    pub environment: String,
    pub credential_status: String,
    pub lifecycle_status: String,
    pub revision: i64,
}

pub async fn locked_settings_connection(
    conn: &mut PgConnection,
    project_id: Uuid,
    connection_id: Uuid,
) -> Result<Option<LockedConnection>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT display_name, definition_version, environment, credential_status, lifecycle_status, revision \
         FROM project_settings_connections WHERE id = $1 AND project_id = $2 FOR UPDATE",
    )
    .bind(connection_id)
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| LockedConnection {
        display_name: row.get(0),
        definition_version: row.get(1),
        environment: row.get(2),
        credential_status: row.get(3),
        lifecycle_status: row.get(4),
        revision: row.get(5),
    }))
}

pub async fn current_budget_tx(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> Result<Option<BudgetPolicy>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT version.revision, version.currency, version.monthly_limit_cents, version.warning_threshold_cents, \
                version.change_reason, version.created_at \
         FROM project_budget_policies policy \
         JOIN project_budget_policy_versions version ON version.project_id = policy.project_id AND version.revision = policy.current_revision \
         WHERE policy.project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| BudgetPolicy {
        revision: row.get(0),
        currency: row.get(1),
        monthly_limit_cents: row.get(2),
        warning_threshold_cents: row.get(3),
        change_reason: row.get(4),
        created_at: row.get(5),
    }))
}

pub struct CurrentApproval {
    pub id: Uuid,
    pub revision: i64,
    pub digest: String,
    pub matrix: BTreeMap<String, ApprovalRule>,
}

pub async fn current_approval_tx(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> Result<Option<CurrentApproval>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT policy.id, version.revision, version.digest, version.matrix::text \
         FROM project_approval_policies policy \
         JOIN project_approval_policy_versions version ON version.policy_id = policy.id AND version.revision = policy.current_revision \
         WHERE policy.project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| {
        let matrix_json: String = row.get(3);
        CurrentApproval {
            id: row.get(0),
            revision: row.get(1),
            digest: row.get(2),
            matrix: parse_matrix(&matrix_json),
        }
    }))
}

pub fn weakens(
    prior: &BTreeMap<String, ApprovalRule>,
    next: &BTreeMap<String, ApprovalRule>,
) -> bool {
    for (cell, previous) in prior {
        match next.get(cell) {
            None => return true,
            Some(replacement) => {
                if replacement.required_approvers < previous.required_approvers {
                    return true;
                }
                if !previous
                    .required_evidence
                    .iter()
                    .all(|evidence| replacement.required_evidence.contains(evidence))
                {
                    return true;
                }
            }
        }
    }
    false
}

pub fn same_metadata(
    prior: &LockedConnection,
    display_name: &str,
    definition_version: &str,
    environment: &str,
    credential_status: &str,
) -> bool {
    prior.display_name == display_name
        && prior.definition_version == definition_version
        && prior.environment == environment
        && prior.credential_status == credential_status
}

pub fn same_connection(
    prior: &LockedConnection,
    display_name: &str,
    definition_version: &str,
    environment: &str,
    credential_status: &str,
    lifecycle_status: &str,
) -> bool {
    same_metadata(
        prior,
        display_name,
        definition_version,
        environment,
        credential_status,
    ) && prior.lifecycle_status == lifecycle_status
}

pub fn safety_reducing(before: &str, after: &str) -> bool {
    (before == "ACTIVE" && (after == "DISABLED" || after == "ARCHIVED"))
        || (before == "DISABLED" && after == "ARCHIVED")
}

pub fn canonical_roles(roles: &[String]) -> String {
    let mut sorted = roles.to_vec();
    sorted.sort();
    sorted.join("|")
}

pub fn connection_facts(
    display_name: &str,
    definition_version: &str,
    environment: &str,
    credential_status: &str,
    lifecycle_status: &str,
) -> String {
    format!(
        "{display_name}|{definition_version}|{environment}|{credential_status}|{lifecycle_status}"
    )
}

pub fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db_error) if db_error.code().as_deref() == Some("23505"))
}
