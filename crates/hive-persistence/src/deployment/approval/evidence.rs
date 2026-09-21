//! Whether the frozen cycle's required evidence holds: the deployment, its policy snapshot and
//! its version-1 plan compared in Rust, against the evidence snapshots and their invalidations.
//!
//! Two constructs are expressed in Rust rather than SQL, each time inside the transaction that
//! already holds the row locks:
//!
//! * The frozen-policy match (`policy_matrix -> (class || '_' || risk) -> ...` against
//!   `required_approvers`/`required_evidence`, and the six digest equalities against the plan) is
//!   the deployment, its policy snapshot and its frozen plan read as three rows and compared in
//!   Rust. The plan and the policy snapshot are insert-once rows of the deployment's own creating
//!   transaction, so nothing can change them under a reader.
//! * The evidence relational division (`NOT EXISTS (jsonb_array_elements_text(required_evidence)
//!   WHERE NOT EXISTS (valid snapshot))`) is one read of the deployment's evidence snapshots plus
//!   one read of their invalidations, divided in Rust. `evidence_ready` is exactly
//!   `approval_evidence_issue(..) == None`, so both answer from the same three reads.

use super::wall_clock;
use crate::entity::{
    deployment_evidence_invalidations, deployment_evidence_snapshots, deployment_plan_versions,
    deployment_policy_snapshots, deployments,
};
use hive_application::deployment::ApprovalEvidenceIssue;
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect,
};
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

/// A `jsonb` array of strings as the column holds it.
pub(super) fn string_list(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// SQL equality, where a comparison with `NULL` is never true.
fn sql_eq<T: PartialEq>(left: &Option<T>, right: &Option<T>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left == right)
}

struct PolicyRow {
    binding_digest: Option<String>,
    agent_version_id: Option<Uuid>,
    environment_definition_version_id: Option<Uuid>,
    target_digest: Option<String>,
    plan_digest: Option<String>,
    package_digest: Option<String>,
    required_evidence: Vec<String>,
}

impl From<&deployment_policy_snapshots::Model> for PolicyRow {
    fn from(policy: &deployment_policy_snapshots::Model) -> Self {
        PolicyRow {
            binding_digest: policy.binding_digest.clone(),
            agent_version_id: policy.agent_version_id,
            environment_definition_version_id: policy.environment_definition_version_id,
            target_digest: policy.target_digest.clone(),
            plan_digest: policy.plan_digest.clone(),
            package_digest: policy.package_digest.clone(),
            required_evidence: string_list(&policy.required_evidence),
        }
    }
}

/// The join conditions between the deployment, its frozen policy snapshot and its version-1 plan,
/// decided in Rust over the three rows. Every comparison keeps SQL's own
/// `NULL`-is-never-equal semantics, and a `->` that finds no key is `NULL`, so a policy matrix with
/// no cell for this environment class and risk never matches.
fn policy_matches(
    deployment: &deployments::Model,
    policy: &deployment_policy_snapshots::Model,
    plan: &deployment_plan_versions::Model,
) -> bool {
    if !sql_eq(&policy.agent_version_id, &Some(deployment.agent_version_id))
        || !sql_eq(
            &policy.environment_definition_version_id,
            &deployment.environment_definition_version_id,
        )
        || !sql_eq(&policy.target_digest, &plan.target_digest)
        || !sql_eq(&policy.plan_digest, &Some(plan.plan_digest.clone()))
        || !sql_eq(&policy.package_digest, &Some(plan.package_digest.clone()))
        || policy.logical_environment_class != deployment.environment
    {
        return false;
    }
    let cell = format!(
        "{}_{}",
        policy.logical_environment_class.to_value(),
        policy.risk.to_value()
    );
    let Some(rule) = policy.policy_matrix.get(&cell) else {
        return false;
    };
    rule.get("requiredApprovers") == Some(&json!(policy.required_approvers))
        && rule.get("requiredEvidence") == Some(&policy.required_evidence)
}

/// The deployment's evidence snapshots and the identifiers of every snapshot an invalidation
/// covers, read once for the whole evidence decision.
struct EvidenceFacts {
    snapshots: Vec<deployment_evidence_snapshots::Model>,
    invalidated: HashSet<Uuid>,
}

