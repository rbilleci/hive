//! Closed-set text columns as SeaORM active enums, so the ORM types them and Seaography exposes
//! them as GraphQL enums with enum filters. Values match each column's `CHECK (... IN (...))`.

use sea_orm::entity::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text", enum_name = "lifecycle_status")]
pub enum LifecycleStatus {
    #[sea_orm(string_value = "ACTIVE")]
    Active,
    #[sea_orm(string_value = "ARCHIVED")]
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "Text",
    enum_name = "agent_lifecycle_status"
)]
pub enum AgentLifecycleStatus {
    #[sea_orm(string_value = "ACTIVE")]
    Active,
    #[sea_orm(string_value = "DEPRECATED")]
    Deprecated,
    #[sea_orm(string_value = "ARCHIVED")]
    Archived,
}
