use crate::filesystem::blocks::{
    DataSource, MemoryDataSource, Sequence, SqliteDataSource, WorkingBlock, ZeroDataSource,
};
use crate::filesystem::sqlite::FileSystemManager;
use crate::filesystem::winter::{WinterFs, WinterInode, WinterTree};
use crate::filesystem::{EntryCore, WinterHandle};
use chrono::{DateTime, Utc};
use libc::{S_IFDIR, blksize_t, dev_t, gid_t, mode_t, off_t, size_t, uid_t};
use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{OptionalExtension, params};
use std::io::{Error, ErrorKind, Read, Write};
use std::path::Path;
use std::time::Duration;

/// Context passed to each FUSE operation
pub struct FloconContext {
    pub conn: PooledConnection<SqliteConnectionManager>,
}

/// A tree that borrows the context
pub struct FloconTree<'ctx> {
    ctx: &'ctx mut FloconContext,
}

impl<'ctx> WinterTree for FloconTree<'ctx> {
    type Inode = FloconInode;

    fn lookup(&mut self, parent: u64, name: &str) -> std::io::Result<u64> {
        match self.ctx.conn.query_row(
            r#"
            select l.child_id
            from link l
            where l.parent_id = ? and l.name = ?
            "#,
            params![parent, name],
            |row| row.get(0),
        ) {
            Ok(child_id) => Ok(child_id),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(Error::new(ErrorKind::Other, e)),
        }
    }

