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

Flocon is in early development and currently supports:

-   ✅ Basic FUSE filesystem operations (create, read, write, delete files and
    directories)
-   ✅ File metadata (permissions, ownership, timestamps)
-   ✅ Efficient block-based storage with automatic deduplication of zero blocks
-   ✅ SQLite-based storage with transaction support
-   ✅ Command-line tools for creating and mounting filesystems

Not yet implemented:

-   ❌ Multi-layer support (combining multiple SQLite files)
-   ❌ Extended attributes (xattr)
-   ❌ Symbolic links
-   ❌ Special files (devices, sockets, FIFOs)
-   ❌ virtiofs server mode

## Installation

### Building from Source

```bash
git clone https://github.com/Starkstonks/flocon
cd flocon
cargo build --release
```

The binary will be available at `target/release/flocon`.

## Usage

### Creating a New Filesystem

```bash
# Create a new Flocon filesystem image
flocon mkfs myfilesystem.db

# With custom root directory permissions and ownership
flocon mkfs myfilesystem.db --mode 755 --uid 1000 --gid 1000
```

### Mounting a Filesystem

```bash
# Mount the filesystem
flocon mount myfilesystem.db /mnt/flocon

# With custom permissions
flocon mount myfilesystem.db /mnt/flocon --mode 755 --uid 1000 --gid 1000

# Run in background (daemon mode)
flocon mount myfilesystem.db /mnt/flocon --daemonize
```

### Unmounting

The filesystem can be unmounted using standard FUSE tools:

```bash
# Linux
fusermount -u /mnt/flocon

# macOS
umount /mnt/flocon
```

Or by pressing `Ctrl+C` if running in foreground mode.

### Logging

Control the verbosity of logging output:

```bash
flocon mount myfilesystem.db /mnt/flocon --log-level debug
```

Available log levels: `trace`, `debug`, `info`, `warn`, `error`

## Architecture

Flocon uses a simple but efficient schema:

-   **Inodes**: Store file metadata (permissions, timestamps, size)
-   **Blocks**: Store actual file data in 1MB chunks with automatic zero-block
    optimization
-   **Links**: Implement the directory structure (parent-child relationships)
-   **Extended attributes**: (Planned) Store additional metadata

The filesystem is transactional - all operations either complete successfully or
are rolled back entirely, ensuring consistency even in case of crashes.

## Performance Considerations

Flocon is optimized for:

-   Fast reads of small files (common in containers)
-   Efficient storage through block deduplication
-   Quick metadata operations through SQLite indices

Trade-offs:

-   Write performance may be slower than traditional filesystems
-   Not optimized for large sequential writes
-   Some POSIX features may have different performance characteristics

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
filesystems. Contributions, ideas, and feedback are welcome!

## Acknowledgments

Built with:

-   [fuse-backend-rs](https://github.com/cloud-hypervisor/fuse-backend-rs) for
    FUSE integration
-   [Diesel](https://diesel.rs/) for SQLite ORM
-   [SQLite](https://www.sqlite.org/) for the storage engine
