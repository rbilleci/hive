//! The batched reads behind `Deployments.plan`, `currentAttempt` and `rollbackTarget`.
//!
//! Seaography wires a dataloader for every `Related` relation it generates, and none for a
//! `#[CustomFields]` resolver, so these three carry their own. `hive-api` registers one of each in
//! the schema data; the resolvers in `computed` load through them.
//!
//! None of the three is a relation field in disguise. `deploymentPlanVersions` and
//! `deploymentAttempts` are already generated relation connections on the same object, and they
//! answer a different question: `plan` is the one plan version the request froze
//! (`version_number = 1`) and `currentAttempt` is the newest attempt, each a single nullable
//! object. `rollbackTarget` is a deployment of the same agent and environment, selected by
//! comparison against the row it hangs off, which no relation expresses.
//!
//! Each loader is registered once for the whole schema and caches nothing: a key is batched with
//! the other keys of its tick and then forgotten, so two requests never share a row.

use crate::entity::{deployment_attempts, deployment_plan_versions, deployments};
use sea_orm::prelude::DateTimeWithTimeZone;
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{
    ColumnTrait, Condition, DatabaseConnection, DbErr, EntityTrait, JoinType, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait,
};
use seaography::async_graphql::dataloader::Loader;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

/// The frozen plan of each deployment: the `deployment_plan_versions` row with `version_number`
/// 1, which the table holds at most one of per deployment.
pub struct FrozenPlanLoader {
    db: DatabaseConnection,
}

impl FrozenPlanLoader {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

impl Loader<Uuid> for FrozenPlanLoader {
    type Value = deployment_plan_versions::Model;
    type Error = Arc<DbErr>;

    async fn load(&self, keys: &[Uuid]) -> Result<HashMap<Uuid, Self::Value>, Self::Error> {
        let rows = deployment_plan_versions::Entity::find()
            .filter(deployment_plan_versions::Column::DeploymentId.is_in(keys.iter().copied()))
            .filter(deployment_plan_versions::Column::VersionNumber.eq(1_i64))
            .all(&self.db)
            .await
            .map_err(Arc::new)?;
        Ok(rows
            .into_iter()
            .map(|row| (row.deployment_id, row))
            .collect())
    }
}

/// The newest execution attempt of each deployment.
pub struct CurrentAttemptLoader {
    db: DatabaseConnection,
}

impl CurrentAttemptLoader {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

impl Loader<Uuid> for CurrentAttemptLoader {
    type Value = deployment_attempts::Model;
    type Error = Arc<DbErr>;

    async fn load(&self, keys: &[Uuid]) -> Result<HashMap<Uuid, Self::Value>, Self::Error> {
        let rows = deployment_attempts::Entity::find()
            .filter(deployment_attempts::Column::DeploymentId.is_in(keys.iter().copied()))
            .order_by_asc(deployment_attempts::Column::AttemptNumber)
            .all(&self.db)
            .await
            .map_err(Arc::new)?;
        // Ascending, so the last row read for a deployment is its newest attempt.
        Ok(rows
            .into_iter()
            .map(|row| (row.deployment_id, row))
            .collect())
    }
}

/// The deployment a rollback would return to, identified by everything the comparison needs from
/// the row the field hangs off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RollbackTargetKey {
    pub project_id: Uuid,
    pub agent_id: Uuid,
    pub environment_definition_version_id: Uuid,
    pub requested_at: DateTimeWithTimeZone,
    pub id: Uuid,
}

impl RollbackTargetKey {
    /// `None` for a deployment with no environment, which has no rollback target at all.
    pub fn of(deployment: &deployments::Model) -> Option<Self> {
        Some(Self {
            project_id: deployment.project_id,
            agent_id: deployment.agent_id,
            environment_definition_version_id: deployment.environment_definition_version_id?,
            requested_at: deployment.requested_at,
            id: deployment.id,
        })
    }

    /// Whether `candidate` is the same agent in the same environment of the same project.
    fn same_target(&self, candidate: &deployments::Model) -> bool {
        candidate.project_id == self.project_id
            && candidate.agent_id == self.agent_id
            && candidate.environment_definition_version_id
                == Some(self.environment_definition_version_id)
    }

    /// Whether `candidate` was requested before the key's own row, ordered by requested instant
    /// and then by identifier, the order the page itself is keyed on.
    fn before(&self, candidate: &deployments::Model) -> bool {
        candidate.id != self.id
            && (candidate.requested_at < self.requested_at
                || (candidate.requested_at == self.requested_at && candidate.id < self.id))
    }
}

pub struct RollbackTargetLoader {
    db: DatabaseConnection,
}

impl RollbackTargetLoader {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

impl Loader<RollbackTargetKey> for RollbackTargetLoader {
    type Value = deployments::Model;
    type Error = Arc<DbErr>;

    async fn load(
        &self,
        keys: &[RollbackTargetKey],
    ) -> Result<HashMap<RollbackTargetKey, Self::Value>, Self::Error> {
        let targets: HashSet<(Uuid, Uuid, Uuid)> = keys
            .iter()
            .map(|key| {
                (
                    key.project_id,
                    key.agent_id,
                    key.environment_definition_version_id,
                )
            })
            .collect();
        let mut of_any_key = Condition::any();
        for (project_id, agent_id, environment_id) in targets {
            of_any_key = of_any_key.add(
                Condition::all()
                    .add(deployments::Column::ProjectId.eq(project_id))
                    .add(deployments::Column::AgentId.eq(agent_id))
                    .add(deployments::Column::EnvironmentDefinitionVersionId.eq(environment_id)),
            );
        }
        // A candidate without a published version, a frozen plan or observed runtime health is
        // passed over, which is what these three inner joins say.
        let candidates = deployments::Entity::find()
            .join(
                JoinType::InnerJoin,
                deployments::Relation::AgentVersions.def(),
            )
            .join(
                JoinType::InnerJoin,
                deployments::Relation::DeploymentPlanVersions
                    .def()
                    .on_condition(|_left, right| {
                        Condition::all().add(
                            Expr::col((right, deployment_plan_versions::Column::VersionNumber))
                                .eq(1_i64),
                        )
                    }),
            )
            .join(
                JoinType::InnerJoin,
                deployments::Relation::DeploymentRuntimeHealth.def(),
            )
            .filter(
                deployments::Column::LifecycleStatus
                    .eq(crate::entity::enums::DeploymentLifecycleStatus::Active),
            )
            .filter(of_any_key)
            .order_by_desc(deployments::Column::RequestedAt)
            .order_by_desc(deployments::Column::Id)
            .all(&self.db)
            .await
            .map_err(Arc::new)?;
        Ok(keys
            .iter()
            .filter_map(|key| {
                let target = candidates
                    .iter()
                    .find(|candidate| key.same_target(candidate) && key.before(candidate))?;
                Some((*key, target.clone()))
            })
            .collect())
    }
}
