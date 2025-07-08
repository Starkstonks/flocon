use crate::models::inode::Inode;
use crate::schema::xattr;
use diesel::prelude::*;

#[derive(Identifiable, Queryable, Selectable, Associations, Debug, PartialEq)]
#[diesel(
    belongs_to(Inode),
    table_name = xattr,
    primary_key(inode_id, name)
)]
pub struct Xattr {
    pub inode_id: i32,
    pub name: String,
    pub value: Vec<u8>,
}

#[derive(Insertable, Debug)]
#[diesel(table_name = xattr)]
pub struct NewXattr<'a> {
    pub inode_id: i32,
    pub name: &'a str,
    pub value: &'a [u8],
}
