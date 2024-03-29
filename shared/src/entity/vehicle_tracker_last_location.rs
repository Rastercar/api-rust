use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "vehicle_tracker_last_location")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false, unique)]
    pub vehicle_tracker_id: i32,
    pub time: DateTime<Utc>,
    #[sea_orm(column_type = "custom(\"geometry\")")]
    pub point: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::vehicle_tracker::Entity",
        from = "Column::VehicleTrackerId",
        to = "super::vehicle_tracker::Column::Id",
        on_update = "Cascade",
        on_delete = "NoAction"
    )]
    VehicleTracker,
}

impl Related<super::vehicle_tracker::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::VehicleTracker.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
