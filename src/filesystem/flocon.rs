use crate::filesystem::blocks::{Sequence, WorkingBlock};
use crate::filesystem::sqlite::FileSystemManager;
use crate::filesystem::winter::{WinterFs, WinterInode, WinterTree};
use crate::filesystem::{EntryCore, WinterHandle};
use crate::models::inode::DateTimeUtc;
use crate::models::{Block as ModelBlock, Inode as ModelInode, NewInode, NewLink};
use crate::schema::inode::dsl::{id as inode_id, inode as inode_table};
use crate::schema::link::dsl::{child_id, link, name as link_name, parent_id};
use chrono::{DateTime, Utc};
use diesel::connection::SimpleConnection;
use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, PooledConnection};
use diesel::sql_types::Integer;
use fuse_backend_rs::api::filesystem::{ZeroCopyReader, ZeroCopyWriter};
use libc::{S_IFDIR, blksize_t, gid_t, mode_t, off_t, size_t, uid_t};
use std::io::{Error, ErrorKind};
use std::time::Duration;

/// Context passed to each FUSE operation
pub struct FloconContext {
    pub conn: PooledConnection<ConnectionManager<SqliteConnection>>,
}

/// A tree that borrows the context
pub struct FloconTree<'ctx> {
    ctx: &'ctx mut FloconContext,
}

impl<'ctx> WinterTree for FloconTree<'ctx> {
    type Inode = FloconInode;

    fn lookup(&mut self, parent: u64, name: &str) -> std::io::Result<u64> {
        link.filter(parent_id.eq(parent as i32))
            .filter(link_name.eq(name))
            .select(child_id)
            .first::<i32>(&mut self.ctx.conn)
            .optional()
            .map(|opt_id| opt_id.map_or(0, |id| id as u64))
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e))
    }

    fn get_inode(&mut self, inode: u64) -> std::io::Result<Self::Inode> {
        inode_table
            .find(inode as i32)
            .first::<ModelInode>(&mut self.ctx.conn)
            .map(|model| FloconInode { model })
            .map_err(|e| {
                let kind = match e {
                    diesel::result::Error::NotFound => std::io::ErrorKind::NotFound,
                    _ => std::io::ErrorKind::Other,
                };
                Error::new(kind, e)
            })
    }

    fn empty_inode(&mut self) -> Self::Inode {
        let epoch = DateTime::from_timestamp(0, 0).unwrap();

        FloconInode {
            model: ModelInode {
                id: 0,
                mode: 0,
                uid: 0,
                gid: 0,
                size: 0,
                atime: epoch.into(),
                mtime: epoch.into(),
                ctime: epoch.into(),
                btime: epoch.into(),
            },
        }
    }

    /// Finding children of a given directory which also are directories. This
    /// might sound weird, but that's what WinterFs needs in order to simulate
    /// UNIX-style link-counting on directories
    fn count_child_directories(&mut self, inode: u64) -> std::io::Result<u64> {
        let num_dirs = link
            .inner_join(inode_table.on(child_id.eq(inode_id)))
            .filter(parent_id.eq(inode as i32))
            .filter(sql::<Integer>(&format!("mode & {}", S_IFDIR)).ne(0))
            .count()
            .get_result::<i64>(&mut self.ctx.conn)
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e))?;

        Ok(num_dirs as u64)
    }

    /// Finds all the potential parents of a given inode, meaning that it's all
    /// the directories in which you would find this inode
    fn find_parents_of(&mut self, inode: u64) -> std::io::Result<Vec<u64>> {
        link.filter(child_id.eq(inode as i32))
            .select(parent_id)
            .load::<i32>(&mut self.ctx.conn)
            .map(|ids| ids.into_iter().map(|id| id as u64).collect())
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e))
    }

    fn create_inode(
        &mut self,
        mode: mode_t,
        uid_t: uid_t,
        gid_t: gid_t,
    ) -> std::io::Result<Self::Inode> {
        use crate::schema::inode;

        let now = Utc::now();
        let new_inode = NewInode {
            mode: mode as i32,
            uid: uid_t as i32,
            gid: gid_t as i32,
            size: 0,
            atime: now.into(),
            mtime: now.into(),
            ctime: now.into(),
            btime: now.into(),
        };

        let inserted_inode = diesel::insert_into(inode::table)
            .values(&new_inode)
            .get_result::<ModelInode>(&mut self.ctx.conn)
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e))?;

        Ok(FloconInode {
            model: inserted_inode,
        })
    }

    fn add_child(&mut self, parent: u64, child: u64, name: &str) -> std::io::Result<()> {
        let new_link = NewLink {
            parent_id: parent as i32,
            child_id: child as i32,
            name: name.to_string(),
        };

        diesel::insert_into(link)
            .values(&new_link)
            .execute(&mut self.ctx.conn)
            .map(|_| ())
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e))
    }

    fn remove_child(&mut self, parent: u64, name: &str) -> std::io::Result<()> {
        let target = link
            .filter(parent_id.eq(parent as i32))
            .filter(link_name.eq(name));

        diesel::delete(target)
            .execute(&mut self.ctx.conn)
            .map(|_| ())
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e))
    }

    fn find_children_of(
        &mut self,
        parent: u64,
        size: size_t,
        offset: size_t,
    ) -> std::io::Result<Vec<(String, u64)>> {
        let results: Vec<(String, i32)> = link
            .filter(parent_id.eq(parent as i32))
            .select((link_name, child_id))
            .order(link_name.asc())
            .limit(size as i64)
            .offset(offset as i64)
            .load(&mut self.ctx.conn)
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e))?;

        Ok(results
            .into_iter()
            .map(|(name, id)| (name, id as u64))
            .collect())
    }
}

