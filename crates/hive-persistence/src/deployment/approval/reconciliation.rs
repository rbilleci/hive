//! Terminalizing an approval cycle the frozen facts can no longer satisfy: `reconcile_pending`
//! for one deployment, `block_approval_execution` for a satisfied requirement that may no longer
//! execute, and the two scheduled entry points the maintenance loop and the worker heartbeat
//! share — `reconcile_approval_expiry` over expired requirements and `reconcile_approval_upgrade`
//! over unprocessed project-archive events.
//!
//! The archive boundary is here in both directions: `archive_boundary_condition` selects the
//! events one deployment is covered by, and `archived_candidates` the deployments one event
//! covers.

use super::evidence::{approval_evidence_issue, evidence_ready, waiting_for_evaluation};
use super::requirements::{
    cancel_runtime_health, delete_handoff_release, move_lifecycle, requirement_expired,
    requirement_of, transition_requirement,
};
use super::wall_clock;
use crate::deployment::rows::{audit, system_audit, touch_projection};
use crate::entity::enums::{
    ApprovalInvalidationCode, ApprovalRequirementStatus as EntityRequirementStatus,
    DeploymentLifecycleStatus as EntityLifecycleStatus, LifecycleStatus,
};
use crate::entity::{
    deployment_approval_project_archive_events, deployment_approval_requirements, deployments,
    projects,
};
use hive_application::deployment::{
    ApprovalEvidenceIssue, ApprovalRequirementStatus, DeploymentLifecycleStatus,
};
use sea_orm::sea_query::{Expr, ExprTrait, IntoTableRef, LockType};
use sea_orm::{
    ActiveEnum, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    JoinType, QueryFilter, QueryOrder, QuerySelect, RelationTrait, TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

/// The five-term archive-boundary `OR` — the deployment's project revision against the event's, or
/// its requested-at against the event's archived-at when either revision is unknown — as a
/// condition over the archive events of the deployment's own project. The deployment's three
/// values are read first, so the disjunction collapses to the branch its own row selects.
pub(super) fn archive_boundary_condition(deployment: &deployments::Model) -> Condition {
    use deployment_approval_project_archive_events::Column;
    match deployment.project_lifecycle_revision {
        Some(revision) => Condition::any()
            .add(
                Condition::all()
                    .add(Column::ArchivedProjectRevision.is_not_null())
                    .add(Column::ArchivedProjectRevision.gte(revision)),
            )
            .add(
                Condition::all()
                    .add(Column::ArchivedProjectRevision.is_null())
                    .add(Column::ArchivedAt.gte(deployment.requested_at)),
            ),
        None => Condition::all().add(Column::ArchivedAt.gte(deployment.requested_at)),
    }
}

pub async fn deployment_archive_boundary(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let Some(deployment) = deployments::Entity::find_by_id(deployment_id)
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    Ok(deployment_approval_project_archive_events::Entity::find()
        .filter(
            deployment_approval_project_archive_events::Column::ProjectId.eq(deployment.project_id),
        )
        .filter(archive_boundary_condition(&deployment))
        .select_only()
        .column(deployment_approval_project_archive_events::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .is_some())
}

/// Terminalizes a pending requirement the frozen facts can no longer satisfy: a deployment that
/// has started execution invalidates it, an expired requirement expires it, and an evidence issue
/// invalidates it unless the deployment is still waiting for an evaluation to finish. With no
/// pending requirement, an already-satisfied one past its expiry blocks execution instead.
/// Returns whether it changed anything.
pub async fn reconcile_pending(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
) -> Result<bool, DbErr> {
    let Some(lifecycle_row) = deployments::Entity::find_by_id(deployment_id)
        .lock_exclusive()
        .select_only()
        .column(deployments::Column::LifecycleStatus)
        .into_tuple::<EntityLifecycleStatus>()
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    let lifecycle = DeploymentLifecycleStatus::from(lifecycle_row);

    let pending = deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .filter(
            deployment_approval_requirements::Column::Status.eq(EntityRequirementStatus::Pending),
        )
        .lock_exclusive()
        .select_only()
        .column(deployment_approval_requirements::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?;
    let Some(requirement_id) = pending else {
        let satisfied_expired = deployment_approval_requirements::Entity::find()
            .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
            .filter(
                deployment_approval_requirements::Column::Status
                    .eq(EntityRequirementStatus::Satisfied),
            )
            .filter(deployment_approval_requirements::Column::ExpiresAt.lte(wall_clock()))
            .select_only()
            .column(deployment_approval_requirements::Column::Id)
            .into_tuple::<Uuid>()
            .one(db)
            .await?
            .is_some();
        if satisfied_expired {
            block_approval_execution(db, deployment_id, None).await?;
            return Ok(true);
        }
        return Ok(false);
    };

    let (action, issue): (&str, Option<&str>) = if lifecycle.has_started_execution() {
        ("APPROVAL_INVALIDATED", Some("TERMINAL_LIFECYCLE"))
    } else if requirement_expired(db, requirement_id).await? {
        ("APPROVAL_EXPIRED", Some("APPROVAL_REQUIREMENT_EXPIRED"))
    } else {
        let issue = approval_evidence_issue(db, deployment_id).await?;
        if issue.is_none()
            || (issue == Some(ApprovalEvidenceIssue::Missing)
                && waiting_for_evaluation(db, deployment_id).await?)
        {
            return Ok(false);
        }
        (
            "APPROVAL_INVALIDATED",
            issue.map(ApprovalEvidenceIssue::as_str),
        )
    };
    let status = if action == "APPROVAL_EXPIRED" {
        ApprovalRequirementStatus::Expired
    } else {
        ApprovalRequirementStatus::Invalidated
    };
    transition_requirement(db, requirement_id, status, issue, &[]).await?;
    system_audit(
        db,
        deployment_id,
        None,
        action,
        json!({"requirementId": requirement_id.to_string(), "code": issue}),
    )
    .await?;
    touch_projection(db, deployment_id).await?;
    if matches!(
        lifecycle,
        DeploymentLifecycleStatus::AwaitingApproval | DeploymentLifecycleStatus::Requested
    ) {
        cancel_runtime_health(
            db,
            deployment_id,
            "Approval could not complete with the frozen requirement facts.",
        )
        .await?;
        move_lifecycle(
            db,
            deployment_id,
            EntityLifecycleStatus::Canceled,
            [
                EntityLifecycleStatus::AwaitingApproval,
                EntityLifecycleStatus::Requested,
            ],
            true,
        )
        .await?;
    }
    Ok(true)
}

/// Blocks execution for a satisfied requirement the deployment can no longer honour. `actor` is
/// `None` for a system-attributed block; only `reconcile_project_archives` passes one, the
/// archive event's own actor.
pub async fn block_approval_execution(
    db: &impl ConnectionTrait,
    deployment_id: Uuid,
    actor: Option<Uuid>,
) -> Result<bool, DbErr> {
    let satisfied_and_pending = match requirement_of(db, deployment_id).await? {
        Some(requirement) if requirement.status == EntityRequirementStatus::Satisfied => {
            deployments::Entity::find_by_id(deployment_id)
                .filter(deployments::Column::LifecycleStatus.is_in([
                    EntityLifecycleStatus::Requested,
                    EntityLifecycleStatus::Approved,
                ]))
                .select_only()
                .column(deployments::Column::Id)
                .into_tuple::<Uuid>()
                .one(db)
                .await?
                .is_some()
        }
        _ => false,
    };
    if !satisfied_and_pending {
        return Ok(false);
    }
    let archive_boundary = deployment_archive_boundary(db, deployment_id).await?;
    let project_active = deployments::Entity::find_by_id(deployment_id)
        .find_also_related(projects::Entity)
        .one(db)
        .await?
        .and_then(|(_, project)| project)
        .is_some_and(|project| project.lifecycle_status == LifecycleStatus::Active);
    let any_expired = deployment_approval_requirements::Entity::find()
        .filter(deployment_approval_requirements::Column::DeploymentId.eq(deployment_id))
        .filter(deployment_approval_requirements::Column::ExpiresAt.lte(wall_clock()))
        .select_only()
        .column(deployment_approval_requirements::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .is_some();
    if project_active
        && !archive_boundary
        && evidence_ready(db, deployment_id).await?
        && !any_expired
    {
        return Ok(false);
    }
    let evidence_issue = approval_evidence_issue(db, deployment_id).await?;
    let block_code = if archive_boundary {
        "PROJECT_ARCHIVED"
    } else if project_active {
        evidence_issue.map_or("APPROVAL_EVIDENCE_NO_LONGER_VALID", |issue| issue.as_str())
    } else {
        "PROJECT_NOT_ACTIVE"
    };
    cancel_runtime_health(
        db,
        deployment_id,
        "Execution stopped because frozen approval requirements were no longer executable.",
    )
    .await?;
    let updated = move_lifecycle(
        db,
        deployment_id,
        EntityLifecycleStatus::Canceled,
        [
            EntityLifecycleStatus::Requested,
            EntityLifecycleStatus::Approved,
        ],
        true,
    )
    .await?;
    if updated == 0 {
        return Ok(false);
    }
    delete_handoff_release(db, deployment_id).await?;
    system_audit(
        db,
        deployment_id,
        actor,
        "APPROVAL_EXECUTION_BLOCKED",
        json!({"code": block_code}),
    )
    .await?;
    touch_projection(db, deployment_id).await?;
    Ok(true)
}

async fn expired_approval_requirement_deployments(
    db: &impl ConnectionTrait,
) -> Result<Vec<Uuid>, DbErr> {
    use deployment_approval_requirements::Column;
    deployment_approval_requirements::Entity::find()
        .filter(Column::ExpiresAt.lte(wall_clock()))
        .filter(Column::Status.is_in([
            EntityRequirementStatus::Pending,
            EntityRequirementStatus::Satisfied,
        ]))
        .order_by_asc(Column::ExpiresAt)
        .order_by_asc(Column::Id)
        .limit(50)
        .select_only()
        .column(Column::DeploymentId)
        .into_tuple::<Uuid>()
        .all(db)
        .await
}

/// Fetches one page of expired requirements and reconciles each in its own transaction, so one bad
/// row cannot abort the batch. Every call site already relies on `reconcile_pending`'s own
/// conditional-UPDATE race safety, so no advisory lock is taken here — matching the
/// DSQL-compatibility reasoning `reconcile_pending` itself documents.
pub(crate) async fn reconcile_expired_approval_requirements(
    db: &DatabaseConnection,
) -> Result<crate::ApprovalMaintenanceHealth, DbErr> {
    let deployments = expired_approval_requirement_deployments(db).await?;
    let mut attempted = 0i64;
    let mut reconciled = 0i64;
    let mut failed = 0i64;
    for deployment_id in deployments {
        attempted += 1;
        let txn = db.begin().await?;
        match reconcile_pending(&txn, deployment_id).await {
            Ok(_) => {
                txn.commit().await?;
                reconciled += 1;
            }
            Err(_) => {
                let _ = txn.rollback().await;
                failed += 1;
            }
        }
    }
    Ok(if failed > 0 {
        crate::ApprovalMaintenanceHealth {
            healthy: false,
            failure_code: Some("PARTIAL_RECONCILIATION".to_string()),
            attempted,
            reconciled,
            failed,
        }
    } else {
        crate::ApprovalMaintenanceHealth {
            healthy: true,
            failure_code: None,
            attempted,
            reconciled,
            failed: 0,
        }
    })
}

/// The scheduled maintenance task's expiry entry point, publishing its outcome directly into the
/// shared `/health` state.
pub async fn reconcile_approval_expiry(
    db: &DatabaseConnection,
    state: &crate::ApprovalMaintenanceState,
) {
    state.set_maintenance(crate::ApprovalMaintenanceHealth::in_progress());
    let health = match reconcile_expired_approval_requirements(db).await {
        Ok(health) => health,
        Err(error) => crate::ApprovalMaintenanceHealth {
            healthy: false,
            failure_code: Some(crate::deployment::worker::db_failure_code(&error)),
            attempted: 0,
            reconciled: 0,
            failed: 0,
        },
    };
    state.set_maintenance(health);
}

async fn approval_project_archive_pending(db: &impl ConnectionTrait) -> Result<bool, DbErr> {
    Ok(deployment_approval_project_archive_events::Entity::find()
        .filter(deployment_approval_project_archive_events::Column::ProcessedAt.is_null())
        .select_only()
        .column(deployment_approval_project_archive_events::Column::Id)
        .into_tuple::<Uuid>()
        .one(db)
        .await?
        .is_some())
}

/// The candidate deployments one archive event's boundary covers: the same five-term `OR`, now
/// non-correlated because the event's own revision and instant are read first.
fn archived_candidates(event: &deployment_approval_project_archive_events::Model) -> Condition {
    match event.archived_project_revision {
        Some(revision) => Condition::any()
            .add(
                Condition::all()
                    .add(deployments::Column::ProjectLifecycleRevision.is_not_null())
                    .add(deployments::Column::ProjectLifecycleRevision.lte(revision)),
            )
            .add(
                Condition::all()
                    .add(deployments::Column::ProjectLifecycleRevision.is_null())
                    .add(deployments::Column::RequestedAt.lte(event.archived_at)),
            ),
        None => Condition::all().add(deployments::Column::RequestedAt.lte(event.archived_at)),
    }
}

/// The lifecycle/requirement-status pairs archive reconciliation terminalizes.
fn archived_pending_condition() -> Condition {
    Condition::any()
        .add(
            Condition::all()
                .add(deployments::Column::LifecycleStatus.is_in([
                    EntityLifecycleStatus::AwaitingApproval,
                    EntityLifecycleStatus::Requested,
                ]))
                .add(
                    deployment_approval_requirements::Column::Status
                        .eq(EntityRequirementStatus::Pending),
                ),
        )
        .add(
            Condition::all()
                .add(deployments::Column::LifecycleStatus.is_in([
                    EntityLifecycleStatus::Approved,
                    EntityLifecycleStatus::Requested,
                ]))
                .add(
                    deployment_approval_requirements::Column::Status
                        .eq(EntityRequirementStatus::Satisfied),
                ),
        )
}

#[derive(sea_orm::FromQueryResult)]
struct ArchiveCandidate {
    id: Uuid,
    deployment_id: Uuid,
    status: EntityRequirementStatus,
}

/// Terminalizes every pending/satisfied approval cycle an archived project's boundary now covers.
/// One transaction per archive event (not per candidate row, unlike
/// `reconcile_expired_approval_requirements`), because the `FOR UPDATE` locks on the deployment and
/// requirement rows must span the whole candidate page and every write that follows. A partial
/// failure leaves `processed_at` NULL, so the whole event retries on the next tick; that is safe
/// because every write here is a conditional UPDATE already idempotent against a re-run.
async fn reconcile_project_archives(db: &DatabaseConnection) -> Result<(), DbErr> {
    use deployment_approval_project_archive_events::Column;
    let events = deployment_approval_project_archive_events::Entity::find()
        .filter(Column::ProcessedAt.is_null())
        .order_by_asc(Column::ArchivedAt)
        .order_by_asc(Column::Id)
        .limit(10)
        .all(db)
        .await?;

    for event in events {
        let txn = db.begin().await?;
        let candidate_select = || {
            deployment_approval_requirements::Entity::find()
                .join(
                    JoinType::InnerJoin,
                    deployment_approval_requirements::Relation::Deployments.def(),
                )
                .filter(deployments::Column::ProjectId.eq(event.project_id))
                .filter(archived_candidates(&event))
                .filter(archived_pending_condition())
        };
        let mut candidate_query = candidate_select()
            .order_by_asc(deployments::Column::Id)
            .limit(50)
            .select_only()
            .column(deployment_approval_requirements::Column::Id)
            .column_as(deployments::Column::Id, "deployment_id")
            .column(deployment_approval_requirements::Column::Status);
        QuerySelect::query(&mut candidate_query).lock_with_tables(
            LockType::Update,
            [
                deployments::Entity.into_table_ref(),
                deployment_approval_requirements::Entity.into_table_ref(),
            ],
        );
        let candidates = candidate_query
            .into_model::<ArchiveCandidate>()
            .all(&txn)
            .await?;

        for candidate in candidates {
            if candidate.status == EntityRequirementStatus::Satisfied {
                block_approval_execution(&txn, candidate.deployment_id, event.actor_principal_id)
                    .await?;
                continue;
            }
            let updated = deployment_approval_requirements::Entity::update_many()
                .col_expr(
                    deployment_approval_requirements::Column::Status,
                    Expr::value(EntityRequirementStatus::Invalidated.to_value()),
                )
                .col_expr(
                    deployment_approval_requirements::Column::Revision,
                    Expr::col(deployment_approval_requirements::Column::Revision).add(1),
                )
                .col_expr(
                    deployment_approval_requirements::Column::InvalidatedAt,
                    Expr::current_timestamp(),
                )
                .col_expr(
                    deployment_approval_requirements::Column::InvalidationCode,
                    Expr::value(ApprovalInvalidationCode::ProjectArchived.to_value()),
                )
                .filter(deployment_approval_requirements::Column::Id.eq(candidate.id))
                .filter(
                    deployment_approval_requirements::Column::Status
                        .eq(EntityRequirementStatus::Pending),
                )
                .exec(&txn)
                .await?;
            if updated.rows_affected != 1 {
                continue;
            }
            audit(
                &txn,
                candidate.deployment_id,
                event.actor_principal_id,
                "APPROVAL_INVALIDATED",
                json!({"requirementId": candidate.id.to_string(), "code": "PROJECT_ARCHIVED"}),
            )
            .await?;
            cancel_runtime_health(
                &txn,
                candidate.deployment_id,
                "Project archive terminalized this pending approval cycle.",
            )
            .await?;
            move_lifecycle(
                &txn,
                candidate.deployment_id,
                EntityLifecycleStatus::Canceled,
                [
                    EntityLifecycleStatus::AwaitingApproval,
                    EntityLifecycleStatus::Requested,
                ],
                true,
            )
            .await?;
            touch_projection(&txn, candidate.deployment_id).await?;
        }

        let still_pending = candidate_select()
            .select_only()
            .column(deployment_approval_requirements::Column::Id)
            .into_tuple::<Uuid>()
            .one(&txn)
            .await?
            .is_some();
        if !still_pending {
            deployment_approval_project_archive_events::Entity::update_many()
                .col_expr(Column::ProcessedAt, Expr::current_timestamp())
                .filter(Column::Id.eq(event.id))
                .exec(&txn)
                .await?;
        }
        txn.commit().await?;
    }
    Ok(())
}

/// The scheduled maintenance task's archive entry point: it reconciles project archives and
/// reports whether any archive event is still unprocessed.
pub async fn reconcile_approval_upgrade(
    db: &DatabaseConnection,
    state: &crate::ApprovalMaintenanceState,
) {
    let outcome = async {
        reconcile_project_archives(db).await?;
        approval_project_archive_pending(db).await
    }
    .await;
    let health = match outcome {
        Ok(archive_pending) => crate::ApprovalMaintenanceHealth {
            healthy: !archive_pending,
            failure_code: if archive_pending {
                Some("ARCHIVE_RECONCILIATION_PENDING".to_string())
            } else {
                None
            },
            attempted: 0,
            reconciled: 0,
            failed: 0,
        },
        Err(error) => crate::ApprovalMaintenanceHealth {
            healthy: false,
            failure_code: Some(format!(
                "ARCHIVE_RECONCILIATION_{}",
                crate::deployment::worker::db_failure_code(&error)
            )),
            attempted: 0,
            reconciled: 0,
            failed: 0,
        },
    };
    state.set_upgrade(health);
}

#[cfg(test)]
mod archive_boundary_tests {
    use super::{archive_boundary_condition, archived_candidates, archived_pending_condition};
    use crate::entity::enums::{
        DeploymentLifecycleStatus, DeploymentStrategy, LogicalEnvironmentClass,
    };
    use crate::entity::{deployment_approval_project_archive_events, deployments};
    use sea_orm::prelude::DateTimeWithTimeZone;
    use sea_orm::sea_query::{PostgresQueryBuilder, Query};
    use sea_orm::Condition;
    use uuid::{uuid, Uuid};

    const DEPLOYMENT: Uuid = uuid!("11111111-1111-1111-1111-111111111111");
    const AGENT_VERSION: Uuid = uuid!("22222222-2222-2222-2222-222222222222");
    const ENVIRONMENT_VERSION: Uuid = uuid!("33333333-3333-3333-3333-333333333333");

    fn instant(text: &str) -> DateTimeWithTimeZone {
        chrono::DateTime::parse_from_rfc3339(text).expect("an RFC 3339 instant")
    }

    /// A deployment whose project revision and requested-at instant are the two values the
    /// boundary compares.
    fn deployment() -> deployments::Model {
        deployments::Model {
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
        }
    }

    /// The condition as the SQL the `WHERE` clause applies, so each rule is asserted on the rows
    /// it admits rather than on the builder calls that produced it.
    fn rendered(condition: Condition) -> String {
        Query::select()
            .expr(sea_orm::sea_query::Expr::val(1))
            .cond_where(condition)
            .to_string(PostgresQueryBuilder)
    }

    #[test]
    fn the_archive_boundary_compares_revisions_when_the_deployment_has_one() {
        let deployment = deployment();
        assert_eq!(
            rendered(archive_boundary_condition(&deployment)),
            "SELECT 1 WHERE \
             (\"deployment_approval_project_archive_events\".\"archived_project_revision\" IS NOT NULL \
             AND \"deployment_approval_project_archive_events\".\"archived_project_revision\" >= 7) \
             OR (\"deployment_approval_project_archive_events\".\"archived_project_revision\" IS NULL \
             AND \"deployment_approval_project_archive_events\".\"archived_at\" \
             >= '2026-01-01 00:00:00.000000 +00:00')"
        );
    }

    #[test]
    fn the_archive_boundary_compares_instants_when_the_deployment_has_no_revision() {
        let mut deployment = deployment();
        deployment.project_lifecycle_revision = None;
        assert_eq!(
            rendered(archive_boundary_condition(&deployment)),
            "SELECT 1 WHERE \"deployment_approval_project_archive_events\".\"archived_at\" \
             >= '2026-01-01 00:00:00.000000 +00:00'"
        );
    }

    fn archive_event(
        archived_project_revision: Option<i64>,
    ) -> deployment_approval_project_archive_events::Model {
        deployment_approval_project_archive_events::Model {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            actor_principal_id: None,
            archived_at: instant("2026-01-01T00:00:00Z"),
            processed_at: None,
            archived_project_revision,
        }
    }

    /// The mirror of `archive_boundary_condition`, with every comparison reversed: the event now
    /// selects the deployments it covers rather than the deployment selecting its events.
    #[test]
    fn archived_candidates_reverse_the_boundary_comparisons() {
        assert_eq!(
            rendered(archived_candidates(&archive_event(Some(7)))),
            "SELECT 1 WHERE (\"deployments\".\"project_lifecycle_revision\" IS NOT NULL \
             AND \"deployments\".\"project_lifecycle_revision\" <= 7) \
             OR (\"deployments\".\"project_lifecycle_revision\" IS NULL \
             AND \"deployments\".\"requested_at\" <= '2026-01-01 00:00:00.000000 +00:00')"
        );
        assert_eq!(
            rendered(archived_candidates(&archive_event(None))),
            "SELECT 1 WHERE \"deployments\".\"requested_at\" \
             <= '2026-01-01 00:00:00.000000 +00:00'"
        );
    }

    /// Archive reconciliation terminalizes two lifecycle/status pairings and no others; a
    /// deployment already executing or finished is outside the boundary whatever its requirement
    /// says.
    #[test]
    fn archived_pending_matches_only_the_two_open_cycles() {
        assert_eq!(
            rendered(archived_pending_condition()),
            "SELECT 1 WHERE (\"deployments\".\"lifecycle_status\" IN \
             ('AWAITING_APPROVAL', 'REQUESTED') \
             AND \"deployment_approval_requirements\".\"status\" = 'PENDING') \
             OR (\"deployments\".\"lifecycle_status\" IN ('APPROVED', 'REQUESTED') \
             AND \"deployment_approval_requirements\".\"status\" = 'SATISFIED')"
        );
    }
}
