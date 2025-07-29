# Flocon

A high-performance, SQLite-based filesystem designed specifically for containers
and modern deployment workflows.

## What is Flocon?

Flocon is an experimental filesystem that stores file data and metadata in
SQLite databases. Unlike traditional container filesystems that require
time-consuming extraction of layers, Flocon provides instant access to files by
leveraging SQLite's efficient query engine and built-in features like WAL
(Write-Ahead Logging) and integrity guarantees.

## Motivation

Current container technologies suffer from several inefficiencies:

-   **Slow extraction times**: Extracting container layers often takes longer
    than downloading them
-   **Large monolithic layers**: A small change in one package invalidates all
    subsequent layers
-   **Poor caching**: Changes early in the build process invalidate everything
    that follows

Flocon addresses these issues by:

-   **No extraction needed**: Files are instantly accessible through SQLite
    queries
-   **Fine-grained layering**: Each package or component can be its own tiny
    SQLite file
-   **Efficient reads**: Optimized for the common container workload of reading
    many small files
-   **Smart dependencies**: Build dependencies don't need to be shipped to
    production
-   **Simple schema**: Easy to write converters from existing package formats
    (Debian, Python, etc.)

## Current State

Flocon is in active development. The core engine is functional and supports a
wide range of POSIX features.

**Implemented Features:**

-   ✅ Basic filesystem operations (create, read, write, delete files and
    directories).
-   ✅ File metadata (permissions, ownership, timestamps).
-   ✅ Efficient block-based storage with automatic optimization for zero-filled
    blocks.
-   ✅ Transactional integrity for all operations via SQLite.
-   ✅ **FUSE mount support** for use on a host system.
-   ✅ **Virtio-fs vhost-user backend** for high-performance use with VMs (e.g.,
    QEMU, Firecracker).
-   ✅ **Extended attributes (xattr)** support.
-   ✅ **Symbolic links**.
-   ✅ **Special files** (e.g., device files via `mknod`).
-   ✅ Command-line tools for creating, mounting, and serving filesystems.

**Future Work:**

-   ❌ **Multi-layer support**: The main architectural goal is to support
    mounting multiple SQLite files as a single, unified filesystem, which is not
    yet implemented.

## Installation

### Building from Source

You'll need the Rust toolchain and standard build tools.

```bash
git clone https://github.com/Starkstonks/flocon
cd flocon
cargo build --release
```

The binary will be available at `target/release/flocon`.

## Usage

### 1. Creating a New Filesystem

Create a new Flocon filesystem image, which is a standard SQLite file.

```bash
# Create a new filesystem image with default permissions
./target/release/flocon mkfs myfilesystem.db

# With custom root directory permissions and ownership
./target/release/flocon mkfs myfilesystem.db --mode 755 --uid 1000 --gid 1000
```

### 2. Mounting a Filesystem (FUSE)

Mount the filesystem on your local machine using FUSE.

```bash
# Create a mount point
mkdir -p /mnt/flocon

# Mount the filesystem
./target/release/flocon mount myfilesystem.db /mnt/flocon

# Mount and run in the background (daemon mode)
./target/release/flocon mount myfilesystem.db /mnt/flocon --daemonize
```

The filesystem can be unmounted using standard tools
(`fusermount -u /mnt/flocon` on Linux) or by pressing `Ctrl+C` if running in the
foreground.

### 3. Exposing a Filesystem (Virtio-fs)

Expose the filesystem to a virtual machine over a vhost-user socket. This is
ideal for container runtimes or custom VM setups.

```bash
# Run the virtiofs server, listening on a socket
./target/release/flocon virtiofs myfilesystem.db /tmp/flocon.sock
```

You can then configure your VMM (e.g., QEMU) to connect to `/tmp/flocon.sock` to
provide the filesystem to the guest.

### Logging

Control the verbosity of logging output for any command:

```bash
# See detailed debug information
./target/release/flocon mount myfilesystem.db /mnt/flocon --log-level debug
```

Available log levels: `trace`, `debug`, `info`, `warn`, `error`.

## Architecture

Flocon is built on two key components: a generic filesystem abstraction and a
concrete SQLite implementation.

-   **WinterFS Abstraction**: A custom trait-based system (`WinterFs`,
    `WinterInode`, `WinterHandle`) that defines a clean interface for filesystem
    operations, separating the core logic from the FUSE or virtio-fs protocol
    details.
-   **SQLite Backend**: The storage is a SQLite database with a simple schema:
    -   `inode`: Stores file metadata (mode, UID, GID, timestamps, size).
    -   `block`: Stores actual file data in chunks. Zero-filled blocks are
        stored implicitly to save space.
    -   `link`: Implements the directory tree by linking parent inodes to child
        inodes with a name.
    -   `xattr`: Stores extended attributes for inodes.

All operations are wrapped in SQLite transactions, ensuring the filesystem
remains consistent. The database runs in `WAL` (Write-Ahead Logging) mode for
improved concurrency and performance.

## Performance Considerations

Flocon is optimized for:

-   Fast reads of many small files.
-   Efficient metadata lookups via SQLite indices.
-   Low storage overhead for sparse files or files with large zero-filled
    sections.

Trade-offs:

-   Write performance for large, sequential files may be slower than traditional
    filesystems due to the overhead of database transactions.
-   It is not yet optimized for workloads with very high write concurrency.

## Future Vision

The goal is to enable workflows like:

1. **Package-level granularity**: Each package (Debian, Python, npm) as its own
   Flocon layer
2. **Smart building**: Build dependencies available during compilation but not
   shipped to production
3. **Instant deployment**: Download only changed SQLite files and mount them
   together
4. **Efficient updates**: Update individual packages without rebuilding entire
   images

## Contributing

Flocon is an experimental project exploring new approaches to container
filesystems. Contributions, ideas, and feedback are highly welcome!

## Acknowledgments

Built with an amazing stack of Rust crates:

-   [fuse-backend-rs](https://github.com/cloud-hypervisor/fuse-backend-rs) for
    FUSE and virtio-fs integration.
-   [rusqlite](https://github.com/rusqlite/rusqlite) for direct,
    high-performance SQLite access.
-   [SQLite](https://www.sqlite.org/) as the rock-solid storage engine.
-   [Clap](https://crates.io/crates/clap) for powerful command-line argument
    parsing.
-   [Tracing](https://crates.io/crates/tracing) for structured, level-based
    logging.
-   Built with the [Rust](https://www.rust-lang.org/) 2024 edition.
