use crate::schema::inode;
use chrono::{DateTime, Utc};
use diesel::deserialize::{FromSql, Result as DeserializeResult};
use diesel::prelude::*;
use diesel::serialize::{IsNull, Output, ToSql};
use diesel::sql_types::Text;
use diesel::sqlite::Sqlite;
use diesel::{AsExpression, FromSqlRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq, AsExpression, FromSqlRow)]
#[diesel(sql_type = Text)]
pub struct DateTimeUtc(pub DateTime<Utc>);

impl ToSql<Text, Sqlite> for DateTimeUtc {
    fn to_sql(&self, out: &mut Output<'_, '_, Sqlite>) -> diesel::serialize::Result {
        let s = self.0.to_rfc3339();
        out.set_value(s);
        Ok(IsNull::No)
    }
}

impl FromSql<Text, Sqlite> for DateTimeUtc {
    fn from_sql(bytes: diesel::backend::RawValue<'_, Sqlite>) -> DeserializeResult<Self> {
        let s = <String as FromSql<Text, Sqlite>>::from_sql(bytes)?;
        let dt = DateTime::parse_from_rfc3339(&s)
            .map_err(|e| format!("Invalid datetime: {}", e))?
            .with_timezone(&Utc);
        Ok(DateTimeUtc(dt))
    }
}

// Model struct
#[derive(Queryable, Selectable, Debug, Clone)]
#[diesel(table_name = inode)]
pub struct Inode {
    pub id: i32,
    pub mode: i32,
    pub uid: i32,
    pub gid: i32,
    pub size: i32,
    pub rdev: i32,
    pub atime: DateTimeUtc,
    pub mtime: DateTimeUtc,
    pub ctime: DateTimeUtc,
    pub btime: DateTimeUtc,
}

// Insertable struct
#[derive(Insertable)]
#[diesel(table_name = inode)]
pub struct NewInode {
    pub mode: i32,
    pub uid: i32,
    pub gid: i32,
    pub size: i32,
    pub rdev: i32,
    pub atime: DateTimeUtc,
    pub mtime: DateTimeUtc,
    pub ctime: DateTimeUtc,
    pub btime: DateTimeUtc,
}

// For convenience, implement From conversions
impl From<DateTime<Utc>> for DateTimeUtc {
    fn from(dt: DateTime<Utc>) -> Self {
        DateTimeUtc(dt)
    }
}

impl From<DateTimeUtc> for DateTime<Utc> {
    fn from(dt: DateTimeUtc) -> Self {
        dt.0
    }
}
