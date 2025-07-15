use anyhow::{Result, anyhow};
use chrono::{SecondsFormat, Utc};
use libc::S_IFDIR;
use r2d2::{CustomizeConnection, Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, Error, params};
use rusqlite_migration::{M, Migrations};
use std::path::Path;
use tracing::{debug, info};

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(include_str!(
        "../../migrations/2025-07-05-000001_create_tables/up.sql"
    ))])
}

/// Custom connection customizer that sets pragmas on each new connection
#[derive(Debug)]
struct SqliteConnectionCustomizer;

impl CustomizeConnection<Connection, Error> for SqliteConnectionCustomizer {
    /// Our goal here is to configure properly the database features for our
    /// needs, especially in terms of guarantees and performance. Everything
    /// is static so far, maybe some values ought to be fine-tuned in the
    /// future.
    fn on_acquire(&self, conn: &mut Connection) -> Result<(), Error> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            PRAGMA auto_vacuum = INCREMENTAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA temp_store = MEMORY;
            PRAGMA mmap_size = 30000000000;
            PRAGMA page_size = 4096;
            "#,
        )?;

        Ok(())
    }
}

pub struct FileSystemManager {
    pool: Pool<SqliteConnectionManager>,
}

impl FileSystemManager {
    /// Build a connection pool for the given SQLite image path
    ///
    /// Used during init
    fn setup_pool(image_path: &Path) -> Result<Pool<SqliteConnectionManager>> {
        let db_url = image_path
            .to_str()
            .ok_or_else(|| anyhow!("Invalid image path (not UTF-8)"))?;
        let manager = SqliteConnectionManager::file(db_url);

        Pool::builder()
            .connection_customizer(Box::new(SqliteConnectionCustomizer))
            .build(manager)
            .map_err(|e| anyhow!("Failed to create DB pool: {}", e))
    }

    /// Run any pending migrations on this connection.
    ///
    /// Used during init
    fn run_migrations(&self, conn: &mut Connection) -> Result<()> {
        info!("Running database migrations...");
        migrations().to_latest(conn)?;
        Ok(())
    }

    /// Ensure that the root inode (ID 1) exists, which is an expected
    /// requirement (yet not exactly covered in migrations, maybe it should
    /// go there instead right?)
    fn ensure_root_inode(
        &self,
        tx: &mut rusqlite::Transaction,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<()> {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM inode WHERE id = ?1)",
            params![1],
            |row| row.get(0),
        )?;

        if !exists {
            info!("Creating root inode...");
            self.create_root_inode(tx, mode, uid, gid)?;
        }
        Ok(())
    }

    /// Insert the root inode record into the database. Sub-branch of
    /// ensure_root_inode(), separated for readability.
    fn create_root_inode(
        &self,
        conn: &mut rusqlite::Transaction,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
        let full_mode = S_IFDIR | mode;

        conn.execute(
            "INSERT INTO inode (id, mode, uid, gid, size, rdev, atime, mtime, ctime, btime) \
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                1,
                full_mode as i32,
                uid as i32,
                gid as i32,
                0i64,
                0i32,
                &now,
                &now,
                &now,
                &now,
            ],
        )?;

        debug!("Created root inode with mode {:o}", full_mode);
        Ok(())
    }

    /// Idempotent initialization: migrations + root inode in a transaction.
    fn init(&self, mode: u32, uid: u32, gid: u32) -> Result<()> {
        let mut conn = self.get_connection()?;
        let tx = conn.transaction()?;

        {
            // We need to run migrations before the transaction since migrations
            // need their own transaction management
            drop(tx);
            self.run_migrations(&mut conn)?;

            // Now create a new transaction for the root inode
            let mut tx = conn.transaction()?;
            self.ensure_root_inode(&mut tx, mode, uid, gid)?;
            tx.commit()?;
        }

        Ok(())
    }

    /// Creates (or opens) the filesystem image, runs migrations,
    /// and ensures the root inode is present.
    pub fn new(image_path: &Path, mode: u32, uid: u32, gid: u32) -> Result<Self> {
        let pool = Self::setup_pool(image_path)?;
        let fs = Self { pool };
        fs.init(mode, uid, gid)?;
        Ok(fs)
    }

    /// Retrieves a pooled connection for thread-safe DB access.
    pub fn get_connection(&self) -> Result<PooledConnection<SqliteConnectionManager>> {
        self.pool
            .get()
            .map_err(|e| anyhow!("Failed to get DB connection from pool: {}", e))
    }
}
