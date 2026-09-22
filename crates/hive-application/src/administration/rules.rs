//! The pure administration rules: the approval policy matrix's canonical encoding and digest, the
//! "a policy may only become stronger" check, the settings-connection lifecycle rules, the role
//! lists an administrator may assign, and the informational budget status. None of them reads the
//! database; the persistence layer loads the rows and asks here.

use super::models::{ApprovalRule, BudgetStatus};
use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const ORGANIZATION_ROLES: [&str; 3] = ["ORGANIZATION_MEMBER", "ORGANIZATION_ADMIN", "AUDITOR"];
pub const PROJECT_ROLES: [&str; 5] = [
    "PROJECT_ADMIN",
    "AGENT_DEVELOPER",
    "OPERATOR",
    "DEPLOYMENT_APPROVER",
    "AUDITOR",
];
pub const DEPLOYMENT_APPROVER: &str = "DEPLOYMENT_APPROVER";

/// The nine fixed environment-by-risk cells of the approval policy matrix.
pub const CELLS: [&str; 9] = [
    "DEVELOPMENT_LOW",
    "DEVELOPMENT_MEDIUM",
    "DEVELOPMENT_HIGH",
    "STAGING_LOW",
    "STAGING_MEDIUM",
    "STAGING_HIGH",
    "PRODUCTION_LOW",
    "PRODUCTION_MEDIUM",
    "PRODUCTION_HIGH",
];

/// The organization roles an administrator may assign, in display order.
pub fn assignable_organization_roles() -> Vec<String> {
    ["AUDITOR", "ORGANIZATION_ADMIN", "ORGANIZATION_MEMBER"]
        .iter()
        .map(|role| role.to_string())
        .collect()
}

/// The project roles an administrator may assign. Only a platform administrator may grant or
/// remove `DEPLOYMENT_APPROVER`.
pub fn assignable_project_roles(platform_administrator: bool) -> Vec<String> {
    [
        "AGENT_DEVELOPER",
        "AUDITOR",
        DEPLOYMENT_APPROVER,
        "OPERATOR",
        "PROJECT_ADMIN",
    ]
    .iter()
    .filter(|role| platform_administrator || **role != DEPLOYMENT_APPROVER)
    .map(|role| role.to_string())
    .collect()
}

/// Whether moving from `previous` to `next` grants or removes `DEPLOYMENT_APPROVER`.
pub fn changes_deployment_approver(previous: &[String], next: &[String]) -> bool {
    let holds = |roles: &[String]| roles.iter().any(|role| role == DEPLOYMENT_APPROVER);
    holds(previous) != holds(next)
}

#[derive(Deserialize)]
struct StoredApprovalRule {
    #[serde(rename = "requiredEvidence")]
    required_evidence: Vec<String>,
    #[serde(rename = "requiredApprovers")]
    required_approvers: i32,
}

/// The matrix a stored `project_approval_policy_versions.matrix` value holds, evidence sorted.
pub fn parse_matrix(
    value: &serde_json::Value,
) -> Result<BTreeMap<String, ApprovalRule>, serde_json::Error> {
    let stored: BTreeMap<String, StoredApprovalRule> = serde_json::from_value(value.clone())?;
    Ok(stored
        .into_iter()
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
        .collect())
}

/// The canonical JSON text of a matrix: cells and evidence sorted, keys in a fixed order, no
/// whitespace. A policy version's digest is taken over this text, so it must not change.
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

/// The matrix every new project starts with.
pub fn default_matrix() -> BTreeMap<String, ApprovalRule> {
    const PLAN: &[&str] = &["PLAN_VALIDATED"];
    const SUMMARY: &[&str] = &["PLAN_VALIDATED", "CHANGE_SUMMARY_READY"];
    const EVALUATED: &[&str] = &[
        "PLAN_VALIDATED",
        "CHANGE_SUMMARY_READY",
        "EVALUATION_PASSED",
    ];
    [
        ("DEVELOPMENT_LOW", PLAN, 0),
        ("DEVELOPMENT_MEDIUM", SUMMARY, 0),
        ("DEVELOPMENT_HIGH", EVALUATED, 1),
        ("STAGING_LOW", SUMMARY, 0),
        ("STAGING_MEDIUM", EVALUATED, 1),
        ("STAGING_HIGH", EVALUATED, 1),
        ("PRODUCTION_LOW", EVALUATED, 1),
        ("PRODUCTION_MEDIUM", EVALUATED, 1),
        ("PRODUCTION_HIGH", EVALUATED, 2),
    ]
    .into_iter()
    .map(|(cell, evidence, approvers)| {
        (
            cell.to_string(),
            ApprovalRule {
                required_evidence: evidence.iter().map(|value| value.to_string()).collect(),
                required_approvers: approvers,
            },
        )
    })
    .collect()
}

