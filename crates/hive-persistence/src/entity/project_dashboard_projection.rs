//! `project_dashboard_projection` (`V004`, a view, not a table). Hand-written: `sea-orm-cli
//! generate entity` does not discover views (`GSR-ENTITY-ALL`'s "76 tables + 1 view" baseline was
//! itself short by 3: this schema has four views, not one — `project_dashboard_projection`,
//! `agent_operational_view_projection`, `effective_evaluation_capabilities`, and
//! `audit_event_projection`). No column here has a real uniqueness guarantee at the SQL level;
//! `project_id` is chosen as the nominal primary key because the view's own definition selects
//! one row per project.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "project_dashboard_projection")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub project_id: Uuid,
    pub organization_id: Uuid,
    #[sea_orm(column_type = "Text")]
    pub slug: String,
    #[sea_orm(column_type = "Text")]
    pub display_name: String,
    #[sea_orm(column_type = "Text")]
    pub lifecycle_status: String,
    pub active_agents: i32,
    pub active_deployments: i32,
    pub failed_deployments: i32,
    pub pending_approvals: i32,
    pub unhealthy_resources: i32,
    #[sea_orm(column_type = "Text")]
    pub cost_availability: String,
    pub current_period_cost_cents: Option<i32>,
    pub cost_period_start: Option<DateTimeWithTimeZone>,
    pub cost_period_end: Option<DateTimeWithTimeZone>,
    #[sea_orm(column_type = "Text", nullable)]
    pub cost_currency: Option<String>,
    pub cost_data_as_of: Option<DateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Projects,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelatedEntity)]
pub enum RelatedEntity {
    #[sea_orm(entity = "super::projects::Entity")]
    Projects,
}
