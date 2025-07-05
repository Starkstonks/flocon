use clap::{Parser, Subcommand, ValueEnum};
use std::{num::ParseIntError, path::PathBuf};
use tracing::{Level, info};
use tracing_subscriber;

#[derive(Parser)]
#[command(author, version, about, color = clap::ColorChoice::Auto)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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

async fn flocon_mount(
    image: PathBuf,
    mount_point: PathBuf,
    mode: u32,
    uid: u32,
    gid: u32,
    daemonize: bool,
) {
    // Example log messages to demonstrate it's working
    info!("Mounting image: {:?}", image);
    info!("Mount point: {:?}", mount_point);
    info!("Mode: {:o}, UID: {}, GID: {}", mode, uid, gid);
    info!("Daemonize: {}", daemonize);

    // stub: your async mount logic here
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Mount {
            image,
            mount_point,
            mode,
            uid,
            gid,
            log_level,
            daemonize,
        } => {
            // Convert LogLevel to tracing::Level
            let level: Level = log_level.into();

            // Initialize the tracing subscriber
            tracing_subscriber::fmt().with_max_level(level).init();

            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(flocon_mount(image, mount_point, mode, uid, gid, daemonize));
        }
    }
}
