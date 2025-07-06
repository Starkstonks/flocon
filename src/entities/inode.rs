use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "inode")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub mode: i32,
    pub uid: i32,
    pub gid: i32,
    pub size: i32,
    pub atime: DateTime<Utc>,
    pub mtime: DateTime<Utc>,
    pub ctime: DateTime<Utc>,
    pub btime: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    // Empty enum for now
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        panic!("No relations implemented")
    }
}

impl ActiveModelBehavior for ActiveModel {}