pub struct FloconInode {
    model: ModelInode,
}

impl WinterInode for FloconInode {
    fn get_id(&self) -> u64 {
        self.model.id as u64
    }

    fn get_mode(&self) -> mode_t {
        self.model.mode as mode_t
    }

    fn make_entry(&self) -> std::io::Result<EntryCore> {
        let atime: DateTime<Utc> = self.model.atime.into();
        let mtime: DateTime<Utc> = self.model.mtime.into();
        let ctime: DateTime<Utc> = self.model.ctime.into();

        Ok(EntryCore {
            st_ino: self.model.id as u64,
            st_size: self.model.size as off_t,
            st_atime: atime.timestamp(),
            st_atime_nsec: atime.timestamp_subsec_nanos().into(),
            st_mtime: mtime.timestamp(),
            st_mtime_nsec: mtime.timestamp_subsec_nanos().into(),
            st_ctime: ctime.timestamp(),
            st_ctime_nsec: ctime.timestamp_subsec_nanos().into(),
            st_mode: self.model.mode as mode_t,
            st_uid: self.model.uid as uid_t,
            st_gid: self.model.gid as gid_t,
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
}

impl FloconHandle {
    /// Shared method between all the set_xxx() methods which will update a
    /// given attribute in storage and reload the associated model with the
    /// latest available version.
    fn update_inode<CS>(
        &mut self,
        context: &mut FloconContext,
        changeset: CS,
    ) -> std::io::Result<()>
    where
        CS: diesel::query_builder::AsChangeset<Target = crate::schema::inode::table>,
        CS::Changeset: diesel::query_builder::QueryFragment<diesel::sqlite::Sqlite>,
    {
        use crate::schema::inode::dsl::inode as inode_table;

        let updated = diesel::update(inode_table.find(self.inode.get_id() as i32))
            .set(changeset)
            .get_result::<ModelInode>(&mut context.conn)
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        self.inode.model = updated;
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
            .batch_execute("PRAGMA wal_checkpoint(TRUNCATE)")
            .map_err(|e| {
                Error::new(ErrorKind::Other, format!("Failed to checkpoint WAL: {}", e))
            })?;

        Ok(())
    }

    /// Same as fsync_data but slightly less aggressive
    fn fsync_metadata(&mut self, context: &mut Self::Context) -> std::io::Result<()> {
        context
            .conn
            .batch_execute("PRAGMA wal_checkpoint(PASSIVE)")
            .map_err(|e| {
                Error::new(ErrorKind::Other, format!("Failed to checkpoint WAL: {}", e))
            })?;

        Ok(())
    }

    fn truncate(&mut self, context: &mut Self::Context) -> std::io::Result<()> {
        use crate::schema::block::dsl::{block, inode_id};
        use crate::schema::inode::dsl::{inode, size as inode_size};

        diesel::delete(block.filter(inode_id.eq(self.inode.get_id() as i32)))
            .execute(&mut context.conn)
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        let updated_inode = diesel::update(inode.find(self.inode.get_id() as i32))
            .set((inode_size.eq(0),))
            .get_result::<ModelInode>(&mut context.conn)
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        self.inode.model = updated_inode;
        Ok(())
    }

    fn set_mode(&mut self, context: &mut Self::Context, mode: mode_t) -> std::io::Result<()> {
        use crate::schema::inode::dsl::mode as inode_mode;
        self.update_inode(context, inode_mode.eq(mode as i32))
    }

    fn set_uid(&mut self, context: &mut Self::Context, uid: uid_t) -> std::io::Result<()> {
        use crate::schema::inode::dsl::uid as inode_uid;
        self.update_inode(context, inode_uid.eq(uid as i32))
    }

    fn set_gid(&mut self, context: &mut Self::Context, gid: gid_t) -> std::io::Result<()> {
        use crate::schema::inode::dsl::gid as inode_gid;
        self.update_inode(context, inode_gid.eq(gid as i32))
    }

    /// Grows or shrinks the file to fit the size that we're asked to have.
    /// There are quite a few logic branches in there, but it should cover all
    /// the cases.
    fn set_size(&mut self, context: &mut Self::Context, size: off_t) -> std::io::Result<()> {
        use crate::schema::block::dsl::{block, first_byte, inode_id, last_byte};
        use crate::schema::inode::dsl::{inode, size as inode_size};

        let current_inode_id = self.inode.get_id() as i32;
        let current_size = self.inode.model.size as off_t;

        if size == current_size {
            return Ok(());
        }

        if size < current_size {
            // Truncating - remove or trim blocks beyond the new size
            let truncate_point = size as i32;

            // Delete blocks that are entirely beyond the new size
            diesel::delete(
                block
                    .filter(inode_id.eq(current_inode_id))
                    .filter(first_byte.ge(truncate_point)),
            )
            .execute(&mut context.conn)
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

            // Find blocks that span the truncation point
            let spanning_blocks: Vec<ModelBlock> = block
                .filter(inode_id.eq(current_inode_id))
                .filter(first_byte.lt(truncate_point))
                .filter(last_byte.ge(truncate_point))
                .load::<ModelBlock>(&mut context.conn)
                .map_err(|e| Error::new(ErrorKind::Other, e))?;

            // Trim blocks that span the truncation point
            for spanning_block in spanning_blocks {
                let mut working_block = WorkingBlock::from_model(spanning_block);
                if let Ok(trimmed) =
                    working_block.clip(working_block.first_byte, truncate_point - 1)
                {
                    // Delete the old block
                    diesel::delete(
                        block.filter(crate::schema::block::id.eq(working_block.id.unwrap())),
                    )
                    .execute(&mut context.conn)
                    .map_err(|e| Error::new(ErrorKind::Other, e))?;

                    // Insert the trimmed block
                    let new_block = trimmed.to_new_block(current_inode_id);
                    diesel::insert_into(crate::schema::block::table)
                        .values(&new_block)
                        .execute(&mut context.conn)
                        .map_err(|e| Error::new(ErrorKind::Other, e))?;
                }
            }
        } else {
            // Growing - create a zero-filled block if needed
            if current_size > 0 {
                // Find the last block to see if we need to fill a gap
                let last_block: Option<ModelBlock> = block
                    .filter(inode_id.eq(current_inode_id))
                    .order(last_byte.desc())
                    .first::<ModelBlock>(&mut context.conn)
                    .optional()
                    .map_err(|e| Error::new(ErrorKind::Other, e))?;

                if let Some(last) = last_block {
                    let gap_start = last.last_byte + 1;
                    let gap_end = (size - 1) as i32;

                    if gap_start <= gap_end {
                        // Create a zero-filled block for the gap
                        let gap_block = WorkingBlock::new(None, gap_start, gap_end, None);
                        let new_block = gap_block.to_new_block(current_inode_id);
                        diesel::insert_into(crate::schema::block::table)
                            .values(&new_block)
                            .execute(&mut context.conn)
                            .map_err(|e| Error::new(ErrorKind::Other, e))?;
                    }
                } else if size > 0 {
                    // No blocks exist, create one zero-filled block
                    let new_block = WorkingBlock::new(None, 0, (size - 1) as i32, None);
                    let block_to_insert = new_block.to_new_block(current_inode_id);
                    diesel::insert_into(crate::schema::block::table)
                        .values(&block_to_insert)
                        .execute(&mut context.conn)
                        .map_err(|e| Error::new(ErrorKind::Other, e))?;
                }
            }
        }

        // Update the inode size
        let updated_inode = diesel::update(inode.find(current_inode_id))
            .set(inode_size.eq(size as i32))
            .get_result::<ModelInode>(&mut context.conn)
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        self.inode.model = updated_inode;
        Ok(())
    }

    fn set_atime(
        &mut self,
        context: &mut Self::Context,
        atime: DateTime<Utc>,
    ) -> std::io::Result<()> {
        use crate::schema::inode::dsl::atime as inode_atime;
        self.update_inode(context, inode_atime.eq::<DateTimeUtc>(atime.into()))
    }

    fn set_mtime(
        &mut self,
        context: &mut Self::Context,
        mtime: DateTime<Utc>,
    ) -> std::io::Result<()> {
        use crate::schema::inode::dsl::mtime as inode_mtime;
        self.update_inode(context, inode_mtime.eq::<DateTimeUtc>(mtime.into()))
    }

    fn set_ctime(
        &mut self,
        context: &mut Self::Context,
        ctime: DateTime<Utc>,
    ) -> std::io::Result<()> {
        use crate::schema::inode::dsl::ctime as inode_ctime;
        self.update_inode(context, inode_ctime.eq::<DateTimeUtc>(ctime.into()))
    }

    /// We're going through all the blocks "touched" by the read operation and
    /// then throwing them as efficiently as possible into the provided writer
    /// given to us by FUSE.
    fn read(
        &mut self,
        context: &mut Self::Context,
        size: size_t,
        offset: off_t,
        w: &mut dyn ZeroCopyWriter,
    ) -> std::io::Result<size_t> {
        use crate::schema::block::dsl::{block, first_byte, inode_id, last_byte};

        let current_inode_id = self.inode.get_id() as i32;
        let file_size = self.inode.model.size as off_t;

        if offset >= file_size {
            return Ok(0);
        }

        let read_start = offset as i32;
        let read_end = (offset + size as off_t - 1).min(file_size - 1) as i32;

        if read_start > read_end {
            return Ok(0);
        }

        let model_blocks: Vec<ModelBlock> = block
            .filter(inode_id.eq(current_inode_id))
            .filter(last_byte.ge(read_start))
            .filter(first_byte.le(read_end))
            .order(first_byte.asc())
            .load::<ModelBlock>(&mut context.conn)
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        let working_blocks: Vec<WorkingBlock> = model_blocks
            .into_iter()
            .map(WorkingBlock::from_model)
            .collect();

        let sequence = Sequence::new(working_blocks).clip(read_start, read_end);
        sequence.concrete_data_to_writer(w)
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
        r: &mut dyn ZeroCopyReader,
    ) -> std::io::Result<size_t> {
        use crate::schema::block::dsl::{block, first_byte, inode_id, last_byte};
        use crate::schema::inode::dsl::{inode, size as inode_size};

        let current_inode_id = self.inode.get_id() as i32;
        let write_start = offset as i32;
        let write_end = (offset + size as off_t - 1) as i32;

        let mut incoming_data = vec![0u8; size];
        let bytes_read = r.read(&mut incoming_data)?;
        if bytes_read != size {
            return Err(Error::new(ErrorKind::Other, "Unexpected EOF"));
        }

        let model_blocks: Vec<ModelBlock> = block
            .filter(inode_id.eq(current_inode_id))
            .filter(last_byte.ge(write_start))
            .filter(first_byte.le(write_end))
            .order(first_byte.asc())
            .load::<ModelBlock>(&mut context.conn)
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        let working_blocks: Vec<WorkingBlock> = model_blocks
            .into_iter()
            .map(WorkingBlock::from_model)
            .collect();

        let original_block_ids: std::collections::HashSet<i32> =
            working_blocks.iter().filter_map(|b| b.id).collect();

        let mut sequence = Sequence::new(working_blocks);

        let mut current_offset = write_start;
        for chunk in incoming_data.chunks(self.block_size as usize) {
            let chunk_end = current_offset + chunk.len() as i32 - 1;
            let new_block =
                WorkingBlock::new(None, current_offset, chunk_end, Some(chunk.to_vec()));
            sequence = sequence.replace(new_block);
            current_offset = chunk_end + 1;
        }

        for blk in &sequence.blocks {
            if let Some(_id) = blk.id {
                // This block didn't change, so we don't do anything
            } else {
                let new_block = blk.to_new_block(current_inode_id);
                diesel::insert_into(crate::schema::block::table)
                    .values(&new_block)
                    .execute(&mut context.conn)
                    .map_err(|e| Error::new(ErrorKind::Other, e))?;
            }
        }

        let remaining_block_ids: std::collections::HashSet<i32> =
            sequence.blocks.iter().filter_map(|b| b.id).collect();

        let deleted_ids: Vec<i32> = original_block_ids
            .difference(&remaining_block_ids)
            .cloned()
            .collect();

        if !deleted_ids.is_empty() {
            use crate::schema::block::dsl::id;
            diesel::delete(block.filter(id.eq_any(deleted_ids)))
                .execute(&mut context.conn)
                .map_err(|e| Error::new(ErrorKind::Other, e))?;
        }

        let new_size = (write_end + 1).max(self.inode.model.size);
        if new_size != self.inode.model.size {
            let updated_inode = diesel::update(inode.find(current_inode_id))
                .set(inode_size.eq(new_size))
                .get_result::<ModelInode>(&mut context.conn)
                .map_err(|e| Error::new(ErrorKind::Other, e))?;
            self.inode.model = updated_inode;
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
        r: &mut dyn ZeroCopyReader,
    ) -> std::io::Result<size_t> {
        use crate::schema::block;
        use crate::schema::block::dsl::{block as block_table, inode_id, last_byte};
        use crate::schema::inode::dsl::{inode, size as inode_size};

        let current_inode_id = self.inode.get_id() as i32;

        let mut incoming_data = vec![0u8; size];
        let bytes_read = r.read(&mut incoming_data)?;
        if bytes_read == 0 {
            return Ok(0);
        }

        incoming_data.truncate(bytes_read);

        let append_start = block_table
            .filter(inode_id.eq(current_inode_id))
            .select(diesel::dsl::max(last_byte))
            .first::<Option<i32>>(&mut context.conn)
            .map_err(|e| Error::new(ErrorKind::Other, e))?
            .map(|max_byte| max_byte + 1)
            .unwrap_or(0);

        let mut working_blocks = Vec::new();
        let mut current_offset = append_start;

        for chunk in incoming_data.chunks(self.block_size as usize) {
            let chunk_end = current_offset + chunk.len() as i32 - 1;
            working_blocks.push(WorkingBlock::new(
                None,
                current_offset,
                chunk_end,
                Some(chunk.to_vec()),
            ));
            current_offset = chunk_end + 1;
        }

        let new_blocks: Vec<_> = working_blocks
            .iter()
            .map(|wb| wb.to_new_block(current_inode_id))
            .collect();

        if !new_blocks.is_empty() {
            diesel::insert_into(block::table)
                .values(&new_blocks)
                .execute(&mut context.conn)
                .map_err(|e| Error::new(ErrorKind::Other, e))?;
        }

        let new_size = append_start + bytes_read as i32;
        let updated_inode = diesel::update(inode.find(current_inode_id))
            .set(inode_size.eq(new_size))
            .get_result::<ModelInode>(&mut context.conn)
            .map_err(|e| Error::new(ErrorKind::Other, e))?;

        self.inode.model = updated_inode;

        Ok(bytes_read)
    }
}

pub struct Flocon {
    fs_manager: FileSystemManager,
}

impl Flocon {
    pub fn new(fs_manager: FileSystemManager) -> Self {
        Flocon { fs_manager }
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
    fn block_size(&self) -> Result<blksize_t, std::io::Error> {
        Ok(1024 * 1024)
    }

    fn create_context(&self) -> Result<Self::Context, Error> {
        let mut conn = self
            .fs_manager
            .get_connection()
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e))?;
        conn.batch_execute("begin")
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e))?;
        Ok(FloconContext { conn })
    }

    fn sync_context(&self, ctx: &mut Self::Context) -> Result<(), Error> {
        ctx.conn
            .batch_execute("commit")
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e))
    }

    fn rollback_context(&self, ctx: &mut Self::Context) -> Result<(), Error> {
        ctx.conn
            .batch_execute("rollback")
            .map_err(|e| Error::new(std::io::ErrorKind::Other, e))
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

        Ok(FloconHandle {
            inode: tree.get_inode(inode)?,
            block_size: self.block_size()?,
        })
    }
}