    fn get_inode(&mut self, inode: u64) -> std::io::Result<Self::Inode> {
        fn parse_datetime(s: String, col: usize) -> Result<DateTime<Utc>, rusqlite::Error> {
            DateTime::parse_from_rfc3339(&s)
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        col,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
                .map(|dt| dt.with_timezone(&Utc))
        }

        match self.ctx.conn.query_row(
            r#"
            select id, mode, uid, gid, size, rdev, atime, mtime, ctime
            from inode
            where id = ?
            "#,
            params![inode],
            |row| {
                Ok(FloconInode {
                    id: row.get(0)?,
                    mode: row.get(1)?,
                    uid: row.get(2)?,
                    gid: row.get(3)?,
                    size: row.get(4)?,
                    rdev: row.get(5)?,
                    atime: parse_datetime(row.get(6)?, 6)?,
                    mtime: parse_datetime(row.get(7)?, 7)?,
                    ctime: parse_datetime(row.get(8)?, 8)?,
                })
            },
        ) {
            Ok(inode) => Ok(inode),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                Err(Error::from_raw_os_error(libc::ENOENT))
            }
            Err(e) => Err(Error::new(ErrorKind::Other, e)),
        }
    }

    fn empty_inode(&mut self) -> Self::Inode {
        let epoch = DateTime::from_timestamp(0, 0).unwrap();

        FloconInode {
            id: 0,
            mode: 0,
            uid: 0,
            gid: 0,
            size: 0,
            rdev: 0,
            atime: epoch.into(),
            mtime: epoch.into(),
            ctime: epoch.into(),
        }
    }

    /// Finding children of a given directory which also are directories. This
    /// might sound weird, but that's what WinterFs needs in order to simulate
    /// UNIX-style link-counting on directories
    fn count_child_directories(&mut self, inode: u64) -> std::io::Result<u64> {
        match self.ctx.conn.query_row(
            r#"
            select count(*)
            from link l
            inner join inode i on l.child_id = i.id
            where l.parent_id = ? and (i.mode & ?) != 0
            "#,
            params![inode, S_IFDIR],
            |row| row.get::<_, u64>(0),
        ) {
            Ok(count) => Ok(count),
            Err(e) => Err(Error::new(ErrorKind::Other, e)),
        }
    }

    /// Finds all the potential parents of a given inode, meaning that it's all
    /// the directories in which you would find this inode
    fn find_parents_of(&mut self, inode: u64) -> std::io::Result<Vec<u64>> {
        let mut stmt = self
            .ctx
            .conn
            .prepare("select parent_id from link where child_id = ?")
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        let parent_ids = stmt
            .query_map(params![inode], |row| row.get::<_, u64>(0))
            .map_err(|e| Error::new(ErrorKind::Other, e))?
            .collect::<Result<Vec<u64>, _>>()
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        Ok(parent_ids)
    }

    fn create_inode(
        &mut self,
        mode: mode_t,
        uid_t: uid_t,
        gid_t: gid_t,
        rdev: dev_t,
    ) -> std::io::Result<Self::Inode> {
        use chrono::SecondsFormat;

        let now = Utc::now();
        let now_str = now.to_rfc3339_opts(SecondsFormat::Nanos, true);

        self.ctx
            .conn
            .execute(
                r#"
                insert into inode (mode, uid, gid, size, rdev, atime, mtime, ctime, btime)
                values (?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                params![
                    mode, uid_t, gid_t, 0, // size
                    rdev, &now_str, &now_str, &now_str, &now_str
                ],
            )
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        let inode_id = self.ctx.conn.last_insert_rowid() as u64;

        self.get_inode(inode_id)
    }

    fn add_child(&mut self, parent: u64, child: u64, name: &str) -> std::io::Result<()> {
        self.ctx
            .conn
            .execute(
                r#"
                insert into link (parent_id, child_id, name)
                values (?, ?, ?)
                "#,
                params![parent, child, name],
            )
            .map(|_| ())
            .map_err(|e| Error::new(ErrorKind::Other, e))
    }

    fn remove_child(&mut self, parent: u64, name: &str) -> std::io::Result<()> {
        self.ctx
            .conn
            .execute(
                r#"
                delete from link
                where parent_id = ? and name = ?
                "#,
                params![parent, name],
            )
            .map(|_| ())
            .map_err(|e| Error::new(ErrorKind::Other, e))
    }

    fn find_children_of(
        &mut self,
        parent: u64,
        size: size_t,
        offset: size_t,
    ) -> std::io::Result<Vec<(String, u64)>> {
        let mut stmt = self
            .ctx
            .conn
            .prepare(
                r#"
                select name, child_id
                from link
                where parent_id = ?
                order by name asc
                limit ? offset ?
                "#,
            )
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        let children = stmt
            .query_map(params![parent, size, offset], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })
            .map_err(|e| Error::new(ErrorKind::Other, e))?
            .collect::<Result<Vec<(String, u64)>, _>>()
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        Ok(children)
    }

    fn delete_inode(&mut self, inode: u64) -> std::io::Result<()> {
        // Check if there are any remaining links to this inode
        let link_count: u64 = self
            .ctx
            .conn
            .query_row(
                "select count(*) from link where child_id = ?",
                params![inode],
                |row| row.get(0),
            )
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        if link_count > 0 {
            return Err(Error::from_raw_os_error(libc::EBUSY));
        }

        // Delete all blocks associated with this inode
        self.ctx
            .conn
            .execute("delete from block where inode_id = ?", params![inode])
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        // Delete all xattrs associated with this inode
        self.ctx
            .conn
            .execute("delete from xattr where inode_id = ?", params![inode])
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        // Finally, delete the inode itself
        self.ctx
            .conn
            .execute("delete from inode where id = ?", params![inode])
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        Ok(())
    }
}

pub struct FloconInode {
    id: u64,
    mode: mode_t,
    uid: uid_t,
    gid: gid_t,
    size: off_t,
    rdev: dev_t,
    atime: DateTime<Utc>,
    mtime: DateTime<Utc>,
    ctime: DateTime<Utc>,
}

impl WinterInode for FloconInode {
    fn get_id(&self) -> u64 {
        self.id
    }

    fn get_mode(&self) -> mode_t {
        self.mode
    }

    fn make_entry(&self) -> std::io::Result<EntryCore> {
        let atime: DateTime<Utc> = self.atime;
        let mtime: DateTime<Utc> = self.mtime;
        let ctime: DateTime<Utc> = self.ctime;

        Ok(EntryCore {
            st_ino: self.id,
            st_size: self.size,
            st_atime: atime.timestamp(),
            st_atime_nsec: atime.timestamp_subsec_nanos().into(),
            st_mtime: mtime.timestamp(),
            st_mtime_nsec: mtime.timestamp_subsec_nanos().into(),
            st_ctime: ctime.timestamp(),
            st_ctime_nsec: ctime.timestamp_subsec_nanos().into(),
            st_mode: self.mode,
            st_uid: self.uid,
            st_gid: self.gid,
            st_rdev: self.rdev,
        })
    }

