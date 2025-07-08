use crate::models::inode::Inode;
use crate::schema::block;
use diesel::prelude::*;

#[derive(Queryable, Selectable, Associations, Debug, PartialEq)]
#[diesel(belongs_to(Inode), table_name = block)]
pub struct Block {
    pub id: i32,
    pub inode_id: i32,
    pub first_byte: i32,
    pub last_byte: i32,
    pub data: Option<Vec<u8>>,
}

#[derive(Insertable, Debug)]
#[diesel(table_name = block)]
pub struct NewBlock<'a> {
    pub inode_id: i32,
    pub first_byte: i32,
    pub last_byte: i32,
    pub data: Option<&'a [u8]>,
}
