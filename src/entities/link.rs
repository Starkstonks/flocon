use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "link")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub parent_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub name: String,
    pub child_id: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::inode::Entity",
        from = "Column::ParentId",
        to = "super::inode::Column::Id"
    )]
    Parent,

    #[sea_orm(
        belongs_to = "super::inode::Entity",
        from = "Column::ChildId",
        to = "super::inode::Column::Id"
    )]
    Child,
}

impl ActiveModelBehavior for ActiveModel {}