/// SHA-256 of `value`, in lowercase hex.
pub fn digest(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

/// Whether `next` drops a cell, lowers an approver count or drops required evidence of `prior`.
pub fn weakens(
    prior: &BTreeMap<String, ApprovalRule>,
    next: &BTreeMap<String, ApprovalRule>,
) -> bool {
    prior.iter().any(|(cell, previous)| match next.get(cell) {
        None => true,
        Some(replacement) => {
            replacement.required_approvers < previous.required_approvers
                || !previous
                    .required_evidence
                    .iter()
                    .all(|evidence| replacement.required_evidence.contains(evidence))
        }
    })
}

/// The only connection lifecycle moves an archived project still accepts.
pub fn safety_reducing(before: &str, after: &str) -> bool {
    (before == "ACTIVE" && (after == "DISABLED" || after == "ARCHIVED"))
        || (before == "DISABLED" && after == "ARCHIVED")
}

/// The role set an audit digest is taken over.
pub fn canonical_roles(roles: &[String]) -> String {
    let mut sorted = roles.to_vec();
    sorted.sort();
    sorted.join("|")
}

/// The budget policy version in force.
pub struct BudgetPolicyFacts<'a> {
    pub currency: &'a str,
    pub monthly_limit_cents: i32,
    pub warning_threshold_cents: i32,
}

/// The newest frozen spend import batch that covers the current UTC month.
pub struct SpendBatchFacts {
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub currency: String,
    /// `COMPLETE`, `FAILED` or `INCOMPLETE`.
    pub state: String,
    pub amount_cents: Option<i32>,
    pub includes_estimates: bool,
    pub data_as_of: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// The first instant of the UTC month `now` is in. A batch covers the current month when its
/// period starts at or before this instant and ends after it.
pub fn utc_month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .expect("the first day of a month is a valid UTC instant")
}

