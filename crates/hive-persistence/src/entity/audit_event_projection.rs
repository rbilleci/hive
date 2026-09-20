//! `audit_event_projection` (`V040`, a view; the one the design document's baseline already
//! expected — see `project_dashboard_projection`'s doc comment for the other three it missed).
//! `projection_id` is the view's own text identifier column.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "audit_event_projection")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
    pub projection_id: String,
    #[sea_orm(column_type = "Text")]
    pub source_kind: String,
    pub source_event_id: Option<Uuid>,
    pub organization_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub actor_principal_id: Option<Uuid>,
    #[sea_orm(column_type = "Text")]
    pub action: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub resource_type: Option<String>,
    pub resource_id: Option<Uuid>,
    #[sea_orm(column_type = "Text")]
    pub outcome: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub before_digest: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub after_digest: Option<String>,
    pub safe_changed_fields: Json,
    pub resource_references: Json,
    pub request_id: Option<Uuid>,
    pub correlation_id: Option<Uuid>,
    #[sea_orm(column_type = "Text", nullable)]
    pub graphql_operation: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub source_ip: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub user_agent: Option<String>,
    pub occurred_at: DateTimeWithTimeZone,
    #[sea_orm(column_type = "Text", nullable)]
    pub required_capability: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::organizations::Entity",
        from = "Column::OrganizationId",
        to = "super::organizations::Column::Id"
    )]
    Organizations,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Projects,
    #[sea_orm(
        belongs_to = "super::principals::Entity",
        from = "Column::ActorPrincipalId",
        to = "super::principals::Column::Id"
    )]
    Principals,
}

impl Related<super::organizations::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Organizations.def()
    }
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl Related<super::principals::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Principals.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelatedEntity)]
pub enum RelatedEntity {}
