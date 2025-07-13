use chrono::{DateTime, TimeZone, Utc};
use core::time::Duration;
use fuse_backend_rs::abi::fuse_abi::{CreateIn, FsOptions, OpenOptions, SetattrValid};
use fuse_backend_rs::api::filesystem::{
    Context, DirEntry, Entry, FileSystem, GetxattrReply, ListxattrReply, ZeroCopyReader,
    ZeroCopyWriter,
};
use libc::{
    O_EXCL, O_TRUNC, RENAME_EXCHANGE, RENAME_NOREPLACE, S_IFDIR, S_IFREG, blkcnt64_t, blksize_t,
    c_ulong, gid_t, ino64_t, mode_t, off_t, size_t, stat64, statvfs64, time_t, uid_t,
};
use std::collections::HashMap;
use std::ffi::CStr;
use std::mem::zeroed;
use std::sync::Mutex;
use std::{io, mem};

pub trait WinterTree {
    type Inode: WinterInode;

    fn lookup(&mut self, parent: u64, name: &str) -> io::Result<u64>;

    fn get_inode(&mut self, inode: u64) -> io::Result<Self::Inode>;

    fn empty_inode(&mut self) -> Self::Inode;

    fn count_child_directories(&mut self, inode: u64) -> io::Result<u64>;

    fn find_parents_of(&mut self, inode: u64) -> io::Result<Vec<u64>>;

    fn create_inode(&mut self, mode: mode_t, uid_t: uid_t, gid_t: gid_t)
    -> io::Result<Self::Inode>;

    fn add_child(&mut self, parent: u64, child: u64, name: &str) -> io::Result<()>;

    fn remove_child(&mut self, parent: u64, name: &str) -> io::Result<()>;

    /// Lists all the children of the given parent directory. You do not need
    /// to return "." and "..", those are automatically generated. The list is
    /// paginated, so make sure that the output order is stable at least until
    /// the content of the directory changes (or ideally until forever).
    ///
    /// - `parent`: ID of the parent inode
    /// - `size`: number of expected returned items
    /// - `offset`: offset from the list
    fn find_children_of(
        &mut self,
        parent: u64,
        size: size_t,
        offset: size_t,
    ) -> io::Result<Vec<(String, u64)>>;

    /// Explicitly deletes an inode and its data from storage. Essentially it's
    /// not because an inode has zero link remaining that it should be deleted,
    /// instead this function will be called to do the job.
    fn delete_inode(&mut self, inode: u64) -> io::Result<()>;
}

/// That's the core of the attributes we're going to need for an inode Entry,
/// minus dynamic or tree-related stuff which will computed otherwise
pub struct EntryCore {
    pub st_ino: ino64_t,
    pub st_size: off_t,
    pub st_atime: time_t,
    pub st_atime_nsec: i64,
    pub st_mtime: time_t,
    pub st_mtime_nsec: i64,
    pub st_ctime: time_t,
    pub st_ctime_nsec: i64,
    pub st_mode: mode_t,
    pub st_uid: uid_t,
    pub st_gid: gid_t,
}

pub trait WinterInode {
    fn get_id(&self) -> u64;

    fn get_mode(&self) -> mode_t;

    fn make_entry(&self) -> io::Result<EntryCore>;

    fn attr_valid_time(&self) -> io::Result<Duration>;

    fn entry_valid_time(&self) -> io::Result<Duration>;
}

pub trait WinterHandle {
    type Context;
    type Inode: WinterInode;

    /// Returns the inode handled by this handle
    fn get_inode(&self) -> &Self::Inode;

    /// You would probably think that this means you have to flush the content
    /// onto the disk. Well you would be wrong. The system calls this when the
    /// file is closed (although it might get called several times from what I
    /// understand?), so it's more something you can use to schedule
    /// maintenance tasks or whatever rather than actually writing the content
    /// That would actually be fsync.
    fn flush(&mut self, context: &mut Self::Context) -> io::Result<()>;

    /// Persists to the disk the data of the file (so blocks basically)
    fn fsync_data(&mut self, context: &mut Self::Context) -> io::Result<()>;

    /// Persists to the disk the metadata (so the inode itself essentially)
    fn fsync_metadata(&mut self, context: &mut Self::Context) -> io::Result<()>;

    /// Empties the file completely
    fn truncate(&mut self, context: &mut Self::Context) -> io::Result<()>;

    fn set_mode(&mut self, context: &mut Self::Context, mode: mode_t) -> io::Result<()>;

    fn set_uid(&mut self, context: &mut Self::Context, uid: uid_t) -> io::Result<()>;

    fn set_gid(&mut self, context: &mut Self::Context, gid: gid_t) -> io::Result<()>;

    fn set_size(&mut self, context: &mut Self::Context, size: off_t) -> io::Result<()>;

    fn set_atime(&mut self, context: &mut Self::Context, atime: DateTime<Utc>) -> io::Result<()>;

    fn set_mtime(&mut self, context: &mut Self::Context, atime: DateTime<Utc>) -> io::Result<()>;

    fn set_ctime(&mut self, context: &mut Self::Context, atime: DateTime<Utc>) -> io::Result<()>;

    /// Reads a slice of the data into the provided writer
    fn read(
        &mut self,
        context: &mut Self::Context,
        size: size_t,
        offset: off_t,
        w: &mut dyn ZeroCopyWriter,
    ) -> io::Result<size_t>;

