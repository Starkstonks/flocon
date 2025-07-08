use anyhow::{Result, anyhow};
use chrono::Utc;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use fuse_backend_rs::abi::fuse_abi::{CreateIn, OpenOptions, SetattrValid, stat64};
use fuse_backend_rs::api::filesystem::{
    Context, Entry, FileSystem, ZeroCopyReader, ZeroCopyWriter,
};
use std::ffi::CStr;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{debug, info};

use crate::models::{Inode, NewInode};
use crate::schema::inode;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

pub struct FileSystemManager {
    db: SqliteConnection,
    image_path: PathBuf,
}

impl FileSystemManager {
    /// Creates a new filesystem manager and initializes a new database
    pub fn new(image_path: &Path) -> Result<Self> {
        let db_url = image_path
            .to_str()
            .ok_or_else(|| anyhow!("Invalid image path (not UTF-8)"))?;
        let db = SqliteConnection::establish(db_url)?;
        Ok(Self {
            db,
            image_path: image_path.to_path_buf(),
        })
    }

    /// Opens an existing filesystem image
    pub fn open(image_path: &Path) -> Result<Self> {
        if !image_path.exists() {
            anyhow::bail!("Image file does not exist: {:?}", image_path);
        }
        let db_url = image_path
            .to_str()
            .ok_or_else(|| anyhow!("Invalid image path (not UTF-8)"))?;
        let db = SqliteConnection::establish(db_url)?;
        Ok(Self {
            db,
            image_path: image_path.to_path_buf(),
        })
    }

    /// Initializes a new filesystem with migrations and root inode
    pub fn initialize_filesystem(&mut self, mode: u32, uid: u32, gid: u32) -> Result<()> {
        info!("Running database migrations...");
        self.db
            .run_pending_migrations(MIGRATIONS)
            .map_err(|e| anyhow!(e))?;

        info!("Creating root inode...");
        self.create_root_inode(mode, uid, gid)?;

        Ok(())
    }

    /// Ensures the filesystem is initialized, running migrations if needed
    pub fn ensure_initialized(&mut self, mode: u32, uid: u32, gid: u32) -> Result<()> {
        self.db
            .run_pending_migrations(MIGRATIONS)
            .map_err(|e| anyhow!(e))?;

        // Check if root inode exists
        if !self.has_root_inode()? {
            info!("Creating root inode...");
            self.create_root_inode(mode, uid, gid)?;
        }

        Ok(())
    }

    /// Creates the root inode (inode 1)
    fn create_root_inode(&mut self, mode: u32, uid: u32, gid: u32) -> Result<()> {
        let now = Utc::now();
        let full_mode = 0o40000 | mode;

        let new_root_inode = NewInode {
            mode: full_mode as i32,
            uid: uid as i32,
            gid: gid as i32,
            size: 0,
            atime: now.into(),
            mtime: now.into(),
            ctime: now.into(),
            btime: now.into(),
        };

        diesel::insert_into(inode::table)
            .values(&new_root_inode)
            .execute(&mut self.db)?;

        debug!("Created root inode with mode {:o}", full_mode);
        Ok(())
    }

    /// Checks if the root inode exists
    fn has_root_inode(&mut self) -> Result<bool> {
        let root: Option<Inode> = inode::table
            .find(1)
            .select(Inode::as_select())
            .first(&mut self.db)
            .optional()?;
        Ok(root.is_some())
    }

    /// Get database connection (for external use)
    pub fn db(&mut self) -> &mut SqliteConnection {
        &mut self.db
    }
}

pub struct FloconFs;

impl FileSystem for FloconFs {
    type Inode = u64;
    type Handle = u64;
}
