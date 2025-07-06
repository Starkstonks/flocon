use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "block")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub inode_id: i32,
    pub first_byte: i32,
    pub last_byte: i32,
    pub data: Option<Vec<u8>>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::inode::Entity",
        from = "Column::InodeId",
        to = "super::inode::Column::Id"
    )]
    Inode,
}

impl ActiveModelBehavior for ActiveModel {}