    /// Persists the provided data into storage
    fn write(
        &mut self,
        context: &mut Self::Context,
        size: size_t,
        offset: off_t,
        r: &mut dyn ZeroCopyReader,
    ) -> io::Result<size_t>;

    /// Like write but guaranteed to append at the end of the file
    fn append(
        &mut self,
        context: &mut Self::Context,
        size: size_t,
        r: &mut dyn ZeroCopyReader,
    ) -> io::Result<size_t>;

    /// Set an extended attribute on the inode
    fn xattr_set(&mut self, context: &mut Self::Context, key: &str, value: &[u8])
    -> io::Result<()>;

    /// Get an extended attribute from the inode (fails if key doesn't exist)
    fn xattr_get(&mut self, context: &mut Self::Context, key: &str) -> io::Result<Vec<u8>>;

    /// Remove an extended attribute from the inode
    fn xattr_remove(&mut self, context: &mut Self::Context, key: &str) -> io::Result<()>;

    /// List all extended attribute keys for the inode
    fn xattr_list(&mut self, context: &mut Self::Context) -> io::Result<Vec<String>>;
}

struct OwnedDirEntry {
    pub ino: ino64_t,
    pub offset: u64,
    pub type_: u32,
    pub name: Vec<u8>,
}

impl OwnedDirEntry {
    fn as_dir_entry(&self) -> DirEntry {
        DirEntry {
            ino: self.ino,
            offset: self.offset,
            type_: self.type_,
            name: &self.name,
        }
    }
}

pub trait WinterFs {
    /// An opaque context object passed to most operations, typically
    /// something that allows you to manipulate the persistence layer
    type Context;

    /// An opaque type for a file handle, which will be stored in RAM as long
    /// as the file is open
    type Handle: WinterHandle<Context = Self::Context, Inode = Self::Inode>;

    /// Internal opaque representation of an Inode
    type Inode: WinterInode;

    /// The type which is responsible for all file tree reading and
    /// manipulation
    type Tree<'ctx>: WinterTree<Inode = Self::Inode>
    where
        Self: 'ctx;

    /// Indicates what is the block size in use of this instance (it does not
    /// have to be strictly true, it's just that lots of syscalls measure size
    /// in blocks instead of bytes, so this is kind of the baseline for giving
    /// editorial info to the callers).
    fn block_size(&self) -> Result<blksize_t, io::Error>;

    /// Returns in bytes an estimation of how much storage is currently being
    /// used on the physical media
    fn estimate_used_storage(&self) -> Result<u64, io::Error>;

    /// Here the goal is to estimate how much free storage there is on the
    /// underlying storage medium so that at least you can have an idea of
    /// how much more data you can add
    fn estimate_free_storage(&self) -> Result<u64, io::Error>;

    /// Counts how many files there are on the disk
    fn estimate_files_count(&self, context: &mut Self::Context) -> Result<u64, io::Error>;

    /// Indicates how many files you could have on the disk
    ///
    /// To be noted that we're taking the max integer and not the max unsigned
    /// integer because apparently some implementations will overflow. But this
    /// is no issue in the sense that it's not _really_ the max given that to
    /// achieve this file count by creating one file every millisecond you'd
    /// still need 292,277,024 years.
    fn max_files_count(&self, _context: &mut Self::Context) -> Result<u64, io::Error> {
        Ok(i64::MAX as u64)
    }

    /// If you have a limit in the max length of a file name give it here, we
    /// give what seems to be a decent default, given that anyways software
    /// up the chain probably has this limit hard-coded anyways.
    fn get_name_max_size(&self, _context: &mut Self::Context) -> Result<u64, io::Error> {
        Ok(4096)
    }

    /// Creates a new context for an operation or a set of operations
    fn create_context(&self) -> Result<Self::Context, io::Error>;

    /// Commits the changes made within the context
    fn sync_context(&self, context: &mut Self::Context) -> Result<(), io::Error>;

    /// Rolls back any changes made within the context
    fn rollback_context(&self, context: &mut Self::Context) -> Result<(), io::Error>;

    /// Closes up and releases the context
    fn close_context(&self, context: &mut Self::Context) -> Result<(), io::Error>;

    /// Returns the tree for the given context
    fn tree<'ctx>(&self, context: &'ctx mut Self::Context) -> Result<Self::Tree<'ctx>, io::Error>;

    /// Opens a file and returns the backend handle
    fn open(
        &self,
        context: &mut Self::Context,
        inode: u64,
        flags: u32,
    ) -> Result<Self::Handle, io::Error>;
}

/// This provides an abstraction layer on top of FUSE to get at disposal a
/// simple trait to implement which has a more orthogonal cut of filesystem
/// operations than what the low-level FUSE asks for. The idea is to separate
/// between the actual filesystem logic and the "dealing with Kernel
/// shenanigans" logic
pub struct WinterFsHandler<FS: WinterFs> {
    fs: FS,
    next_handle: Mutex<u64>,
    handles: Mutex<HashMap<u64, FS::Handle>>,
    lookup_counts: Mutex<HashMap<u64, u64>>,
}

impl<FS: WinterFs> WinterFsHandler<FS> {
    pub fn new(fs: FS) -> Self {
        Self {
            fs,
            next_handle: Mutex::new(1),
            handles: Mutex::new(HashMap::new()),
            lookup_counts: Mutex::new(HashMap::new()),
        }
    }

