use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use fuse_backend_rs::api::server::Server;
use fuse_backend_rs::transport::FuseSession;
use std::sync::Arc;
use std::{num::ParseIntError, path::PathBuf};
use tracing::{Level, error, info};
use tracing_subscriber;

mod entities;
mod filesystem;

use filesystem::{FileSystemManager, FloconFs};

#[derive(Parser)]
#[command(author, version, about, color = clap::ColorChoice::Auto)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Creates a new filesystem image
    Mkfs {
        /// Image file path
        #[arg(value_name = "IMAGE", value_parser = ensure_parent_exists)]
        image: PathBuf,

        /// Root dir permissions (octal, e.g. 755)
        #[arg(long, value_parser = parse_octal, default_value = "755")]
        mode: u32,

        /// Root dir owner UID
        #[arg(long, default_value_t = 0)]
        uid: u32,

        /// Root dir owner GID
        #[arg(long, default_value_t = 0)]
        gid: u32,

        /// Verbosity for application logging.
        #[arg(long, value_enum, default_value_t = LogLevel::Info)]
        log_level: LogLevel,
    },

    /// Mounts the specified DB into the specified location
    Mount {
        /// Image file path
        #[arg(value_name = "IMAGE", value_parser = ensure_parent_exists)]
        image: PathBuf,

        /// Mount point directory
        #[arg(value_name = "MOUNT_POINT", value_parser = parse_existing_dir)]
        mount_point: PathBuf,

        /// Root dir permissions (octal, e.g. 755)
        #[arg(long, value_parser = parse_octal, default_value = "755")]
        mode: u32,

        /// Root dir owner UID
        #[arg(long, default_value_t = 0)]
        uid: u32,

        /// Root dir owner GID
        #[arg(long, default_value_t = 0)]
        gid: u32,

        /// Verbosity for application logging.
        #[arg(long, value_enum, default_value_t = LogLevel::Info)]
        log_level: LogLevel,

        /// Run in background.
        #[arg(long, action, default_value_t = false)]
        daemonize: bool,
    },
}

#[derive(ValueEnum, Clone, Debug)]
enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl From<LogLevel> for Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => Level::TRACE,
            LogLevel::Debug => Level::DEBUG,
            LogLevel::Info => Level::INFO,
            LogLevel::Warn => Level::WARN,
            LogLevel::Error => Level::ERROR,
        }
    }
}

fn parse_octal(s: &str) -> Result<u32, String> {
    u32::from_str_radix(s, 8)
        .map_err(|_: ParseIntError| "MODE must be an octal number, e.g. 755".into())
}

fn ensure_parent_exists(s: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(s);
    match p.parent() {
        Some(parent) if parent.exists() => Ok(p),
        _ => Err("parent directory of IMAGE does not exist".into()),
    }
}

fn parse_existing_dir(s: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(s);
    if !path.exists() {
        return Err(format!("Path does not exist: {}", s));
    }
    if !path.is_dir() {
        return Err(format!("Path is not a directory: {}", s));
    }
    Ok(path)
}

/// Creates a file system image by creating a SQLite database initialized with
/// the right schema.
async fn flocon_mkfs(image: PathBuf, mode: u32, uid: u32, gid: u32) -> Result<()> {
    info!("Creating new filesystem image: {:?}", image);

    let mut fs_manager = FileSystemManager::new(&image).await?;
    fs_manager.initialize_filesystem(mode, uid, gid).await?;

    info!("Successfully created filesystem image");
    Ok(())
}

/// Mounts a flocon filesystem using FUSE on the local system
async fn flocon_mount(
    image: PathBuf,
    mount_point: PathBuf,
    mode: u32,
    uid: u32,
    gid: u32,
    daemonize: bool,
) -> Result<()> {
    info!("Mounting image: {:?}", image);
    info!("Mount point: {:?}", mount_point);
    info!("Mode: {:o}, UID: {}, GID: {}", mode, uid, gid);
    info!("Daemonize: {}", daemonize);

    let mut fs_manager = FileSystemManager::open(&image).await?;

    // Ensure filesystem is initialized (runs migrations if needed)
    fs_manager.ensure_initialized(mode, uid, gid).await?;

    let fs = FloconFs {};
    let fs_arc = Arc::new(fs);
    let server = Server::new(fs_arc);

    info!("Mounting filesystem at {:?}", mount_point);
    let mut session = FuseSession::new(mount_point.as_path(), "flocon", "", false)?;
    session.mount()?;

    let mut channel = session.new_channel()?;

    // Spawn the FUSE server thread
    let mut fuse_handle = tokio::task::spawn_blocking(move || {
        info!("Starting FUSE server");

        loop {
            match channel.get_request() {
                Ok(Some((reader, writer))) => {
                    if let Err(e) = server.handle_message(reader, writer.into(), None, None) {
                        error!("Error handling FUSE request: {:?}", e);
                    }
                }

                Ok(None) => {
                    info!("Fuse channel closed, exiting server loop");
                    break;
                }

                Err(e) => {
                    error!("Error reading FUSE request: {:?}", e);
                    break;
                }
            }
        }
    });

    // Create a task that waits for CTRL+C
    let mut ctrl_c_handle = tokio::spawn(async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for ctrl+c");
        info!("Received CTRL+C signal");
    });

    // Race between CTRL+C and FUSE thread completion
    tokio::select! {
        _ = &mut ctrl_c_handle => {
            info!("Shutdown requested, unmounting filesystem...");
            session.umount()?;

            // Now wait for FUSE thread to finish
            match fuse_handle.await {
                Ok(_) => info!("FUSE server stopped successfully"),
                Err(e) => error!("FUSE server join error: {:?}", e),
            }
        }

        result = &mut fuse_handle => {
            // FUSE thread finished on its own
            match result {
                Ok(_) => info!("FUSE server stopped"),
                Err(e) => error!("FUSE server error: {:?}", e),
            }

            // Try to unmount (might already be unmounted)
            let _ = session.umount();

            // Cancel the ctrl+c handler
            ctrl_c_handle.abort();
        }
    }

    info!("Filesystem unmounted");
    Ok(())
}

fn main() {
    let cli = Cli::parse();

    let rt = tokio::runtime::Runtime::new().unwrap();

    match cli.command {
        Commands::Mkfs {
            image,
            mode,
            uid,
            gid,
            log_level,
        } => {
            let level: Level = log_level.into();
            tracing_subscriber::fmt().with_max_level(level).init();

            if let Err(e) = rt.block_on(flocon_mkfs(image, mode, uid, gid)) {
                error!("Failed to create filesystem: {}", e);
                std::process::exit(1);
            }
        }
        Commands::Mount {
            image,
            mount_point,
            mode,
            uid,
            gid,
            log_level,
            daemonize,
        } => {
            let level: Level = log_level.into();
            tracing_subscriber::fmt().with_max_level(level).init();

            if let Err(e) = rt.block_on(flocon_mount(image, mount_point, mode, uid, gid, daemonize))
            {
                error!("Failed to mount filesystem: {}", e);
                std::process::exit(1);
            }
        }
    }
}
