use anyhow::{Result, anyhow};
use chrono::Utc;
use diesel::Connection as _;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, CustomizeConnection, Pool, PooledConnection};
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use std::path::{Path, PathBuf};
use tracing::{debug, info};

use crate::models::NewInode;
use crate::schema::inode;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

/// Custom connection customizer that sets pragmas on each new connection
#[derive(Debug)]
struct SqliteConnectionCustomizer;

impl CustomizeConnection<SqliteConnection, diesel::r2d2::Error> for SqliteConnectionCustomizer {
    /// Our goal here is to configure properly the database features for our
    /// needs, especially in terms of guarantees and performance. Everything
    /// is static so far, maybe some values ought to be fine-tuned in the
    /// future.
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), diesel::r2d2::Error> {
        diesel::sql_query("PRAGMA journal_mode = WAL")
            .execute(conn)
            .map_err(|e| diesel::r2d2::Error::QueryError(e))?;

        diesel::sql_query("PRAGMA foreign_keys = ON")
            .execute(conn)
            .map_err(|e| diesel::r2d2::Error::QueryError(e))?;

        diesel::sql_query("PRAGMA auto_vacuum = INCREMENTAL")
            .execute(conn)
            .map_err(|e| diesel::r2d2::Error::QueryError(e))?;

        diesel::sql_query("PRAGMA synchronous = NORMAL")
            .execute(conn)
            .map_err(|e| diesel::r2d2::Error::QueryError(e))?;

        diesel::sql_query("PRAGMA temp_store = MEMORY")
            .execute(conn)
            .map_err(|e| diesel::r2d2::Error::QueryError(e))?;

        diesel::sql_query("PRAGMA mmap_size = 30000000000") // 30GB mmap
            .execute(conn)
            .map_err(|e| diesel::r2d2::Error::QueryError(e))?;

        diesel::sql_query("PRAGMA page_size = 4096")
            .execute(conn)
            .map_err(|e| diesel::r2d2::Error::QueryError(e))?;

        Ok(())
    }
}

pub struct FileSystemManager {
    pool: Pool<ConnectionManager<SqliteConnection>>,
    image_path: PathBuf,
}

impl FileSystemManager {
    /// Build a connection pool for the given SQLite image path
    ///
    /// Used during init
    fn setup_pool(image_path: &Path) -> Result<Pool<ConnectionManager<SqliteConnection>>> {
        let db_url = image_path
            .to_str()
            .ok_or_else(|| anyhow!("Invalid image path (not UTF-8)"))?;
        let manager = ConnectionManager::<SqliteConnection>::new(db_url);

        Pool::builder()
            .connection_customizer(Box::new(SqliteConnectionCustomizer))
            .build(manager)
            .map_err(|e| anyhow!("Failed to create DB pool: {}", e))
    }

    /// Run any pending Diesel migrations on this connection.
    ///
    /// Used during init
    fn run_migrations(&self, conn: &mut SqliteConnection) -> Result<()> {
        info!("Running database migrations...");
        conn.run_pending_migrations(MIGRATIONS)
            .map_err(|e| anyhow!(e))?;
        Ok(())
    }

    /// Ensure that the root inode (ID 1) exists, which is an expected
    /// requirement (yet not exactly covered in migrations, maybe it should
    /// go there instead right?)
    fn ensure_root_inode(
        &self,
        conn: &mut SqliteConnection,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<()> {
        let exists = inode::table
            .find(1)
            .select(inode::id)
            .first::<i32>(conn)
            .optional()?
            .is_some();

        if !exists {
            info!("Creating root inode...");
            self.create_root_inode(conn, mode, uid, gid)?;
        }
        Ok(())
    }

    /// Insert the root inode record into the database. Sub-branch of
    /// ensure_root_inode(), separated for readability.
    fn create_root_inode(
        &self,
        conn: &mut SqliteConnection,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<()> {
        let now = Utc::now();
        let full_mode = 0o40000 | mode;
        let new_root = NewInode {
            mode: full_mode as i32,
            uid: uid as i32,
            gid: gid as i32,
            size: 0,
            rdev: 0,
            atime: now.into(),
            mtime: now.into(),
            ctime: now.into(),
            btime: now.into(),
        };

        diesel::insert_into(inode::table)
            .values(&new_root)
            .execute(conn)?;
        debug!("Created root inode with mode {:o}", full_mode);
        Ok(())
    }

    /// Idempotent initialization: migrations + root inode in a transaction.
    fn init(&self, mode: u32, uid: u32, gid: u32) -> Result<()> {
        let mut conn = self.get_connection()?;

        conn.transaction::<_, anyhow::Error, _>(|conn| {
            self.run_migrations(conn)?;
            self.ensure_root_inode(conn, mode, uid, gid)?;
            Ok(())
        })?;

        Ok(())
    }

    /// Creates (or opens) the filesystem image, runs migrations,
    /// and ensures the root inode is present.
    pub fn new(image_path: &Path, mode: u32, uid: u32, gid: u32) -> Result<Self> {
        let pool = Self::setup_pool(image_path)?;
        let fs = Self {
            pool,
            image_path: image_path.to_path_buf(),
        };
        fs.init(mode, uid, gid)?;
        Ok(fs)
    }

    /// Retrieves a pooled connection for thread-safe DB access.
    pub fn get_connection(&self) -> Result<PooledConnection<ConnectionManager<SqliteConnection>>> {
        self.pool
            .get()
            .map_err(|e| anyhow!("Failed to get DB connection from pool: {}", e))
    }
}