    /// Some operations will increase the "lookup count" of a given inode. This
    /// is essentially how many parts of the system might still be referencing
    /// this inode. For example even after a file is deleted you can still
    /// write to it. It's because of this lookup count.
    fn increase_lookup(&self, inode: u64, amount: u64) {
        let mut counts = self.lookup_counts.lock().unwrap();
        let count = counts.entry(inode).or_insert(0);
        *count = count.saturating_add(amount);
    }

    /// Decreases the lookup count on a given inode, and at the same time
    /// perform garbage collection on said inode. For starters, when the count
    /// drops to zero then we stop keeping scores on the lookup count. But also
    /// when there are no links left to that inode then we know it needs to be
    /// removed from storage.
    fn decrease_lookup(&self, inode: u64, amount: u64, tree: &mut FS::Tree<'_>) -> io::Result<()> {
        tracing::trace!(
            "decrease_lookup called for inode {} with amount {}",
            inode,
            amount
        );

        let mut counts = self.lookup_counts.lock().unwrap();
        if let Some(count) = counts.get_mut(&inode) {
            let old_count = *count;
            *count = count.saturating_sub(amount);
            tracing::trace!(
                "decrease_lookup: inode {} count changed from {} to {}",
                inode,
                old_count,
                *count
            );

            if *count == 0 {
                counts.remove(&inode);
                tracing::debug!(
                    "decrease_lookup: inode {} lookup count reached 0, checking link count for deletion",
                    inode
                );

                let parents = tree.find_parents_of(inode)?;
                tracing::debug!(
                    "decrease_lookup: inode {} has {} parent links",
                    inode,
                    parents.len()
                );

                if parents.is_empty() {
                    tracing::info!(
                        "decrease_lookup: deleting inode {} as it has no remaining links or lookups",
                        inode
                    );
                    tree.delete_inode(inode)?;
                }
            }
        } else {
            tracing::debug!(
                "decrease_lookup called for inode {} which is not tracked. This is safe to ignore (e.g., after a restart).",
                inode
            );
        }
        Ok(())
    }

    /// Helper method to work with a handle if it exists
    fn with_handle<R>(
        &self,
        handle: u64,
        f: impl FnOnce(&FS::Handle) -> R,
    ) -> io::Result<Option<R>> {
        if handle == 0 {
            Ok(None)
        } else {
            let guard = self.handles.lock().unwrap();
            guard
                .get(&handle)
                .map(|h| Some(f(h)))
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid handle"))
        }
    }

    /// Helper method to work with a mutable handle if it exists
    fn with_handle_mut<R>(
        &self,
        handle: u64,
        f: impl FnOnce(&mut FS::Handle) -> R,
    ) -> io::Result<Option<R>> {
        if handle == 0 {
            Ok(None)
        } else {
            let mut guard = self.handles.lock().unwrap();
            guard
                .get_mut(&handle)
                .map(|h| Some(f(h)))
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid handle"))
        }
    }

    /// Helper that provides a context and handle, creating a temporary handle
    /// if needed
    fn with_context_and_handle<R>(
        &self,
        inode: u64,
        handle: Option<u64>,
        flags: u32,
        f: impl FnOnce(&mut FS::Context, &mut FS::Handle) -> io::Result<R>,
    ) -> io::Result<R> {
        self.with_context(|ctx| {
            let (use_temp_handle, handle_id) = if let Some(h) = handle.filter(|&h| h != 0) {
                (false, h)
            } else {
                let fh = self.fs.open(ctx, inode, flags)?;
                let id = self.create_handle(fh);
                (true, id)
            };

            let result = self
                .with_handle_mut(handle_id, |fh| f(ctx, fh))?
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid handle"))?;

            if use_temp_handle {
                self.remove_handle(handle_id);
            }

            result
        })
    }

    /// Helper that provides a context and handle for read operations
    fn with_context_and_handle_read<R>(
        &self,
        inode: u64,
        handle: u64,
        f: impl FnOnce(&mut FS::Context, &mut FS::Handle) -> io::Result<R>,
    ) -> io::Result<R> {
        self.with_context_and_handle(inode, Some(handle), libc::O_RDONLY as u32, f)
    }

    /// Helper that provides a context and handle for write operations
    fn with_context_and_handle_write<R>(
        &self,
        inode: u64,
        handle: u64,
        f: impl FnOnce(&mut FS::Context, &mut FS::Handle) -> io::Result<R>,
    ) -> io::Result<R> {
        self.with_context_and_handle(inode, Some(handle), libc::O_WRONLY as u32, f)
    }

    /// Allocate a new handle ID and insert the handle into the map
    fn create_handle(&self, handle: FS::Handle) -> u64 {
        let mut next = self.next_handle.lock().unwrap();
        let mut map = self.handles.lock().unwrap();
        let id = *next;
        map.insert(id, handle);
        *next += 1;
        id
    }

    /// Remove a handle from the map
    fn remove_handle(&self, handle: u64) -> Option<FS::Handle> {
        if handle == 0 {
            None
        } else {
            self.handles.lock().unwrap().remove(&handle)
        }
    }

    /// Implements a context manager which will make sure to properly open
    /// close and commit the context depending on the underlying success of the
    /// operation.
    fn with_context<R>(&self, f: impl FnOnce(&mut FS::Context) -> io::Result<R>) -> io::Result<R> {
        let mut ctx = self.fs.create_context()?;
        let res = f(&mut ctx);
        match res {
            Ok(v) => {
                self.fs.sync_context(&mut ctx)?;
                self.fs.close_context(&mut ctx)?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.fs.rollback_context(&mut ctx);
                let _ = self.fs.close_context(&mut ctx);
                Err(e)
            }
        }
    }