    /// We're not a network file system so we leave the cache in place for
    /// a significant amount of time (the kernel will expire the cache entries
    /// when it modifies things from its own point of view)
    fn attr_valid_time(&self) -> std::io::Result<Duration> {
        Ok(Duration::from_secs(3600 * 24 * 365))
    }

    /// We're not a network file system so we leave the cache in place for
    /// a significant amount of time (the kernel will expire the cache entries
    /// when it modifies things from its own point of view)
    fn entry_valid_time(&self) -> std::io::Result<Duration> {
        Ok(Duration::from_secs(3600 * 24 * 365))
    }
}

pub struct FloconHandle {
    inode: FloconInode,
    block_size: blksize_t,
    image_name: String,
}

impl FloconHandle {
    /// Update a numeric field in the inode and reload the full inode
    fn update_inode_numeric(
        &mut self,
        context: &mut FloconContext,
        field_name: &str,
        value: u64,
    ) -> std::io::Result<()> {
        // Validate field name to prevent SQL injection
        match field_name {
            "mode" | "uid" | "gid" | "size" | "rdev" => {}
            _ => return Err(Error::new(ErrorKind::InvalidInput, "Invalid field name")),
        }

        // Update the field
        let sql = format!("update inode set {} = ? where id = ?", field_name);
        context
            .conn
            .execute(&sql, params![value, self.inode.id])
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        // Fetch the updated inode
        let mut tree = FloconTree { ctx: context };
        self.inode = tree.get_inode(self.inode.id)?;

        Ok(())
    }

    /// Update a datetime field in the inode and reload the full inode
    fn update_inode_datetime(
        &mut self,
        context: &mut FloconContext,
        field_name: &str,
        value: DateTime<Utc>,
    ) -> std::io::Result<()> {
        use chrono::SecondsFormat;

        // Validate field name to prevent SQL injection
        match field_name {
            "atime" | "mtime" | "ctime" | "btime" => {}
            _ => return Err(Error::new(ErrorKind::InvalidInput, "Invalid field name")),
        }

        // Convert datetime to string with nanosecond precision
        let datetime_str = value.to_rfc3339_opts(SecondsFormat::Nanos, true);

        // Update the field
        let sql = format!("update inode set {} = ? where id = ?", field_name);
        context
            .conn
            .execute(&sql, params![datetime_str, self.inode.id])
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        // Fetch the updated inode
        let mut tree = FloconTree { ctx: context };
        self.inode = tree.get_inode(self.inode.id)?;

        Ok(())
    }
}

impl WinterHandle for FloconHandle {
    type Context = FloconContext;
    type Inode = FloconInode;

    fn get_inode(&self) -> &Self::Inode {
        &self.inode
    }

    fn flush(&mut self, _context: &mut Self::Context) -> std::io::Result<()> {
        Ok(())
    }

    /// We force all data to be written onto the disk (for all files at the
    /// same time but at least we're sure). Not sure if it's a good idea to be
    /// so aggressive but let's try like that for now.
    fn fsync_data(&mut self, context: &mut Self::Context) -> std::io::Result<()> {
        context
            .conn
            .execute_batch("pragma wal_checkpoint(truncate)")
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        Ok(())
    }

    /// Same as fsync_data but slightly less aggressive
    fn fsync_metadata(&mut self, context: &mut Self::Context) -> std::io::Result<()> {
        context
            .conn
            .execute_batch("pragma wal_checkpoint(passive)")
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        Ok(())
    }

    fn truncate(&mut self, context: &mut Self::Context) -> std::io::Result<()> {
        // Delete all blocks associated with this inode
        context
            .conn
            .execute(
                "delete from block where inode_id = ?",
                params![self.inode.id],
            )
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        // Update the inode's size to 0 in the database
        context
            .conn
            .execute(
                "update inode set size = 0 where id = ?",
                params![self.inode.id],
            )
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        // Update the local inode's size
        self.inode.size = 0;

        Ok(())
    }

    fn set_mode(&mut self, context: &mut Self::Context, mode: mode_t) -> std::io::Result<()> {
        self.update_inode_numeric(context, "mode", mode as u64)
    }