/// The informational budget status. It never authorizes or blocks work.
pub fn budget_status(
    policy: Option<BudgetPolicyFacts<'_>>,
    batch: Option<SpendBatchFacts>,
    now: DateTime<Utc>,
) -> BudgetStatus {
    let Some(policy) = policy else {
        return BudgetStatus {
            state: "NOT_CONFIGURED".to_string(),
            reason: Some("NO_POLICY".to_string()),
            amount_cents: None,
            includes_estimates: false,
            currency: None,
            period_start: None,
            period_end: None,
            data_as_of: None,
            last_successful_import_at: None,
        };
    };
    let Some(batch) = batch else {
        return BudgetStatus {
            state: "UNKNOWN".to_string(),
            reason: Some("NO_DATA".to_string()),
            amount_cents: None,
            includes_estimates: false,
            currency: Some(policy.currency.to_string()),
            period_start: None,
            period_end: None,
            data_as_of: None,
            last_successful_import_at: None,
        };
    };
    let unknown = |reason: &str, currency: &str| BudgetStatus {
        state: "UNKNOWN".to_string(),
        reason: Some(reason.to_string()),
        amount_cents: None,
        includes_estimates: batch.includes_estimates,
        currency: Some(currency.to_string()),
        period_start: Some(batch.period_start),
        period_end: Some(batch.period_end),
        data_as_of: batch.data_as_of,
        last_successful_import_at: batch.completed_at,
    };
    if policy.currency != batch.currency {
        return unknown("CURRENCY_MISMATCH", policy.currency);
    }
    if batch.state != "COMPLETE" {
        let reason = if batch.state == "FAILED" {
            "IMPORT_FAILED"
        } else {
            "INCOMPLETE_DATA"
        };
        return unknown(reason, &batch.currency);
    }
    if batch
        .completed_at
        .is_none_or(|completed_at| completed_at < now - Duration::hours(24))
    {
        return unknown("STALE_DATA", &batch.currency);
    }
    let amount = batch.amount_cents.unwrap_or(0);
    let state = if amount >= policy.monthly_limit_cents {
        "EXCEEDED"
    } else if amount >= policy.warning_threshold_cents {
        "WARNING"
    } else {
        "NORMAL"
    };
    BudgetStatus {
        state: state.to_string(),
        reason: None,
        amount_cents: Some(amount),
        includes_estimates: batch.includes_estimates,
        currency: Some(batch.currency),
        period_start: Some(batch.period_start),
        period_end: Some(batch.period_end),
        data_as_of: batch.data_as_of,
        last_successful_import_at: batch.completed_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(evidence: &[&str], approvers: i32) -> ApprovalRule {
        ApprovalRule {
            required_evidence: evidence.iter().map(|value| value.to_string()).collect(),
            required_approvers: approvers,
        }
    }

    #[test]
    fn the_canonical_matrix_text_sorts_evidence_and_round_trips() {
        let matrix = BTreeMap::from([(
            "DEVELOPMENT_LOW".to_string(),
            rule(&["PLAN_VALIDATED", "CHANGE_SUMMARY_READY"], 1),
        )]);
        let text = matrix_json(&matrix);
        assert_eq!(
            text,
            r#"{"DEVELOPMENT_LOW":{"requiredApprovers":1,"requiredEvidence":["CHANGE_SUMMARY_READY","PLAN_VALIDATED"]}}"#
        );
        let parsed = parse_matrix(&serde_json::from_str(&text).unwrap()).unwrap();
        assert_eq!(matrix_json(&parsed), text);
    }

    #[test]
    fn the_default_matrix_names_every_cell() {
        let matrix = default_matrix();
        assert_eq!(matrix.keys().map(String::as_str).collect::<Vec<_>>(), {
            let mut cells = CELLS.to_vec();
            cells.sort();
            cells
        });
        assert_eq!(matrix["PRODUCTION_HIGH"].required_approvers, 2);
    }

    #[test]
    fn a_policy_weakens_when_it_drops_evidence_approvers_or_a_cell() {
        let prior = BTreeMap::from([("A".to_string(), rule(&["X", "Y"], 1))]);
        assert!(!weakens(&prior, &prior));
        assert!(!weakens(
            &prior,
            &BTreeMap::from([("A".to_string(), rule(&["X", "Y", "Z"], 2))])
        ));
        assert!(weakens(
            &prior,
            &BTreeMap::from([("A".to_string(), rule(&["X"], 1))])
        ));
        assert!(weakens(
            &prior,
            &BTreeMap::from([("A".to_string(), rule(&["X", "Y"], 0))])
        ));
        assert!(weakens(&prior, &BTreeMap::new()));
    }

    #[test]
    fn only_a_platform_administrator_may_assign_deployment_approver() {
        assert!(!assignable_project_roles(false).contains(&DEPLOYMENT_APPROVER.to_string()));
        assert!(assignable_project_roles(true).contains(&DEPLOYMENT_APPROVER.to_string()));
        assert!(changes_deployment_approver(
            &[],
            &[DEPLOYMENT_APPROVER.to_string()]
        ));
        assert!(!changes_deployment_approver(
            &[DEPLOYMENT_APPROVER.to_string()],
            &[DEPLOYMENT_APPROVER.to_string(), "AUDITOR".to_string()]
        ));
    }

    #[test]
    fn the_utc_month_starts_at_midnight_on_the_first() {
        let now = Utc.with_ymd_and_hms(2026, 9, 20, 13, 45, 12).unwrap();
        assert_eq!(
            utc_month_start(now),
            Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap()
        );
    }

    fn batch(now: DateTime<Utc>) -> SpendBatchFacts {
        SpendBatchFacts {
            period_start: utc_month_start(now),
            period_end: utc_month_start(now) + Duration::days(31),
            currency: "USD".to_string(),
            state: "COMPLETE".to_string(),
            amount_cents: Some(12_345),
            includes_estimates: true,
            data_as_of: Some(now - Duration::hours(1)),
            completed_at: Some(now - Duration::hours(1)),
        }
    }

    fn policy() -> Option<BudgetPolicyFacts<'static>> {
        Some(BudgetPolicyFacts {
            currency: "USD",
            monthly_limit_cents: 100_000,
            warning_threshold_cents: 10_000,
        })
    }

    #[test]
    fn the_budget_status_reports_each_reason() {
        let now = Utc.with_ymd_and_hms(2026, 9, 20, 12, 0, 0).unwrap();
        assert_eq!(budget_status(None, None, now).state, "NOT_CONFIGURED");
        assert_eq!(
            budget_status(policy(), None, now).reason.as_deref(),
            Some("NO_DATA")
        );
        let warning = budget_status(policy(), Some(batch(now)), now);
        assert_eq!(
            (warning.state.as_str(), warning.amount_cents),
            ("WARNING", Some(12_345))
        );
        let mismatch = SpendBatchFacts {
            currency: "EUR".to_string(),
            ..batch(now)
        };
        let status = budget_status(policy(), Some(mismatch), now);
        assert_eq!(status.reason.as_deref(), Some("CURRENCY_MISMATCH"));
        assert_eq!(status.currency.as_deref(), Some("USD"));
        let failed = SpendBatchFacts {
            state: "FAILED".to_string(),
            ..batch(now)
        };
        assert_eq!(
            budget_status(policy(), Some(failed), now).reason.as_deref(),
            Some("IMPORT_FAILED")
        );
        let incomplete = SpendBatchFacts {
            state: "INCOMPLETE".to_string(),
            ..batch(now)
        };
        assert_eq!(
            budget_status(policy(), Some(incomplete), now)
                .reason
                .as_deref(),
            Some("INCOMPLETE_DATA")
        );
        let stale = SpendBatchFacts {
            completed_at: Some(now - Duration::days(2)),
            ..batch(now)
        };
        assert_eq!(
            budget_status(policy(), Some(stale), now).reason.as_deref(),
            Some("STALE_DATA")
        );
    }
}