    /// Counts the hardlinks to a given inode, simulating what the regular
    /// count would be on a UNIX file system (even if you ask me this way of
    /// counting makes absolutely no fucking sense, that's why we're simulating
    /// it there).
    fn count_links<'ctx>(
        &self,
        inode: u64,
        mode: mode_t,
        tree: &mut FS::Tree<'ctx>,
    ) -> io::Result<u64> {
        if mode & S_IFDIR as mode_t != 0 {
            let count = tree.count_child_directories(inode)?;
            Ok(count + 2)
        } else {
            let found = tree.find_parents_of(inode)?;
            Ok(found.len() as u64)
        }
    }

    /// Transforms an Inode into an Entry for FUSE
    fn make_entry<'ctx>(&self, inode: &FS::Inode, tree: &mut FS::Tree<'ctx>) -> io::Result<Entry> {
        let core = inode.make_entry()?;
        let bs = self.fs.block_size()? as off_t;

        let mut st: stat64 = unsafe { zeroed() };
        st.st_ino = core.st_ino;
        st.st_nlink = self.count_links(core.st_ino, core.st_mode, tree)?;
        st.st_mode = core.st_mode;
        st.st_uid = core.st_uid;
        st.st_gid = core.st_gid;
        st.st_size = core.st_size;
        st.st_blksize = bs;
        st.st_blocks = ((core.st_size + bs - 1) / bs) as blkcnt64_t;
        st.st_atime = core.st_atime;
        st.st_atime_nsec = core.st_atime_nsec;
        st.st_mtime = core.st_mtime;
        st.st_mtime_nsec = core.st_mtime_nsec;
        st.st_ctime = core.st_ctime;
        st.st_ctime_nsec = core.st_ctime_nsec;

        Ok(Entry {
            inode: core.st_ino,
            generation: 0,
            attr: st,
            attr_flags: 0,
            attr_timeout: inode.attr_valid_time()?,
            entry_timeout: inode.entry_valid_time()?,
        })
    }

    /// Helper function for removing entries from the filesystem
    /// - `require_directory`: if true, the target must be a directory; if
    ///   false, it must NOT be a directory
    /// - `check_empty`: if true, checks that the directory is empty (only
    ///   relevant when require_directory is true)
    fn remove_entry(
        &self,
        parent: u64,
        name: &CStr,
        require_directory: bool,
        check_empty: bool,
    ) -> io::Result<()> {
        self.with_context(|op_ctx| {
            let name_str = name
                .to_str()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid UTF-8"))?;
            let mut tree = self.fs.tree(op_ctx)?;

            let parent_inode = tree.get_inode(parent)?;

            if parent_inode.get_mode() & S_IFDIR == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "Parent is not a directory",
                ));
            }

            let child_id = tree.lookup(parent, name_str)?;

            if child_id == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    if require_directory {
                        "Directory not found"
                    } else {
                        "File not found"
                    },
                ));
            }

            let child = tree.get_inode(child_id)?;
            let is_directory = child.get_mode() & S_IFDIR != 0;

            if require_directory && !is_directory {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "Not a directory",
                ));
            } else if !require_directory && is_directory {
                return Err(io::Error::new(
                    io::ErrorKind::IsADirectory,
                    "Is a directory",
                ));
            }

            if check_empty && is_directory {
                let children = tree.find_children_of(child_id, 1, 0)?;
                if !children.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::DirectoryNotEmpty,
                        "Directory not empty",
                    ));
                }
            }

            tree.remove_child(parent, name_str)?;

            Ok(())
        })
    }

    fn inner_readdir(
        &self,
        _ctx: &Context,
        inode: u64,
        handle: u64,
        size: u32,
        offset: u64,
    ) -> io::Result<Vec<(OwnedDirEntry, Entry)>> {
        self.with_context(|op_ctx| {
            let mut tree = self.fs.tree(op_ctx)?;

            let dir_id = if handle != 0 {
                self.with_handle(handle, |fh| fh.get_inode().get_id())?
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid handle"))?
            } else {
                inode
            };

            let dir_inode = tree.get_inode(dir_id)?;

            if dir_inode.get_mode() & S_IFDIR == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "Not a directory",
                ));
            }

            let mut entries = Vec::new();
            let mut current_offset = 1u64;

            if offset == 0 && size > 0 {
                let dot_entry = OwnedDirEntry {
                    ino: dir_id as ino64_t,
                    offset: current_offset,
                    type_: libc::DT_DIR.into(),
                    name: b".".to_vec(),
                };
                let dot_inode = tree.get_inode(dir_id)?;
                let dot_entry_data = self.make_entry(&dot_inode, &mut tree)?;
                entries.push((dot_entry, dot_entry_data));
                current_offset += 1;

                if entries.len() < size as usize {
                    let parent_ids = tree.find_parents_of(dir_id)?;
                    let parent_id = parent_ids.first().copied().unwrap_or(dir_id);

                    let dotdot_entry = OwnedDirEntry {
                        ino: parent_id as ino64_t,
                        offset: current_offset,
                        type_: libc::DT_DIR.into(),
                        name: b"..".to_vec(),
                    };
                    let parent_inode = tree.get_inode(parent_id)?;
                    let dotdot_entry_data = self.make_entry(&parent_inode, &mut tree)?;
                    entries.push((dotdot_entry, dotdot_entry_data));
                    current_offset += 1;
                }
            }

            let regular_offset = if offset <= 2 {
                0
            } else {
                (offset - 2) as usize
            };

            let remaining_size = (size as usize).saturating_sub(entries.len());

            if remaining_size > 0 && offset <= current_offset {
                let children: Vec<(String, u64)> = tree
                    .find_children_of(dir_id, remaining_size, regular_offset)?
                    .into_iter()
                    .collect();

                for (name, child_id) in children {
                    current_offset += 1;

                    let child_inode = tree.get_inode(child_id)?;
                    let child_mode = child_inode.get_mode();

                    let dtype = ((child_mode & libc::S_IFMT) >> 12) as u32;

                    let dir_entry = OwnedDirEntry {
                        ino: child_id as ino64_t,
                        offset: current_offset,
                        type_: dtype,
                        name: name.into_bytes(),
                    };

                    let entry = self.make_entry(&child_inode, &mut tree)?;
                    entries.push((dir_entry, entry));
                }
            }

            Ok(entries)
        })
    }
}

