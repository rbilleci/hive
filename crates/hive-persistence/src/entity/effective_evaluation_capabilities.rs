//! `effective_evaluation_capabilities` (a view). See `project_dashboard_projection`'s doc comment
//! for why this is hand-written. No single column is unique; the view emits one row per
//! `(principal_id, project_id, code)` triple, so all three together are the nominal composite
//! primary key.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "effective_evaluation_capabilities")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub principal_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub project_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
    pub code: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::principals::Entity",
        from = "Column::PrincipalId",
        to = "super::principals::Column::Id"
    )]
    Principals,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Projects,
}

impl Related<super::principals::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Principals.def()
    }
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelatedEntity)]
pub enum RelatedEntity {}