    fn set_uid(&mut self, context: &mut Self::Context, uid: uid_t) -> std::io::Result<()> {
        self.update_inode_numeric(context, "uid", uid as u64)
    }

    fn set_gid(&mut self, context: &mut Self::Context, gid: gid_t) -> std::io::Result<()> {
        self.update_inode_numeric(context, "gid", gid as u64)
    }

    /// Grows or shrinks the file to fit the size that we're asked to have.
    /// There are quite a few logic branches in there, but it should cover all
    /// the cases.
    fn set_size(&mut self, context: &mut Self::Context, size: off_t) -> std::io::Result<()> {
        let current_inode_id = self.inode.id;
        let current_size = self.inode.size as off_t;

        if size == current_size {
            return Ok(());
        }

        if size < current_size {
            // Truncating - remove or trim blocks beyond the new size
            let truncate_point = size as i32;

            // Delete blocks that are entirely beyond the new size
            context
                .conn
                .execute(
                    "delete from block where inode_id = ? and first_byte >= ?",
                    params![current_inode_id, truncate_point],
                )
                .map_err(|e| Error::new(ErrorKind::Other, e))?;

            // Find blocks that span the truncation point
            let mut stmt = context
                .conn
                .prepare(
                    r#"
                    select id, inode_id, first_byte, last_byte, data is not null as has_data
                    from block
                    where inode_id = ? and first_byte < ? and last_byte >= ?
                    "#,
                )
                .map_err(|e| Error::new(ErrorKind::Other, e))?;

            let spanning_blocks: Vec<WorkingBlock> = stmt
                .query_map(
                    params![current_inode_id, truncate_point, truncate_point],
                    |row| {
                        let id: u64 = row.get(0)?;
                        let first_byte: u64 = row.get(2)?;
                        let last_byte: u64 = row.get(3)?;
                        let size: u64 = last_byte - first_byte + 1;
                        let source: Box<dyn DataSource> = if row.get(4)? {
                            Box::new(SqliteDataSource::new(id, 0, size))
                        } else {
                            Box::new(ZeroDataSource::new(size))
                        };

                        Ok(WorkingBlock {
                            id: Some(id),
                            first_byte,
                            last_byte,
                            source,
                        })
                    },
                )
                .map_err(|e| Error::new(ErrorKind::Other, e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| Error::new(ErrorKind::Other, e))?;

            drop(stmt);

            // Trim blocks that span the truncation point
            for spanning_block in spanning_blocks {
                if let Ok(trimmed) =
                    spanning_block.clip(spanning_block.first_byte, (truncate_point - 1) as u64)
                {
                    let concrete_data = trimmed.concrete_data(context);

                    // Delete the old block
                    context
                        .conn
                        .execute(
                            "delete from block where id = ?",
                            params![spanning_block.id.unwrap()],
                        )
                        .map_err(|e| Error::new(ErrorKind::Other, e))?;

                    context
                        .conn
                        .execute(
                            r#"
                            insert into block (inode_id, first_byte, last_byte, data)
                            values (?, ?, ?, ?)
                            "#,
                            params![
                                current_inode_id,
                                trimmed.first_byte,
                                trimmed.last_byte,
                                &concrete_data
                            ],
                        )
                        .map_err(|e| Error::new(ErrorKind::Other, e))?;
                }
            }
        } else {
            // Growing - create a zero-filled block if needed
            if current_size > 0 {
                // Find the last block to see if we need to fill a gap
                let last_block: Option<(i32, i32)> = context
                    .conn
                    .query_row(
                        r#"
                        select first_byte, last_byte
                        from block
                        where inode_id = ?
                        order by last_byte desc
                        limit 1
                        "#,
                        params![current_inode_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(|e| Error::new(ErrorKind::Other, e))?;

                if let Some((_first, last)) = last_block {
                    let gap_start = last + 1;
                    let gap_end = (size - 1) as i32;

                    if gap_start <= gap_end {
                        // Create a zero-filled block for the gap
                        context
                            .conn
                            .execute(
                                r#"
                                insert into block (inode_id, first_byte, last_byte, data)
                                values (?, ?, ?, ?)
                                "#,
                                params![current_inode_id, gap_start, gap_end, None::<Vec<u8>>],
                            )
                            .map_err(|e| Error::new(ErrorKind::Other, e))?;
                    }
                } else if size > 0 {
                    // No blocks exist, create one zero-filled block
                    context
                        .conn
                        .execute(
                            r#"
                            insert into block (inode_id, first_byte, last_byte, data)
                            values (?, ?, ?, ?)
                            "#,
                            params![current_inode_id, 0, (size - 1) as i32, None::<Vec<u8>>],
                        )
                        .map_err(|e| Error::new(ErrorKind::Other, e))?;
                }
            }
        }

        // Update the inode size using the helper method
        self.update_inode_numeric(context, "size", size as u64)?;

        Ok(())
    }

    fn set_atime(
        &mut self,
        context: &mut Self::Context,
        atime: DateTime<Utc>,
    ) -> std::io::Result<()> {
        self.update_inode_datetime(context, "atime", atime)
    }

    fn set_mtime(
        &mut self,
        context: &mut Self::Context,
        mtime: DateTime<Utc>,
    ) -> std::io::Result<()> {
        self.update_inode_datetime(context, "mtime", mtime)
    }

    fn set_ctime(
        &mut self,
        context: &mut Self::Context,
        ctime: DateTime<Utc>,
    ) -> std::io::Result<()> {
        self.update_inode_datetime(context, "ctime", ctime)
    }

    /// We're going through all the blocks "touched" by the read operation and
    /// then throwing them as efficiently as possible into the provided writer
    /// given to us by FUSE.
    fn read(
        &mut self,
        context: &mut Self::Context,
        size: size_t,
        offset: off_t,
        w: &mut dyn Write,
    ) -> std::io::Result<size_t> {
        let current_inode_id = self.inode.get_id();
        let file_size = self.inode.size;

        if offset >= file_size as off_t {
            return Ok(0);
        }

        let read_start = offset as size_t;
        let read_end = (offset + size as off_t - 1).min(file_size as off_t - 1) as size_t;

        if read_start > read_end {
            return Ok(0);
        }

        let working_blocks = {
            let mut stmt = context
                .conn
                .prepare(
                    r#"
                    select id, first_byte, last_byte, data is not null as has_data
                    from block
                    where inode_id = ? and last_byte >= ? and first_byte <= ?
                    order by first_byte asc
                    "#,
                )
                .map_err(|e| Error::new(ErrorKind::Other, e))?;

            stmt.query_map(params![current_inode_id, read_start, read_end], |row| {
                let id: u64 = row.get(0)?;
                let first_byte: u64 = row.get(1)?;
                let last_byte: u64 = row.get(2)?;
                let size: u64 = last_byte - first_byte + 1;
                let source: Box<dyn DataSource> = if row.get(3)? {
                    Box::new(SqliteDataSource::new(id, 0, size))
                } else {
                    Box::new(ZeroDataSource::new(size))
                };

                Ok(WorkingBlock {
                    id: Some(id),
                    first_byte,
                    last_byte,
                    source,
                })
            })
            .map_err(|e| Error::new(ErrorKind::Other, e))?
            .collect::<Result<Vec<WorkingBlock>, _>>()
            .map_err(|e| Error::new(ErrorKind::Other, e))?
        };

        let sequence = Sequence::new(working_blocks).clip(read_start as u64, read_end as u64);
        sequence.concrete_data_to_writer(context, w)
    }

    /// We're writing the data following that logic:
    ///
    /// 1. The input data is chunked into blocks of the size that we like
    /// 2. Then we fetch existing blocks that overlap with those would-be
    ///    blocks
    /// 3. Using a blocks Sequence, we insert the new blocks
    /// 4. This allows us to know which blocks are left untouched (those who
    ///    kept their ID), which ones are to be inserted (if they do not have
    ///    an ID) and which ones need to be deleted (those not in the output
    ///    anymore)
    /// 5. Finally we wrap up and return the written size, without forgetting
    ///    to update the pre-computed size on the inode as well
    fn write(
        &mut self,
        context: &mut Self::Context,
        size: size_t,
        offset: off_t,
        r: &mut dyn Read,
    ) -> std::io::Result<size_t> {
        let current_inode_id = self.inode.get_id();
        let write_start = offset;
        let write_end = offset + size as off_t - 1;

        let mut incoming_data = vec![0u8; size];
        let bytes_read = r.read(&mut incoming_data)?;
        if bytes_read != size {
            return Err(Error::new(ErrorKind::Other, "Unexpected EOF"));
        }

        let mut stmt = context
            .conn
            .prepare(
                r#"
                select id, first_byte, last_byte, data is not null as has_data
                from block
                where inode_id = ? and last_byte >= ? and first_byte <= ?
                order by first_byte asc
                "#,
            )
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        let working_blocks: Vec<WorkingBlock> = stmt
            .query_map(params![current_inode_id, write_start, write_end], |row| {
                let id: u64 = row.get(0)?;
                let first_byte: u64 = row.get(1)?;
                let last_byte: u64 = row.get(2)?;
                let size: u64 = last_byte - first_byte + 1;
                let source: Box<dyn DataSource> = if row.get(3)? {
                    Box::new(SqliteDataSource::new(id, 0, size))
                } else {
                    Box::new(ZeroDataSource::new(size))
                };

                Ok(WorkingBlock {
                    id: Some(id),
                    first_byte,
                    last_byte,
                    source,
                })
            })
            .map_err(|e| Error::new(ErrorKind::Other, e))?
            .collect::<Result<Vec<WorkingBlock>, _>>()
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        let original_block_ids: std::collections::HashSet<u64> =
            working_blocks.iter().filter_map(|b| b.id).collect();

        drop(stmt);

        let mut sequence = Sequence::new(working_blocks);

        let mut current_offset = write_start;
        for chunk in incoming_data.chunks(self.block_size as usize) {
            let chunk_end = current_offset as u64 + chunk.len() as u64 - 1;
            let new_block = WorkingBlock::new(
                None,
                current_offset as u64,
                chunk_end,
                Box::new(MemoryDataSource::from_data(chunk.to_vec())?),
            );
            sequence = sequence.replace(new_block);
            current_offset = (chunk_end + 1) as off_t;
        }

        let new_blocks_params: Vec<_> = sequence
            .blocks
            .iter()
            .filter_map(|blk| {
                if blk.id.is_none() {
                    Some((
                        current_inode_id,
                        blk.first_byte,
                        blk.last_byte,
                        blk.concrete_data(context),
                    ))
                } else {
                    None
                }
            })
            .collect();

        for (inode_id, first_byte, last_byte, data) in new_blocks_params {
            context
                .conn
                .execute(
                    r#"
                    insert into block (inode_id, first_byte, last_byte, data)
                    values (?, ?, ?, ?)
                    "#,
                    params![inode_id, first_byte, last_byte, data],
                )
                .map_err(|e| Error::new(ErrorKind::Other, e))?;
        }

        let remaining_block_ids: std::collections::HashSet<u64> =
            sequence.blocks.iter().filter_map(|b| b.id).collect();

        let deleted_ids: Vec<u64> = original_block_ids
            .difference(&remaining_block_ids)
            .cloned()
            .collect();

        if !deleted_ids.is_empty() {
            let placeholders = deleted_ids
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(", ");

            let sql = format!("delete from block where id in ({})", placeholders);

            let params: Vec<_> = deleted_ids.iter().map(|&id| id as i64).collect();

            context
                .conn
                .execute(&sql, rusqlite::params_from_iter(params))
                .map_err(|e| Error::new(ErrorKind::Other, e))?;
        }

        let new_size = (write_end + 1).max(self.inode.size as i64);
        if new_size != self.inode.size as i64 {
            context
                .conn
                .execute(
                    "update inode set size = ? where id = ?",
                    params![new_size, current_inode_id],
                )
                .map_err(|e| Error::new(ErrorKind::Other, e))?;

            self.inode.size = new_size;
        }

        Ok(bytes_read)
    }

    /// Mostly bypassing the block logic in order to have an efficient append
    /// (we're probably writing logs or something like that, so it's more
    /// important to atomically insert the lines we're given than doing block
    /// computation).
    fn append(
        &mut self,
        context: &mut Self::Context,
        size: size_t,
        r: &mut dyn Read,
    ) -> std::io::Result<size_t> {
        let current_inode_id = self.inode.id;

        let mut incoming_data = vec![0u8; size];
        let bytes_read = r.read(&mut incoming_data)?;
        if bytes_read == 0 {
            return Ok(0);
        }

        incoming_data.truncate(bytes_read);

        // Find the last byte position of existing blocks
        let append_start: u64 = context
            .conn
            .query_row(
                "select max(last_byte) from block where inode_id = ?",
                params![current_inode_id],
                |row| row.get::<_, Option<u64>>(0),
            )
            .map_err(|e| Error::new(ErrorKind::Other, e))?
            .map(|max_byte| max_byte + 1)
            .unwrap_or(0);

        // Create working blocks from incoming data
        let mut working_blocks = Vec::new();
        let mut current_offset = append_start;

        for chunk in incoming_data.chunks(self.block_size as usize) {
            let chunk_end = current_offset + chunk.len() as u64 - 1;
            working_blocks.push(WorkingBlock {
                id: None,
                first_byte: current_offset as u64,
                last_byte: chunk_end as u64,
                source: Box::new(
                    MemoryDataSource::from_data(chunk.to_vec())
                        .expect("Failed to create MemoryDataSource"),
                ),
            });
            current_offset = chunk_end + 1;
        }

        // Insert all new blocks
        if !working_blocks.is_empty() {
            let params_list: Vec<_> = working_blocks
                .iter()
                .map(|wb| {
                    (
                        current_inode_id,
                        wb.first_byte,
                        wb.last_byte,
                        wb.concrete_data(context),
                    )
                })
                .collect();

            let mut stmt = context
                .conn
                .prepare(
                    r#"
                    insert into block (inode_id, first_byte, last_byte, data)
                    values (?, ?, ?, ?)
                    "#,
                )
                .map_err(|e| Error::new(ErrorKind::Other, e))?;

            for (inode_id, first_byte, last_byte, data) in params_list {
                stmt.execute(params![inode_id, first_byte, last_byte, data])
                    .map_err(|e| Error::new(ErrorKind::Other, e))?;
            }
        }

        // Update inode size
        let new_size = append_start + bytes_read as u64;
        context
            .conn
            .execute(
                "update inode set size = ? where id = ?",
                params![new_size, current_inode_id],
            )
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        // Update the local inode's size
        self.inode.size = new_size as off_t;

        Ok(bytes_read)
    }

    fn xattr_set(
        &mut self,
        context: &mut Self::Context,
        key: &str,
        value: &[u8],
    ) -> std::io::Result<()> {
        // Check if it's trying to set a flocon.* attribute
        if key.starts_with("flocon.") {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "Cannot set flocon.* attributes",
            ));
        }

        // Try to update existing xattr
        let updated = context
            .conn
            .execute(
                "update xattr set value = ? where inode_id = ? and name = ?",
                params![value, self.inode.id, key],
            )
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        // If no rows were updated, insert new xattr
        if updated == 0 {
            context
                .conn
                .execute(
                    "insert into xattr (inode_id, name, value) values (?, ?, ?)",
                    params![self.inode.id, key, value],
                )
                .map_err(|e| Error::new(ErrorKind::Other, e))?;
        }

        Ok(())
    }

    fn xattr_get(&mut self, context: &mut Self::Context, key: &str) -> std::io::Result<Vec<u8>> {
        let namespace = key.split('.').next().unwrap_or("");

        match namespace {
            "flocon" => match key {
                "flocon.image" => Ok(self.image_name.as_bytes().to_vec()),
                _ => Err(Error::new(ErrorKind::NotFound, "No data available")),
            },
            _ => {
                match context.conn.query_row(
                    "select value from xattr where inode_id = ? and name = ?",
                    params![self.inode.id, key],
                    |row| row.get::<_, Vec<u8>>(0),
                ) {
                    Ok(value) => Ok(value),
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        Err(Error::new(ErrorKind::NotFound, "No data available"))
                    }
                    Err(e) => Err(Error::new(ErrorKind::Other, e)),
                }
            }
        }
    }