async fn evidence_facts(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<EvidenceFacts, DbErr> {
    let snapshots = deployment_evidence_snapshots::Entity::find()
        .filter(deployment_evidence_snapshots::Column::DeploymentId.eq(deployment_id))
        .all(db)
        .await?;
    let ids: Vec<Uuid> = snapshots.iter().map(|row| row.id).collect();
    let invalidated = if ids.is_empty() {
        HashSet::new()
    } else {
        deployment_evidence_invalidations::Entity::find()
            .filter(deployment_evidence_invalidations::Column::EvidenceSnapshotId.is_in(ids))
            .select_only()
            .column(deployment_evidence_invalidations::Column::EvidenceSnapshotId)
            .into_tuple::<Uuid>()
            .all(db)
            .await?
            .into_iter()
            .collect()
    };
    Ok(EvidenceFacts {
        snapshots,
        invalidated,
    })
}

impl EvidenceFacts {
    fn digests_match(snapshot: &deployment_evidence_snapshots::Model, policy: &PolicyRow) -> bool {
        sql_eq(&snapshot.binding_digest, &policy.binding_digest)
            && sql_eq(&snapshot.agent_version_id, &policy.agent_version_id)
            && sql_eq(
                &snapshot.environment_definition_version_id,
                &policy.environment_definition_version_id,
            )
            && sql_eq(&snapshot.target_digest, &policy.target_digest)
            && sql_eq(&snapshot.plan_digest, &policy.plan_digest)
            && sql_eq(&snapshot.package_digest, &policy.package_digest)
    }

    fn valid(&self, policy: &PolicyRow, kind: &str, now: DateTimeWithTimeZone) -> bool {
        self.snapshots.iter().any(|snapshot| {
            snapshot.evidence_kind.to_value() == kind
                && Self::digests_match(snapshot, policy)
                && snapshot
                    .expires_at
                    .is_none_or(|expires_at| expires_at > now)
                && !self.invalidated.contains(&snapshot.id)
        })
    }

    fn observed(&self, kind: &str) -> bool {
        self.snapshots
            .iter()
            .any(|snapshot| snapshot.evidence_kind.to_value() == kind)
    }

    fn expired(&self, policy: &PolicyRow, kind: &str, now: DateTimeWithTimeZone) -> bool {
        self.snapshots.iter().any(|snapshot| {
            snapshot.evidence_kind.to_value() == kind
                && Self::digests_match(snapshot, policy)
                && snapshot
                    .expires_at
                    .is_some_and(|expires_at| expires_at <= now)
                && !self.invalidated.contains(&snapshot.id)
        })
    }
}

/// The deployment, its frozen policy snapshot and its version-1 plan, when all three exist.
async fn frozen_cycle(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<
    Option<(
        deployments::Model,
        deployment_policy_snapshots::Model,
        deployment_plan_versions::Model,
    )>,
    DbErr,
> {
    let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let Some(policy) = deployment_policy_snapshots::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let Some(plan) = deployment_plan_versions::Entity::find()
        .filter(deployment_plan_versions::Column::DeploymentId.eq(deployment_id))
        .filter(deployment_plan_versions::Column::VersionNumber.eq(1_i64))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some((deployment, policy, plan)))
}

/// The frozen policy the evidence decision runs against, or `None` when the cycle no longer matches
/// its own plan and policy.
async fn evidence_issue_policy_row(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<PolicyRow>, DbErr> {
    let Some((deployment, policy, plan)) = frozen_cycle(db, deployment_id).await? else {
        return Ok(None);
    };
    if !policy_matches(&deployment, &policy, &plan) {
        return Ok(None);
    }
    Ok(Some(PolicyRow::from(&policy)))
}

/// The policy snapshot of a deployment that still exists, with no frozen-cycle test.
async fn waiting_policy_row(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<PolicyRow>, DbErr> {
    if deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    Ok(
        deployment_policy_snapshots::Entity::find_by_id(deployment_id)
            .one(db)
            .await?
            .as_ref()
            .map(PolicyRow::from),
    )
}

/// Why the frozen cycle's required evidence does not hold, or `None` when every required evidence
/// kind is valid. A cycle that no longer matches its own plan and policy is itself a mismatch.
pub async fn approval_evidence_issue(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<Option<ApprovalEvidenceIssue>, DbErr> {
    let Some(policy) = evidence_issue_policy_row(db, deployment_id).await? else {
        return Ok(Some(ApprovalEvidenceIssue::Mismatch));
    };
    let facts = evidence_facts(db, deployment_id).await?;
    Ok(evidence_issue_of(&policy, &facts))
}

fn evidence_issue_of(policy: &PolicyRow, facts: &EvidenceFacts) -> Option<ApprovalEvidenceIssue> {
    let now = wall_clock();
    for kind in &policy.required_evidence {
        if facts.valid(policy, kind, now) {
            continue;
        }
        if !facts.observed(kind) {
            return Some(ApprovalEvidenceIssue::Missing);
        }
        if facts.expired(policy, kind, now) {
            return Some(ApprovalEvidenceIssue::Expired);
        }
        return Some(ApprovalEvidenceIssue::Mismatch);
    }
    None
}

pub async fn waiting_for_evaluation(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let Some(waiting_policy) = waiting_policy_row(db, deployment_id).await? else {
        return Ok(false);
    };
    if !waiting_policy
        .required_evidence
        .iter()
        .any(|kind| kind == "EVALUATION_PASSED")
    {
        return Ok(false);
    }
    let Some(policy) = evidence_issue_policy_row(db, deployment_id).await? else {
        return Ok(false);
    };
    let facts = evidence_facts(db, deployment_id).await?;
    if evidence_issue_of(&policy, &facts) != Some(ApprovalEvidenceIssue::Missing) {
        return Ok(false);
    }
    if facts.observed("EVALUATION_PASSED") {
        return Ok(false);
    }
    let now = wall_clock();
    for kind in &waiting_policy.required_evidence {
        if kind == "EVALUATION_PASSED" {
            continue;
        }
        if !facts.valid(&policy, kind, now) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The frozen-cycle test plus "no required evidence kind lacks a valid snapshot", which together
/// are exactly `approval_evidence_issue(..) == None` over the same three reads.
pub(super) async fn evidence_ready(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    Ok(approval_evidence_issue(db, deployment_id).await?.is_none())
}

#[cfg(test)]
mod evidence_tests {
    use super::{evidence_issue_of, policy_matches, sql_eq, string_list, EvidenceFacts, PolicyRow};
    use crate::entity::enums::{
        DeploymentEvidenceKind, DeploymentLifecycleStatus, DeploymentRisk, DeploymentStrategy,
        LogicalEnvironmentClass,
    };
    use crate::entity::{
        deployment_evidence_snapshots, deployment_plan_versions, deployment_policy_snapshots,
        deployments,
    };
    use hive_application::deployment::ApprovalEvidenceIssue;
    use sea_orm::prelude::DateTimeWithTimeZone;
    use serde_json::json;
    use std::collections::HashSet;
    use uuid::{uuid, Uuid};

    const DEPLOYMENT: Uuid = uuid!("11111111-1111-1111-1111-111111111111");
    const AGENT_VERSION: Uuid = uuid!("22222222-2222-2222-2222-222222222222");
    const ENVIRONMENT_VERSION: Uuid = uuid!("33333333-3333-3333-3333-333333333333");
    const SNAPSHOT: Uuid = uuid!("44444444-4444-4444-4444-444444444444");

    fn instant(text: &str) -> DateTimeWithTimeZone {
        chrono::DateTime::parse_from_rfc3339(text).expect("an RFC 3339 instant")
    }

    /// Far enough ahead of any `wall_clock()` a test run observes that `evidence_issue_of`, which
    /// takes its own clock, sees these snapshots as unexpired.
    fn distant_future() -> DateTimeWithTimeZone {
        instant("2999-01-01T00:00:00Z")
    }

    fn distant_past() -> DateTimeWithTimeZone {
        instant("2000-01-01T00:00:00Z")
    }

    /// A deployment, its frozen policy snapshot and its plan that `policy_matches` accepts. Each
    /// test changes one cell and asserts the refusal that cell alone causes.
    fn frozen_cycle() -> (
        deployments::Model,
        deployment_policy_snapshots::Model,
        deployment_plan_versions::Model,
    ) {
        let deployment = deployments::Model {
            organization_id: Uuid::nil(),
            project_id: Uuid::nil(),
            agent_id: Uuid::nil(),
            agent_version_id: AGENT_VERSION,
            catalog_release_id: "release".to_string(),
            catalog_release_digest: "c".repeat(64),
            environment: LogicalEnvironmentClass::Production,
            target_digest: "t".repeat(64),
            strategy: DeploymentStrategy::Rolling,
            lifecycle_status: DeploymentLifecycleStatus::AwaitingApproval,
            revision: 1,
            idempotency_key: "idempotency-key".to_string(),
            requested_by: Uuid::nil(),
            requested_at: instant("2026-01-01T00:00:00Z"),
            updated_at: instant("2026-01-01T00:00:00Z"),
            environment_definition_version_id: Some(ENVIRONMENT_VERSION),
            request_fingerprint: None,
            projection_revision: None,
            project_lifecycle_revision: Some(7),
            id: DEPLOYMENT,
        };
        let policy = deployment_policy_snapshots::Model {
            policy_id: Uuid::nil(),
            policy_revision: 1,
            policy_digest: "p".repeat(64),
            policy_matrix: json!({ "PRODUCTION_HIGH": {
                "requiredApprovers": 2,
                "requiredEvidence": ["PLAN_VALIDATED"],
            } }),
            logical_environment_class: LogicalEnvironmentClass::Production,
            risk: DeploymentRisk::High,
            required_evidence: json!(["PLAN_VALIDATED"]),
            required_approvers: 2,
            created_at: instant("2026-01-01T00:00:00Z"),
            agent_version_id: Some(AGENT_VERSION),
            environment_definition_version_id: Some(ENVIRONMENT_VERSION),
            target_digest: Some("t".repeat(64)),
            plan_digest: Some("d".repeat(64)),
            package_digest: Some("k".repeat(64)),
            binding_digest: Some("b".repeat(64)),
            evaluation_requirement_expires_at: None,
            risk_verification_digest: None,
            deployment_id: DEPLOYMENT,
        };
        let plan = deployment_plan_versions::Model {
            deployment_id: DEPLOYMENT,
            version_number: 1,
            agent_version_id: AGENT_VERSION,
            catalog_release_id: "release".to_string(),
            environment: LogicalEnvironmentClass::Production,
            compiler_version: "1".to_string(),
            canonical_plan: json!({}),
            plan_digest: "d".repeat(64),
            package_digest: "k".repeat(64),
            package_reference: "reference".to_string(),
            created_by: Uuid::nil(),
            created_at: instant("2026-01-01T00:00:00Z"),
            environment_definition_version_id: Some(ENVIRONMENT_VERSION),
            agent_content_digest: None,
            catalog_release_digest: None,
            target_digest: Some("t".repeat(64)),
            id: Uuid::nil(),
        };
        (deployment, policy, plan)
    }

    fn evidence(
        kind: DeploymentEvidenceKind,
        expires_at: Option<DateTimeWithTimeZone>,
    ) -> deployment_evidence_snapshots::Model {
        let (_, policy, _) = frozen_cycle();
        deployment_evidence_snapshots::Model {
            deployment_id: DEPLOYMENT,
            evidence_kind: kind,
            evidence_digest: "e".repeat(64),
            expires_at,
            created_at: instant("2026-01-01T00:00:00Z"),
            agent_version_id: policy.agent_version_id,
            environment_definition_version_id: policy.environment_definition_version_id,
            target_digest: policy.target_digest.clone(),
            plan_digest: policy.plan_digest.clone(),
            package_digest: policy.package_digest.clone(),
            binding_digest: policy.binding_digest.clone(),
            source_evaluation_run_id: None,
            id: SNAPSHOT,
        }
    }

    fn facts(snapshots: Vec<deployment_evidence_snapshots::Model>) -> EvidenceFacts {
        EvidenceFacts {
            snapshots,
            invalidated: HashSet::new(),
        }
    }

    fn policy_row() -> PolicyRow {
        PolicyRow::from(&frozen_cycle().1)
    }

    #[test]
    fn sql_eq_is_true_only_when_both_sides_are_present_and_equal() {
        assert!(sql_eq(&Some(1), &Some(1)));
        assert!(!sql_eq(&Some(1), &Some(2)));
        assert!(!sql_eq(&Some(1), &None));
        assert!(!sql_eq(&None, &Some(1)));
        // The reason this function exists: Rust's `==` answers true here and SQL's does not.
        assert!(!sql_eq::<i32>(&None, &None));
    }

    #[test]
    fn string_list_reads_a_jsonb_array_of_strings_and_nothing_else() {
        assert_eq!(
            string_list(&json!(["PLAN_VALIDATED", "EVALUATION_PASSED"])),
            vec![
                "PLAN_VALIDATED".to_string(),
                "EVALUATION_PASSED".to_string()
            ]
        );
        assert!(string_list(&json!([])).is_empty());
        assert!(string_list(&json!(null)).is_empty());
        assert!(string_list(&json!({ "a": "b" })).is_empty());
        assert!(string_list(&json!("PLAN_VALIDATED")).is_empty());
        assert_eq!(
            string_list(&json!(["PLAN_VALIDATED", 3, null])),
            vec!["PLAN_VALIDATED".to_string()]
        );
    }

    #[test]
    fn policy_matches_the_cycle_it_was_frozen_against() {
        let (deployment, policy, plan) = frozen_cycle();
        assert!(policy_matches(&deployment, &policy, &plan));
    }

    /// A matrix with no cell for the class and risk the snapshot froze is not a weaker rule; it is
    /// no rule, and `->` returning `NULL` must refuse rather than default.
    #[test]
    fn policy_does_not_match_when_the_matrix_has_no_cell_for_its_class_and_risk() {
        let (deployment, mut policy, plan) = frozen_cycle();
        policy.policy_matrix = json!({ "STAGING_HIGH": {
            "requiredApprovers": 2,
            "requiredEvidence": ["PLAN_VALIDATED"],
        } });
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    /// A cell that is present but empty is a different refusal from a cell that is absent, and
    /// both must refuse: the rule's two keys are read, not its presence.
    #[test]
    fn policy_does_not_match_a_cell_that_is_present_but_empty() {
        let (deployment, mut policy, plan) = frozen_cycle();
        policy.policy_matrix = json!({ "PRODUCTION_HIGH": {} });
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    #[test]
    fn policy_does_not_match_a_cell_whose_approver_count_or_evidence_differs() {
        let (deployment, policy, plan) = frozen_cycle();
        let mut fewer_approvers = policy.clone();
        fewer_approvers.policy_matrix = json!({ "PRODUCTION_HIGH": {
            "requiredApprovers": 1,
            "requiredEvidence": ["PLAN_VALIDATED"],
        } });
        assert!(!policy_matches(&deployment, &fewer_approvers, &plan));

        let mut other_evidence = policy;
        other_evidence.policy_matrix = json!({ "PRODUCTION_HIGH": {
            "requiredApprovers": 2,
            "requiredEvidence": ["EVALUATION_PASSED"],
        } });
        assert!(!policy_matches(&deployment, &other_evidence, &plan));
    }

    /// Both sides absent is the case SQL refuses and Rust's `==` would admit. A deployment with no
    /// environment definition must not match a snapshot that froze none either.
    #[test]
    fn policy_does_not_match_when_both_environment_definition_versions_are_absent() {
        let (mut deployment, mut policy, plan) = frozen_cycle();
        deployment.environment_definition_version_id = None;
        policy.environment_definition_version_id = None;
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    #[test]
    fn policy_does_not_match_when_the_plan_carries_no_target_digest() {
        let (deployment, policy, mut plan) = frozen_cycle();
        plan.target_digest = None;
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    #[test]
    fn policy_does_not_match_a_deployment_in_another_environment_class() {
        let (mut deployment, policy, plan) = frozen_cycle();
        deployment.environment = LogicalEnvironmentClass::Staging;
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    #[test]
    fn policy_does_not_match_a_plan_with_another_package_digest() {
        let (deployment, policy, mut plan) = frozen_cycle();
        plan.package_digest = "0".repeat(64);
        assert!(!policy_matches(&deployment, &policy, &plan));
    }

    /// The expiry comparison is strict on one side and inclusive on the other, so a snapshot that
    /// expires at exactly the instant under test is expired, never valid, and the two answers
    /// never both hold.
    #[test]
    fn evidence_expiring_at_the_instant_is_expired_and_not_valid() {
        let now = instant("2026-06-01T12:00:00Z");
        let policy = policy_row();
        let kind = "PLAN_VALIDATED";

        let at = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(now),
        )]);
        assert!(!at.valid(&policy, kind, now));
        assert!(at.expired(&policy, kind, now));

        let after = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(instant("2026-06-01T12:00:01Z")),
        )]);
        assert!(after.valid(&policy, kind, now));
        assert!(!after.expired(&policy, kind, now));

        let before = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(instant("2026-06-01T11:59:59Z")),
        )]);
        assert!(!before.valid(&policy, kind, now));
        assert!(before.expired(&policy, kind, now));
    }

    #[test]
    fn evidence_with_no_expiry_never_expires() {
        let now = instant("2026-06-01T12:00:00Z");
        let policy = policy_row();
        let facts = facts(vec![evidence(DeploymentEvidenceKind::PlanValidated, None)]);
        assert!(facts.valid(&policy, "PLAN_VALIDATED", now));
        assert!(!facts.expired(&policy, "PLAN_VALIDATED", now));
    }

    /// An invalidated snapshot is neither valid nor expired, but it was still observed — which is
    /// what turns the refusal from `MISSING` into `MISMATCH`.
    #[test]
    fn an_invalidated_snapshot_is_neither_valid_nor_expired_but_stays_observed() {
        let now = instant("2026-06-01T12:00:00Z");
        let policy = policy_row();
        let facts = EvidenceFacts {
            snapshots: vec![evidence(
                DeploymentEvidenceKind::PlanValidated,
                Some(instant("2026-06-01T11:00:00Z")),
            )],
            invalidated: HashSet::from([SNAPSHOT]),
        };
        assert!(!facts.valid(&policy, "PLAN_VALIDATED", now));
        assert!(!facts.expired(&policy, "PLAN_VALIDATED", now));
        assert!(facts.observed("PLAN_VALIDATED"));
    }

    #[test]
    fn a_snapshot_frozen_against_another_cycle_is_observed_but_not_valid() {
        let now = instant("2026-06-01T12:00:00Z");
        let policy = policy_row();
        let mut snapshot = evidence(DeploymentEvidenceKind::PlanValidated, None);
        snapshot.plan_digest = Some("0".repeat(64));
        let facts = facts(vec![snapshot]);
        assert!(!facts.valid(&policy, "PLAN_VALIDATED", now));
        assert!(facts.observed("PLAN_VALIDATED"));
    }

    #[test]
    fn a_cycle_that_requires_no_evidence_has_no_issue() {
        let mut policy = policy_row();
        policy.required_evidence = Vec::new();
        assert_eq!(evidence_issue_of(&policy, &facts(Vec::new())), None);
    }

    #[test]
    fn evidence_never_observed_is_missing() {
        assert_eq!(
            evidence_issue_of(&policy_row(), &facts(Vec::new())),
            Some(ApprovalEvidenceIssue::Missing)
        );
    }

    /// Observed, matching and past its expiry: `EXPIRED`, not `MISSING` and not `MISMATCH`.
    #[test]
    fn evidence_observed_and_past_its_expiry_is_expired() {
        let facts = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(distant_past()),
        )]);
        assert_eq!(
            evidence_issue_of(&policy_row(), &facts),
            Some(ApprovalEvidenceIssue::Expired)
        );
    }

    /// Observed and unexpired, but frozen against another plan: `MISMATCH`. This is the refusal a
    /// re-planned deployment must get instead of silently reusing old evidence.
    #[test]
    fn evidence_observed_against_another_cycle_is_a_mismatch() {
        let mut snapshot = evidence(DeploymentEvidenceKind::PlanValidated, None);
        snapshot.binding_digest = Some("0".repeat(64));
        assert_eq!(
            evidence_issue_of(&policy_row(), &facts(vec![snapshot])),
            Some(ApprovalEvidenceIssue::Mismatch)
        );
    }

    #[test]
    fn a_valid_snapshot_for_every_required_kind_has_no_issue() {
        let facts = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(distant_future()),
        )]);
        assert_eq!(evidence_issue_of(&policy_row(), &facts), None);
    }

    /// The loop returns on the first required kind that fails, so a satisfied kind listed first
    /// does not mask a missing one listed second.
    #[test]
    fn the_first_unsatisfied_required_kind_decides_the_issue() {
        let mut policy = policy_row();
        policy.required_evidence = vec![
            "PLAN_VALIDATED".to_string(),
            "EVALUATION_PASSED".to_string(),
        ];
        let facts = facts(vec![evidence(
            DeploymentEvidenceKind::PlanValidated,
            Some(distant_future()),
        )]);
        assert_eq!(
            evidence_issue_of(&policy, &facts),
            Some(ApprovalEvidenceIssue::Missing)
        );
    }
}
