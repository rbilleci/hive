//! Ports `PostgresAgentOperationalViewRepository`: reads only compact
//! summaries after a single query verifies active membership and route
//! ownership against `agent_operational_view_projection`.
//!
//! `GSR-PERSISTENCE`: runs through `sea_orm::ConnectionTrait` via
//! `Statement::from_sql_and_values` + `query_one_raw`, preserving the original SQL text verbatim
//! (same idiom as `capability`/`console`, `GSR-PHASE-P5`).

use hive_application::agent::{
    AgentOperationalView, AgentOperationalViewRepository,
    AgentOperationalViewRepositoryError as RepositoryError, AliasTargetsSummary, DeploymentSummary,
    DraftValidationSummary, EvaluationSummary, PublishedVersionSummary, RuntimeHealthSummary,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use uuid::Uuid;

pub struct PgAgentOperationalViewRepository {
    db: DatabaseConnection,
}

impl PgAgentOperationalViewRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

const LOOKUP: &str =
    "SELECT overview.agent_id, overview.slug, overview.display_name, overview.lifecycle_status, \
       overview.draft_validation_status, overview.draft_error_count, overview.draft_warning_count, \
       overview.draft_validated_at, overview.published_version_status, overview.published_version, \
       overview.published_at, overview.alias_target_count, overview.active_alias_target_count, \
       overview.deployment_status, overview.deployment_observed_at, overview.evaluation_outcome, \
       overview.evaluation_completed_at, overview.runtime_health, overview.runtime_observed_at, \
       overview.runtime_freshness \
     FROM agent_operational_view_projection overview \
     INNER JOIN organization_memberships membership \
       ON membership.organization_id = overview.organization_id \
     WHERE overview.project_id = $1 \
       AND overview.agent_id = $2 \
       AND membership.principal_id = $3 \
       AND membership.started_at <= CURRENT_TIMESTAMP \
       AND membership.ended_at IS NULL";

#[async_trait::async_trait]
impl AgentOperationalViewRepository for PgAgentOperationalViewRepository {
    async fn find_overview(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
    ) -> Result<Option<AgentOperationalView>, RepositoryError> {
        let statement = Statement::from_sql_and_values(
            self.db.get_database_backend(),
            LOOKUP,
            [project_id.into(), agent_id.into(), principal_id.into()],
        );
        let row = self
            .db
            .query_one_raw(statement)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;
        row.map(|row| {
            Ok(AgentOperationalView {
                id: row.try_get_by("agent_id")?,
                slug: row.try_get_by("slug")?,
                display_name: row.try_get_by("display_name")?,
                lifecycle_status: row.try_get_by("lifecycle_status")?,
                draft_validation: DraftValidationSummary {
                    status: row.try_get_by("draft_validation_status")?,
                    error_count: row.try_get_by("draft_error_count")?,
                    warning_count: row.try_get_by("draft_warning_count")?,
                    validated_at: row.try_get_by("draft_validated_at")?,
                },
                latest_published_version: PublishedVersionSummary {
                    status: row.try_get_by("published_version_status")?,
                    version: row.try_get_by("published_version")?,
                    published_at: row.try_get_by("published_at")?,
                },
                alias_targets: AliasTargetsSummary {
                    total_count: row.try_get_by("alias_target_count")?,
                    active_count: row.try_get_by("active_alias_target_count")?,
                },
                active_deployment: DeploymentSummary {
                    status: row.try_get_by("deployment_status")?,
                    observed_at: row.try_get_by("deployment_observed_at")?,
                },
                recent_evaluation: EvaluationSummary {
                    outcome: row.try_get_by("evaluation_outcome")?,
                    completed_at: row.try_get_by("evaluation_completed_at")?,
                },
                runtime_health: RuntimeHealthSummary {
                    status: row.try_get_by("runtime_health")?,
                    observed_at: row.try_get_by("runtime_observed_at")?,
                    freshness: row.try_get_by("runtime_freshness")?,
                },
            })
        })
        .transpose()
        .map_err(|error: sea_orm::DbErr| RepositoryError::Other(error.into()))
    }
}
