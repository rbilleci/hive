//! `agent_operational_view_projection` (`V006`, a view). See `project_dashboard_projection`'s doc
//! comment for why this is hand-written rather than `sea-orm-cli`-generated. `agent_id` is the
//! nominal primary key: the view selects one row per agent.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "agent_operational_view_projection")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub agent_id: Uuid,
    pub project_id: Uuid,
    pub organization_id: Uuid,
    #[sea_orm(column_type = "Text")]
    pub slug: String,
    #[sea_orm(column_type = "Text")]
    pub display_name: String,
    #[sea_orm(column_type = "Text")]
    pub lifecycle_status: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub draft_validation_status: Option<String>,
    pub draft_error_count: Option<i32>,
    pub draft_warning_count: Option<i32>,
    pub draft_validated_at: Option<DateTimeWithTimeZone>,
    #[sea_orm(column_type = "Text", nullable)]
    pub published_version_status: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub published_version: Option<String>,
    pub published_at: Option<DateTimeWithTimeZone>,
    pub alias_target_count: Option<i32>,
    pub active_alias_target_count: Option<i32>,
    #[sea_orm(column_type = "Text", nullable)]
    pub deployment_status: Option<String>,
    pub deployment_observed_at: Option<DateTimeWithTimeZone>,
    #[sea_orm(column_type = "Text", nullable)]
    pub evaluation_outcome: Option<String>,
    pub evaluation_completed_at: Option<DateTimeWithTimeZone>,
    #[sea_orm(column_type = "Text", nullable)]
    pub runtime_health: Option<String>,
    pub runtime_observed_at: Option<DateTimeWithTimeZone>,
    #[sea_orm(column_type = "Text", nullable)]
    pub runtime_freshness: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelatedEntity)]
pub enum RelatedEntity {}