    fn xattr_remove(&mut self, context: &mut Self::Context, key: &str) -> std::io::Result<()> {
        if key.starts_with("flocon.") {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "Cannot remove flocon.* attributes",
            ));
        }

        let deleted = context
            .conn
            .execute(
                "delete from xattr where inode_id = ? and name = ?",
                params![self.inode.id, key],
            )
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        if deleted == 0 {
            return Err(Error::new(ErrorKind::NotFound, "No data available"));
        }

        Ok(())
    }

    fn xattr_list(&mut self, context: &mut Self::Context) -> std::io::Result<Vec<String>> {
        let mut stmt = context
            .conn
            .prepare("select name from xattr where inode_id = ?")
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        let mut names = stmt
            .query_map(params![self.inode.id], |row| row.get::<_, String>(0))
            .map_err(|e| Error::new(ErrorKind::Other, e))?
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        names.push("flocon.image".to_string());

        Ok(names)
    }
}

pub struct Flocon {
    fs_manager: FileSystemManager,
    image_path: std::path::PathBuf,
}

impl Flocon {
    pub fn new(fs_manager: FileSystemManager, image_path: &Path) -> Self {
        Flocon {
            fs_manager,
            image_path: image_path.to_path_buf(),
        }
    }
}

impl WinterFs for Flocon {
    type Context = FloconContext;
    type Handle = FloconHandle;
    type Inode = FloconInode;

