use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "xattr")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub inode_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub name: String,
    pub value: Vec<u8>,
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
