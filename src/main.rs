use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use fuse_backend_rs::api::server::Server;
use fuse_backend_rs::transport::FuseSession;
use nix::mount::MsFlags;
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::Duration;
use std::{num::ParseIntError, path::PathBuf, thread};
use tracing::{Level, error, info};
use tracing_subscriber;

mod filesystem;
mod virtiofs;

use crate::filesystem::{FileSystemManager, Flocon, WinterFsHandler};
use crate::virtiofs::run_virtiofs_daemon; // Import the daemon runner

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
        #[arg(value_name = "IMAGE", value_parser = parse_existing_file)]
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

    /// Exposes the filesystem via virtio-fs vhost-user.
    Virtiofs {
        /// Image file path
        #[arg(value_name = "IMAGE", value_parser = parse_existing_file)]
        image: PathBuf,

        /// vhost-user socket path
        #[arg(value_name = "SOCKET")]
        socket: PathBuf,

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
    if let Some(parent) = p.parent() {
        if parent.is_dir() || parent.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    Err("parent directory of IMAGE does not exist".into())
}

fn parse_existing_file(s: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(s);
    if !path.exists() {
        return Err(format!("Path does not exist: {}", s));
    }
    if !path.is_file() {
        return Err(format!("Path is not a file: {}", s));
    }
    Ok(path)
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
fn flocon_mkfs(image: PathBuf, mode: u32, uid: u32, gid: u32) -> Result<()> {
    info!("Creating new filesystem image: {:?}", image);

    FileSystemManager::new(&image, mode, uid, gid)?;

    info!("Successfully created filesystem image");
    Ok(())
}

/// Mounts a flocon filesystem using FUSE on the local system
fn flocon_mount(
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

    let fs_manager = FileSystemManager::new(&image, mode, uid, gid)?;
    let fs = WinterFsHandler::new(Flocon::new(fs_manager, &image));
    let fs_arc = Arc::new(fs);
    let server = Server::new(fs_arc);

    info!("Mounting filesystem at {:?}", mount_point);
    let mut session = FuseSession::new(
        mount_point.as_path(),
        image.to_str().unwrap(),
        "flocon",
        false,
    )?;
    session.set_mount_flags(MsFlags::MS_NOATIME);
    session.mount()?;

    let (tx, rx) = channel();
    ctrlc::set_handler(move || {
        tx.send(()).expect("Could not send signal on channel.");
    })?;

    let mut channel = session.new_channel()?;

    let fuse_handle = thread::spawn(move || {
        info!("Starting FUSE server");
        loop {
            match channel.get_request() {
                Ok(Some((reader, writer))) => {
                    if let Err(e) = server.handle_message(reader, writer.into(), None, None) {
                        error!("Error handling FUSE request: {:?}", e);
                    }
                }
                Ok(None) => {
                    info!("FUSE channel closed, exiting server loop");
                    break;
                }
                Err(e) => {
                    error!("Error reading FUSE request: {:?}", e);
                    break;
                }
            }
        }
        info!("FUSE server stopped");
    });

    info!("Waiting for Ctrl-C to unmount...");
    rx.recv()?;

    info!("Shutdown requested, unmounting filesystem...");
    session.umount()?;

    match fuse_handle.join() {
        Ok(_) => info!("FUSE server thread finished cleanly"),
        Err(e) => error!("FUSE server thread panicked: {:?}", e),
    }

    info!("Filesystem unmounted");
    Ok(())
}

fn flocon_virtiofs(image: PathBuf, socket: PathBuf, mode: u32, uid: u32, gid: u32) -> Result<()> {
    info!("Exposing filesystem via virtiofs:");
    info!("  Image: {:?}", image);
    info!("  Socket: {:?}", socket);
    info!("  Mode: {:o}, UID: {}, GID: {}", mode, uid, gid);

    let fs_manager = FileSystemManager::new(&image, mode, uid, gid)?;
    let fs = WinterFsHandler::new(Flocon::new(fs_manager, &image));
    let fs_arc = Arc::new(fs);
    let server = Arc::new(Server::new(fs_arc));

    let (tx, rx) = channel();
    ctrlc::set_handler(move || {
        tx.send(()).expect("Could not send signal on channel.");
    })?;

    let virtiofs_handle = run_virtiofs_daemon(socket, server)?;

    info!("Virtiofs daemon started. Waiting for Ctrl-C to stop...");
    rx.recv()?;

    info!("Shutdown requested, stopping virtiofs daemon...");
    // The daemon thread will be terminated when the main process exits.
    // For a more graceful shutdown, you might need to implement a mechanism
    // to signal the daemon thread to stop, for example by using its kill_evt.
    // For now, we'll just wait for it briefly.
    // Note: A robust implementation would involve a more graceful shutdown mechanism.
    if let Err(e) = virtiofs_handle.join_timeout(Duration::from_secs(1)) {
        error!("Virtiofs daemon thread did not exit cleanly: {:?}", e);
    }

    info!("Virtiofs daemon stopped.");
    Ok(())
}

// Extension trait to allow joining with a timeout.
trait JoinTimeout {
    fn join_timeout(self, timeout: Duration) -> std::result::Result<(), String>;
}

impl<T> JoinTimeout for thread::JoinHandle<T> {
    fn join_timeout(self, timeout: Duration) -> std::result::Result<(), String> {
        let _handle_thread = self.thread().clone();
        let start = std::time::Instant::now();
        while !self.is_finished() {
            if start.elapsed() > timeout {
                return Err("timed out".to_string());
            }
            thread::yield_now();
            thread::sleep(Duration::from_millis(100));
        }

        self.join()
            .map_err(|e| format!("thread panicked: {:?}", e))?;
        Ok(())
    }
}

fn main() {
    let cli = Cli::parse();

    let log_level = match &cli.command {
        Commands::Mkfs { log_level, .. } => log_level.clone(),
        Commands::Mount { log_level, .. } => log_level.clone(),
        Commands::Virtiofs { log_level, .. } => log_level.clone(),
    };

    let level: Level = log_level.into();
    tracing_subscriber::fmt().with_max_level(level).init();

    let result = match cli.command {
        Commands::Mkfs {
            image,
            mode,
            uid,
            gid,
            ..
        } => flocon_mkfs(image, mode, uid, gid),

        Commands::Mount {
            image,
            mount_point,
            mode,
            uid,
            gid,
            daemonize,
            ..
        } => flocon_mount(image, mount_point, mode, uid, gid, daemonize),
        Commands::Virtiofs {
            image,
            socket,
            mode,
            uid,
            gid,
            ..
        } => flocon_virtiofs(image, socket, mode, uid, gid),
    };

    if let Err(e) = result {
        error!("Operation failed: {:#}", e);
        std::process::exit(1);
    }
}
