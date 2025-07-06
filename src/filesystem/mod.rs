use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use fuse_backend_rs::abi::fuse_abi::{Attr, CreateIn, OpenOptions, ROOT_ID, SetattrValid, stat64};
use fuse_backend_rs::api::filesystem::{
    AsyncFileSystem, AsyncZeroCopyReader, AsyncZeroCopyWriter, Context, Entry, FileSystem,
};
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, EntityTrait, Set};
use sea_orm_migration::MigratorTrait;
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{debug, info};

use crate::entities::{self, inode};
use migration::Migrator;

pub struct FileSystemManager {
    db: DatabaseConnection,
    image_path: PathBuf,
}

impl FileSystemManager {
    /// Creates a new filesystem manager and initializes a new database
    pub async fn new(image_path: &Path) -> Result<Self> {
        let db_url = format!("sqlite://{}?mode=rwc", image_path.display());
        let db = Database::connect(&db_url).await?;

        Ok(Self {
            db,
            image_path: image_path.to_path_buf(),
        })
    }

    /// Opens an existing filesystem image
    pub async fn open(image_path: &Path) -> Result<Self> {
        if !image_path.exists() {
            anyhow::bail!("Image file does not exist: {:?}", image_path);
        }

        let db_url = format!("sqlite://{}", image_path.display());
        let db = Database::connect(&db_url).await?;

        Ok(Self {
            db,
            image_path: image_path.to_path_buf(),
        })
    }

    /// Initializes a new filesystem with migrations and root inode
    pub async fn initialize_filesystem(&mut self, mode: u32, uid: u32, gid: u32) -> Result<()> {
        info!("Running database migrations...");
        Migrator::up(&self.db, None).await?;

        info!("Creating root inode...");
        self.create_root_inode(mode, uid, gid).await?;

        Ok(())
    }

    /// Ensures the filesystem is initialized, running migrations if needed
    pub async fn ensure_initialized(&mut self, mode: u32, uid: u32, gid: u32) -> Result<()> {
        // Check if migrations need to be run
        let pending = Migrator::get_pending_migrations(&self.db).await?;
        if !pending.is_empty() {
            info!("Running {} pending migrations...", pending.len());
            Migrator::up(&self.db, None).await?;
        }

        // Check if root inode exists
        if !self.has_root_inode().await? {
            info!("Creating root inode...");
            self.create_root_inode(mode, uid, gid).await?;
        }

        Ok(())
    }

    /// Creates the root inode (inode 1)
    async fn create_root_inode(&self, mode: u32, uid: u32, gid: u32) -> Result<()> {
        let now = Utc::now();

        // Combine directory bit (S_IFDIR = 0o40000) with permissions
        let full_mode = 0o40000 | mode;

        let root_inode = inode::ActiveModel {
            id: Set(1), // Root inode is always ID 1
            mode: Set(full_mode as i32),
            uid: Set(uid as i32),
            gid: Set(gid as i32),
            size: Set(0),
            atime: Set(now),
            mtime: Set(now),
            ctime: Set(now),
            btime: Set(now),
        };

        root_inode.insert(&self.db).await?;
        debug!("Created root inode with mode {:o}", full_mode);

        Ok(())
    }

    /// Checks if the root inode exists
    async fn has_root_inode(&self) -> Result<bool> {
        let root = entities::Inode::find_by_id(1).one(&self.db).await?;
        Ok(root.is_some())
    }

    /// Get database connection (for external use)
    pub fn db(&self) -> &DatabaseConnection {
        &self.db
    }
}

pub struct FloconFs;

impl FileSystem for FloconFs {
    type Inode = u64;
    type Handle = u64;
}

#[async_trait]
impl AsyncFileSystem for FloconFs {
    async fn async_lookup(
        &self,
        ctx: &Context,
        parent: Self::Inode,
        name: &CStr,
    ) -> std::io::Result<Entry> {
        todo!()
    }

    async fn async_getattr(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Option<Self::Handle>,
    ) -> std::io::Result<(stat64, Duration)> {
        todo!()
    }

    async fn async_setattr(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        attr: stat64,
        handle: Option<Self::Handle>,
        valid: SetattrValid,
    ) -> std::io::Result<(stat64, Duration)> {
        todo!()
    }

    async fn async_open(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        flags: u32,
        fuse_flags: u32,
    ) -> std::io::Result<(Option<Self::Handle>, OpenOptions)> {
        todo!()
    }

    async fn async_create(
        &self,
        ctx: &Context,
        parent: Self::Inode,
        name: &CStr,
        args: CreateIn,
    ) -> std::io::Result<(Entry, Option<Self::Handle>, OpenOptions)> {
        todo!()
    }

    async fn async_read(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        w: &mut (dyn AsyncZeroCopyWriter + Send),
        size: u32,
        offset: u64,
        lock_owner: Option<u64>,
        flags: u32,
    ) -> std::io::Result<usize> {
        todo!()
    }

    async fn async_write(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        r: &mut (dyn AsyncZeroCopyReader + Send),
        size: u32,
        offset: u64,
        lock_owner: Option<u64>,
        delayed_write: bool,
        flags: u32,
        fuse_flags: u32,
    ) -> std::io::Result<usize> {
        todo!()
    }

    async fn async_fsync(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        datasync: bool,
        handle: Self::Handle,
    ) -> std::io::Result<()> {
        todo!()
    }

    async fn async_fallocate(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        mode: u32,
        offset: u64,
        length: u64,
    ) -> std::io::Result<()> {
        todo!()
    }

    async fn async_fsyncdir(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        datasync: bool,
        handle: Self::Handle,
    ) -> std::io::Result<()> {
        todo!()
    }
}