    type Tree<'ctx>
        = FloconTree<'ctx>
    where
        Self: 'ctx;

    /// For now let's hard-code this. According to my statistics, most files
    /// are less than 1 Mio (and then it jumps through the roof) so we'll use
    /// this as a block size to limit having to deal with multi-block stuff for
    /// most files.
    fn block_size(&self) -> Result<blksize_t, Error> {
        Ok(1024 * 1024)
    }

    /// Used storage will be the DB file size
    fn estimate_used_storage(&self) -> Result<u64, Error> {
        let metadata = std::fs::metadata(&self.image_path)?;
        Ok(metadata.len())
    }

    /// Free storage is how much storage we got left on the supporting media,
    /// which is affected by many other factors but gives you a decent idea
    /// of how much storage you can still use before things blow up I guess
    fn estimate_free_storage(&self) -> Result<u64, Error> {
        let parent = self
            .image_path
            .parent()
            .ok_or_else(|| Error::new(ErrorKind::Other, "Image has no parent directory"))?;

        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let path_cstr = std::ffi::CString::new(parent.to_string_lossy().as_bytes())?;

        let result = unsafe { libc::statvfs(path_cstr.as_ptr(), &mut stat) };
        if result != 0 {
            return Err(Error::last_os_error());
        }

        Ok(stat.f_bavail * stat.f_frsize)
    }

