//! `audit_event_projection` (`V040`, a view; the one the design document's baseline already
//! expected — see `project_dashboard_projection`'s doc comment for the other three it missed).
//! `projection_id` is the view's own text identifier column. It is declared last and
//! `occurred_at` before it, because Seaography applies `orderBy` in declaration order: newest
//! first, then the key as the tie-break.
//!
//! `source_ip` and `user_agent` are not generated fields, filters or orderings: a read exposes
//! the computed `sourceIp` / `userAgent` (`crate::audit`), which answer `null` unless the
//! requesting principal holds `AUDIT_SENSITIVE.VIEW` at the row's scope. `required_capability`
//! is what the tenant rule reads (`crate::authority`); it was never part of the API.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "audit_event_projection")]
pub struct Model {
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
    #[seaography(ignore)]
    #[sea_orm(column_type = "Text", nullable)]
    pub source_ip: Option<String>,
    #[seaography(ignore)]
    #[sea_orm(column_type = "Text", nullable)]
    pub user_agent: Option<String>,
    #[seaography(ignore)]
    #[sea_orm(column_type = "Text", nullable)]
    pub required_capability: Option<String>,
    pub occurred_at: DateTimeWithTimeZone,
    #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
    pub projection_id: String,
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
pub enum RelatedEntity {
    #[sea_orm(entity = "super::organizations::Entity")]
    Organizations,
    #[sea_orm(entity = "super::projects::Entity")]
    Projects,
    #[sea_orm(entity = "super::principals::Entity")]
    Principals,
}
