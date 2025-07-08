use crate::models::inode::Inode;
use crate::schema::link;
use diesel::prelude::*;

#[derive(Identifiable, Queryable, Selectable, Associations, Debug, PartialEq)]
#[diesel(
    belongs_to(Inode, foreign_key = parent_id),
    table_name = link,
    primary_key(parent_id, name)
)]
pub struct Link {
    pub parent_id: i32,
    pub child_id: i32,
    pub name: String,
}

#[derive(Insertable, Debug)]
#[diesel(table_name = link)]
pub struct NewLink {
    pub parent_id: i32,
    pub child_id: i32,
    pub name: String,
}