    /// For now we'll live-count the number of inodes. That's not the most
    /// efficient way to go, so this will have to be optimized in the future.
    fn estimate_files_count(&self, context: &mut Self::Context) -> Result<u64, Error> {
        let count = context
            .conn
            .query_row("select count(*) from inode", [], |row| row.get::<_, u64>(0))
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        Ok(count)
    }

    fn create_context(&self) -> Result<Self::Context, Error> {
        let conn = self
            .fs_manager
            .get_connection()
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        conn.execute("begin", [])
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        Ok(FloconContext { conn })
    }

    fn sync_context(&self, ctx: &mut Self::Context) -> Result<(), Error> {
        ctx.conn
            .execute("commit", [])
            .map_err(|e| Error::new(ErrorKind::Other, e))?;
        Ok(())
    }

    fn rollback_context(&self, ctx: &mut Self::Context) -> Result<(), Error> {
        ctx.conn
            .execute("rollback", [])
            .map_err(|e| Error::new(ErrorKind::Other, e))?;
        Ok(())
    }

    fn close_context(&self, ctx: &mut Self::Context) -> Result<(), Error> {
        let _ = self.rollback_context(ctx);
        Ok(())
    }

    fn tree<'ctx>(&self, context: &'ctx mut Self::Context) -> Result<Self::Tree<'ctx>, Error> {
        Ok(FloconTree { ctx: context })
    }

    fn open(
        &self,
        ctx: &mut Self::Context,
        inode: u64,
        _flags: u32,
    ) -> Result<Self::Handle, Error> {
        let mut tree = self.tree(ctx)?;

        let image_name = self
            .image_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".to_string());

        Ok(FloconHandle {
            inode: tree.get_inode(inode)?,
            block_size: self.block_size()?,
            image_name,
        })
    }
}