/// Here we implement the sync interface (the async one isn't ready apparently)
impl<FS> FileSystem for WinterFsHandler<FS>
where
    FS: WinterFs + Send + Sync + 'static,
{
    type Inode = u64;
    type Handle = u64;

    fn init(&self, capable: FsOptions) -> io::Result<FsOptions> {
        let mut wanted = FsOptions::empty();

        #[cfg(target_os = "linux")]
        {
            wanted.insert(FsOptions::MAX_PAGES);
            wanted.insert(FsOptions::BIG_WRITES);
            wanted.insert(FsOptions::WRITEBACK_CACHE);
        }

        wanted.insert(FsOptions::AUTO_INVAL_DATA);
        wanted.insert(FsOptions::ATOMIC_O_TRUNC);
        wanted.insert(FsOptions::DO_READDIRPLUS);
        wanted.insert(FsOptions::READDIRPLUS_AUTO);
        wanted.insert(FsOptions::CACHE_SYMLINKS);
        wanted.insert(FsOptions::POSIX_ACL);

        Ok(capable & wanted)
    }

    fn lookup(&self, _ctx: &Context, parent: Self::Inode, name: &CStr) -> io::Result<Entry> {
        self.with_context(|op_ctx| {
            let mut tree = self.fs.tree(op_ctx)?;
            let name_str = name
                .to_str()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid UTF-8"))?;
            let file_id = tree.lookup(parent, name_str)?;

            if file_id > 0 {
                let entry = self.make_entry(&tree.get_inode(file_id)?, &mut tree)?;
                self.increase_lookup(file_id, 1);
                Ok(entry)
            } else {
                self.make_entry(&tree.empty_inode(), &mut tree)
            }
        })
    }

    fn forget(&self, _ctx: &Context, inode: Self::Inode, count: u64) {
        tracing::debug!("forget called for inode {} with count {}", inode, count);

        let result = self.with_context(|op_ctx| {
            let mut tree = self.fs.tree(op_ctx)?;
            self.decrease_lookup(inode, count, &mut tree)
        });

        match result {
            Ok(()) => {
                tracing::debug!("forget completed successfully for inode {}", inode);
            }
            Err(e) => {
                tracing::error!(
                    "forget failed for inode {} with count {}: {:?}",
                    inode,
                    count,
                    e
                );
            }
        }
    }

    fn batch_forget(&self, _ctx: &Context, requests: Vec<(Self::Inode, u64)>) {
        tracing::debug!("batch_forget called with {} requests", requests.len());

        let result = self.with_context(|op_ctx| {
            let mut tree = self.fs.tree(op_ctx)?;

            for (inode, count) in &requests {
                tracing::debug!(
                    "  processing forget for inode {} with count {}",
                    inode,
                    count
                );
                self.decrease_lookup(*inode, *count, &mut tree)?;
            }

            Ok(())
        });

        match result {
            Ok(()) => {
                tracing::debug!("batch_forget completed successfully");
            }
            Err(e) => {
                tracing::error!("batch_forget failed: {:?}", e);
            }
        }
    }

    fn getattr(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        handle: Option<Self::Handle>,
    ) -> io::Result<(stat64, Duration)> {
        self.with_context(|op_ctx| {
            let mut tree = self.fs.tree(op_ctx)?;

            let inode_id = if let Some(handle_id) = handle.filter(|&h| h != 0) {
                self.with_handle(handle_id, |fh| fh.get_inode().get_id())?
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid handle"))?
            } else {
                inode
            };

            let inode_obj = tree.get_inode(inode_id)?;
            let entry = self.make_entry(&inode_obj, &mut tree)?;
            Ok((entry.attr, entry.attr_timeout))
        })
    }

    fn setattr(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        attr: stat64,
        handle: Option<Self::Handle>,
        valid: SetattrValid,
    ) -> io::Result<(stat64, Duration)> {
        self.with_context_and_handle(inode, handle, 0, |op_ctx, fh| {
            if valid.contains(SetattrValid::MODE) {
                fh.set_mode(op_ctx, attr.st_mode)?;
            }

            if valid.contains(SetattrValid::UID) {
                fh.set_uid(op_ctx, attr.st_uid)?;
            }

            if valid.contains(SetattrValid::GID) {
                fh.set_gid(op_ctx, attr.st_gid)?;
            }

            if valid.contains(SetattrValid::SIZE) {
                fh.set_size(op_ctx, attr.st_size)?;
            }

            if valid.contains(SetattrValid::ATIME) {
                let atime = Utc
                    .timestamp_opt(attr.st_atime, attr.st_atime_nsec as u32)
                    .single()
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("Invalid atime: {} {}", attr.st_atime, attr.st_atime_nsec),
                        )
                    })?;
                fh.set_atime(op_ctx, atime)?;
            } else if valid.contains(SetattrValid::ATIME_NOW) {
                fh.set_atime(op_ctx, Utc::now())?;
            }

            if valid.contains(SetattrValid::MTIME) {
                let mtime = Utc
                    .timestamp_opt(attr.st_mtime, attr.st_mtime_nsec as u32)
                    .single()
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("Invalid mtime: {} {}", attr.st_mtime, attr.st_mtime_nsec),
                        )
                    })?;
                fh.set_mtime(op_ctx, mtime)?;
            } else if valid.contains(SetattrValid::MTIME_NOW) {
                fh.set_mtime(op_ctx, Utc::now())?;
            }

            if valid.contains(SetattrValid::CTIME) {
                let ctime = Utc
                    .timestamp_opt(attr.st_ctime, attr.st_ctime_nsec as u32)
                    .single()
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("Invalid ctime: {} {}", attr.st_ctime, attr.st_ctime_nsec),
                        )
                    })?;
                fh.set_ctime(op_ctx, ctime)?;
            }

            // Get the result
            let mut tree = self.fs.tree(op_ctx)?;
            let result_inode = tree.get_inode(fh.get_inode().get_id())?;
            let entry = self.make_entry(&result_inode, &mut tree)?;
            Ok((entry.attr, entry.attr_timeout))
        })
    }

    fn mkdir(
        &self,
        ctx: &Context,
        parent: Self::Inode,
        name: &CStr,
        mode: u32,
        umask: u32,
    ) -> io::Result<Entry> {
        self.with_context(|op_ctx| {
            let name_str = name
                .to_str()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid UTF-8"))?;
            let mut tree = self.fs.tree(op_ctx)?;
            let file_id = tree.lookup(parent, name_str)?;

            if file_id > 0 {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "File already exists",
                ));
            }

            let full_mode = S_IFDIR | (mode & !umask);
            let file = tree.create_inode(full_mode, ctx.uid, ctx.gid)?;
            tree.add_child(parent, file.get_id(), name_str)?;

            let entry = self.make_entry(&file, &mut tree)?;
            self.increase_lookup(file.get_id(), 1);
            Ok(entry)
        })
    }

    fn unlink(&self, _ctx: &Context, parent: Self::Inode, name: &CStr) -> io::Result<()> {
        self.remove_entry(parent, name, false, false)
    }

    fn rmdir(&self, _ctx: &Context, parent: Self::Inode, name: &CStr) -> io::Result<()> {
        self.remove_entry(parent, name, true, true)
    }

    fn rename(
        &self,
        _ctx: &Context,
        olddir: Self::Inode,
        oldname: &CStr,
        newdir: Self::Inode,
        newname: &CStr,
        flags: u32,
    ) -> io::Result<()> {
        const ALLOWED: [u32; 3] = [0, RENAME_NOREPLACE, RENAME_EXCHANGE];

        if !ALLOWED.contains(&flags) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid flags"));
        }

        let old_name_str = oldname
            .to_str()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid UTF-8"))?;
        let new_name_str = newname
            .to_str()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid UTF-8"))?;

        if old_name_str.is_empty() || new_name_str.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Empty name"));
        }

        self.with_context(|op_ctx| {
            let mut tree = self.fs.tree(op_ctx)?;
            let old_parent = tree.get_inode(olddir)?;
            let new_parent = tree.get_inode(newdir)?;

            let source_inode_id = tree.lookup(old_parent.get_id(), old_name_str)?;

            if source_inode_id == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Source does not exist",
                ));
            }

            let target_inode_id = tree.lookup(new_parent.get_id(), new_name_str)?;
            let target_exists = target_inode_id > 0;

            if target_exists && flags == RENAME_NOREPLACE {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "File already exists",
                ));
            } else if !target_exists && flags == RENAME_EXCHANGE {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Target does not exist",
                ));
            }

            if flags == RENAME_EXCHANGE {
                tree.remove_child(old_parent.get_id(), old_name_str)?;
                tree.remove_child(new_parent.get_id(), new_name_str)?;
                tree.add_child(new_parent.get_id(), source_inode_id, new_name_str)?;
                tree.add_child(old_parent.get_id(), target_inode_id, old_name_str)?;
            } else {
                if target_exists {
                    tree.remove_child(new_parent.get_id(), new_name_str)?;
                }

                tree.remove_child(old_parent.get_id(), old_name_str)?;
                tree.add_child(new_parent.get_id(), source_inode_id, new_name_str)?;
            }

            Ok(())
        })
    }

    fn link(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        newparent: Self::Inode,
        newname: &CStr,
    ) -> io::Result<Entry> {
        self.with_context(|op_ctx| {
            let name_str = newname
                .to_str()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid UTF-8"))?;

            let mut tree = self.fs.tree(op_ctx)?;

            // Get the parent directory
            let parent = tree.get_inode(newparent)?;

            // Verify parent is a directory
            if parent.get_mode() & S_IFDIR == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "Parent is not a directory",
                ));
            }

            // Get the file to link
            let file = tree.get_inode(inode)?;

            // Verify the file is not a directory (hard links to directories are not allowed)
            if file.get_mode() & S_IFDIR != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::IsADirectory,
                    "Cannot create hard link to directory",
                ));
            }

            // Check if the name already exists in the parent directory
            let existing_id = tree.lookup(newparent, name_str)?;
            if existing_id != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "File already exists",
                ));
            }

            // Add the link
            tree.add_child(newparent, inode, name_str)?;

            // Return the entry for the linked file
            let entry = self.make_entry(&file, &mut tree)?;
            self.increase_lookup(inode, 1);
            Ok(entry)
        })
    }

    fn create(
        &self,
        ctx: &Context,
        parent: Self::Inode,
        name: &CStr,
        args: CreateIn,
    ) -> io::Result<(Entry, Option<Self::Handle>, OpenOptions, Option<u32>)> {
        self.with_context(|op_ctx| {
            let name_str = name
                .to_str()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid UTF-8"))?;

            let (file_id, entry) = {
                let mut tree = self.fs.tree(op_ctx)?;
                let existing_id = tree.lookup(parent, name_str)?;

                if existing_id != 0 && args.flags & O_EXCL as u32 != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "File already exists",
                    ));
                }

                let file_id = if existing_id == 0 {
                    let full_mode = S_IFREG | (args.mode & !args.umask);
                    let file = tree.create_inode(full_mode, ctx.uid, ctx.gid)?;
                    let new_id = file.get_id();
                    tree.add_child(parent, new_id, name_str)?;
                    self.increase_lookup(new_id, 1);
                    new_id
                } else {
                    self.increase_lookup(existing_id, 1);
                    existing_id
                };

                let inode = tree.get_inode(file_id)?;
                let entry = self.make_entry(&inode, &mut tree)?;

                (file_id, entry)
            };

            let mut fh = self.fs.open(op_ctx, file_id, args.flags)?;

            if args.flags & O_TRUNC as u32 != 0 {
                fh.truncate(op_ctx)?;
            }

            let id = self.create_handle(fh);

            Ok((entry, Some(id), OpenOptions::empty(), None))
        })
    }

    fn read(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        w: &mut dyn ZeroCopyWriter,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _flags: u32,
    ) -> io::Result<usize> {
        self.with_context_and_handle_read(inode, handle, |op_ctx, fh| {
            fh.read(op_ctx, size as size_t, offset as off_t, w)
        })
    }

    fn write(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        r: &mut dyn ZeroCopyReader,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _delayed_write: bool,
        flags: u32,
        _fuse_flags: u32,
    ) -> io::Result<usize> {
        self.with_context_and_handle_write(inode, handle, |op_ctx, fh| {
            if flags & libc::O_APPEND as u32 != 0 {
                fh.append(op_ctx, size as size_t, r)
            } else {
                fh.write(op_ctx, size as size_t, offset as off_t, r)
            }
        })
    }

    fn flush(
        &self,
        _ctx: &Context,
        _inode: Self::Inode,
        handle: Self::Handle,
        _lock_owner: u64,
    ) -> io::Result<()> {
        if handle == 0 {
            return Ok(());
        }

        self.with_context(|op_ctx| {
            self.with_handle_mut(handle, |fh| fh.flush(op_ctx))?
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "Invalid handle (flush)")
                })?
        })
    }

    fn fsync(
        &self,
        _ctx: &Context,
        _inode: Self::Inode,
        datasync: bool,
        handle: Self::Handle,
    ) -> io::Result<()> {
        if handle == 0 {
            return Ok(());
        }

        self.with_context(|op_ctx| {
            self.with_handle_mut(handle, |fh| {
                if datasync {
                    fh.fsync_data(op_ctx)?;
                }
                fh.fsync_metadata(op_ctx)?;
                Ok(())
            })?
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid handle (fsync)"))?
        })
    }

    /// Our fallocate implementation does not really respect the standard
    /// because we have no intent of managing the actual layout of the disk
    /// and this is entirely a call to manage the layout of the dis. Best we
    /// can do is pretend the file is bigger (even though it isn't). We could
    /// probably zero-fill to "reserve" the space but that'd be long and stupid
    /// and I don't see any need to do this for the use-case.
    fn fallocate(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        mode: u32,
        offset: u64,
        length: u64,
    ) -> io::Result<()> {
        if mode != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unsupported fallocate mode",
            ));
        }

        let new_size = offset + length;

        self.with_context_and_handle(inode, Some(handle), 0, |op_ctx, fh| {
            // Get current size
            let current_size = fh.get_inode().make_entry()?.st_size as u64;

            // If the new size is larger than current size, extend the file
            if new_size > current_size {
                fh.set_size(op_ctx, new_size as off_t)?;
            }

            Ok(())
        })
    }

    fn release(
        &self,
        _ctx: &Context,
        _inode: Self::Inode,
        _flags: u32,
        handle: Self::Handle,
        flush: bool,
        _flock_release: bool,
        _lock_owner: Option<u64>,
    ) -> io::Result<()> {
        if handle == 0 {
            return Ok(());
        }

        // If flush is requested, flush the handle first
        if flush {
            self.with_context(|op_ctx| {
                self.with_handle_mut(handle, |fh| fh.flush(op_ctx))?
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid handle"))?
            })?;
        }

        // Remove the handle from our map
        self.remove_handle(handle);

        Ok(())
    }

    fn statfs(&self, _ctx: &Context, _inode: Self::Inode) -> io::Result<statvfs64> {
        self.with_context(|op_ctx| {
            let mut stat: statvfs64 = unsafe { mem::zeroed() };

            let block_size = self.fs.block_size()?;
            let used_storage = self.fs.estimate_used_storage()?;
            let free_storage = self.fs.estimate_free_storage()?;

            stat.f_bsize = block_size as c_ulong;
            stat.f_frsize = block_size as c_ulong;
            stat.f_blocks =
                (used_storage + free_storage + block_size as u64 - 1) / block_size as u64;
            stat.f_bfree = free_storage / block_size as u64;
            stat.f_bavail = stat.f_bfree;
            stat.f_files = self.fs.estimate_files_count(op_ctx)?;
            stat.f_ffree = self.fs.max_files_count(op_ctx)? - stat.f_files;
            stat.f_namemax = self.fs.get_name_max_size(op_ctx)?;

            Ok(stat)
        })
    }

    fn setxattr(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        name: &CStr,
        value: &[u8],
        flags: u32,
    ) -> io::Result<()> {
        const XATTR_CREATE: u32 = 1;
        const XATTR_REPLACE: u32 = 2;

        let name_str = name
            .to_str()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid UTF-8"))?;

        if name_str.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Empty xattr name",
            ));
        }

        if flags != 0 && flags != XATTR_CREATE && flags != XATTR_REPLACE {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid flags"));
        }

        self.with_context_and_handle(inode, None, libc::O_WRONLY as u32, |ctx, fh| {
            if flags == XATTR_CREATE {
                match fh.xattr_get(ctx, name_str) {
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "Attribute already exists",
                        ));
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        // This is expected, we can proceed
                    }
                    Err(e) => return Err(e),
                }
            }

            if flags == XATTR_REPLACE {
                match fh.xattr_get(ctx, name_str) {
                    Ok(_) => {
                        // This is expected, we can proceed
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        return Err(io::Error::from_raw_os_error(libc::ENODATA));
                    }
                    Err(e) => return Err(e),
                }
            }

            fh.xattr_set(ctx, name_str, value)
        })
    }

    fn getxattr(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        name: &CStr,
        size: u32,
    ) -> io::Result<GetxattrReply> {
        let name_str = name
            .to_str()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid UTF-8"))?;

        if name_str.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Name cannot be empty",
            ));
        }

        self.with_context_and_handle(inode, None, libc::O_RDONLY as u32, |ctx, fh| {
            match fh.xattr_get(ctx, name_str) {
                Ok(value) => {
                    if size == 0 {
                        Ok(GetxattrReply::Count(value.len() as u32))
                    } else if (size as usize) < value.len() {
                        Err(io::Error::from_raw_os_error(libc::ERANGE))
                    } else {
                        Ok(GetxattrReply::Value(value))
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    Err(io::Error::from_raw_os_error(libc::ENODATA))
                }
                Err(e) => Err(e),
            }
        })
    }

    fn listxattr(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        size: u32,
    ) -> io::Result<ListxattrReply> {
        self.with_context_and_handle(inode, None, libc::O_RDONLY as u32, |ctx, fh| {
            let names = fh.xattr_list(ctx)?;
            let total_size: usize = names.iter().map(|name| name.len() + 1).sum();

            if size == 0 {
                Ok(ListxattrReply::Count(total_size as u32))
            } else if (size as usize) < total_size {
                Err(io::Error::from_raw_os_error(libc::ENODATA))
            } else {
                let mut buffer = Vec::with_capacity(total_size);
                for name in names {
                    buffer.extend_from_slice(name.as_bytes());
                    buffer.push(0);
                }
                Ok(ListxattrReply::Names(buffer))
            }
        })
    }

    fn removexattr(&self, _ctx: &Context, inode: Self::Inode, name: &CStr) -> io::Result<()> {
        let name_str = name
            .to_str()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid UTF-8"))?;

        if name_str.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Empty xattr name",
            ));
        }

        self.with_context_and_handle(inode, None, libc::O_WRONLY as u32, |ctx, fh| {
            fh.xattr_remove(ctx, name_str)
        })
    }

    fn readdir(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        size: u32,
        offset: u64,
        add_entry: &mut dyn FnMut(DirEntry) -> io::Result<usize>,
    ) -> io::Result<()> {
        let entries = self.inner_readdir(ctx, inode, handle, size, offset)?;

        for (owned_dir_entry, _) in entries {
            if add_entry(owned_dir_entry.as_dir_entry())? == 0 {
                break;
            }
        }

        Ok(())
    }

    fn readdirplus(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        size: u32,
        offset: u64,
        add_entry: &mut dyn FnMut(DirEntry, Entry) -> io::Result<usize>,
    ) -> io::Result<()> {
        let entries = self.inner_readdir(ctx, inode, handle, size, offset)?;

        for (owned_dir_entry, entry) in entries {
            if owned_dir_entry.name != b"." && owned_dir_entry.name != b".." {
                self.increase_lookup(entry.inode, 1);
            }

            if add_entry(owned_dir_entry.as_dir_entry(), entry)? == 0 {
                break;
            }
        }

        Ok(())
    }

    fn fsyncdir(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        datasync: bool,
        handle: Self::Handle,
    ) -> io::Result<()> {
        self.fsync(ctx, inode, datasync, handle)
    }

    fn releasedir(
        &self,
        _ctx: &Context,
        _inode: Self::Inode,
        _flags: u32,
        handle: Self::Handle,
    ) -> io::Result<()> {
        if handle == 0 {
            return Ok(());
        }

        self.remove_handle(handle);

        Ok(())
    }
}
