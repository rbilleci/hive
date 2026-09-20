//! Ports `PostgresAgentOperationalViewRepository`: reads only compact
//! summaries after a single query verifies active membership and route
//! ownership against `agent_operational_view_projection`.

use hive_application::agent::{
    AgentOperationalView, AgentOperationalViewRepository,
    AgentOperationalViewRepositoryError as RepositoryError, AliasTargetsSummary, DeploymentSummary,
    DraftValidationSummary, EvaluationSummary, PublishedVersionSummary, RuntimeHealthSummary,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub struct PgAgentOperationalViewRepository {
    pool: PgPool,
}

impl PgAgentOperationalViewRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
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
        let row = sqlx::query(LOOKUP)
            .bind(project_id)
            .bind(agent_id)
            .bind(principal_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| RepositoryError::Other(error.into()))?;
        Ok(row.map(|row| AgentOperationalView {
            id: row.get("agent_id"),
            slug: row.get("slug"),
            display_name: row.get("display_name"),
            lifecycle_status: row.get("lifecycle_status"),
            draft_validation: DraftValidationSummary {
                status: row.get("draft_validation_status"),
                error_count: row.get("draft_error_count"),
                warning_count: row.get("draft_warning_count"),
                validated_at: row.get("draft_validated_at"),
            },
            latest_published_version: PublishedVersionSummary {
                status: row.get("published_version_status"),
                version: row.get("published_version"),
                published_at: row.get("published_at"),
            },
            alias_targets: AliasTargetsSummary {
                total_count: row.get("alias_target_count"),
                active_count: row.get("active_alias_target_count"),
            },
            active_deployment: DeploymentSummary {
                status: row.get("deployment_status"),
                observed_at: row.get("deployment_observed_at"),
            },
            recent_evaluation: EvaluationSummary {
                outcome: row.get("evaluation_outcome"),
                completed_at: row.get("evaluation_completed_at"),
            },
            runtime_health: RuntimeHealthSummary {
                status: row.get("runtime_health"),
                observed_at: row.get("runtime_observed_at"),
                freshness: row.get("runtime_freshness"),
            },
        }))
    }
}
