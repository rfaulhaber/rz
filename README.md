# rz

A multi-format archive tool written in Rust. Compress, decompress, test, list,
and inspect tar, zip, and 7z archives through a single, consistent CLI — no
more remembering whether it's `tar xzf`, `unzip`, or `7z x`.

## Supported formats

| Format    | Extension(s)          | Compression      | Backend           |
|-----------|-----------------------|------------------|-------------------|
| tar       | `.tar`                | none             | `tar`             |
| tar+gzip  | `.tar.gz`, `.tgz`     | gzip             | `flate2`          |
| tar+zstd  | `.tar.zst`, `.tzst`   | Zstandard        | `ruzstd`          |
| tar+xz    | `.tar.xz`, `.txz`     | LZMA2            | `lzma-rust2` / `xz2` |
| tar+bzip2 | `.tar.bz2`, `.tbz2`   | bzip2            | `bzip2` (opt-in)  |
| zip       | `.zip`                | Deflate          | `zip`             |
| 7z        | `.7z`                 | LZMA2            | `sevenz-rust2`    |

Format is auto-detected from the output extension (compress) or magic bytes
then extension (decompress/list/info). You can override with `--format`.

## Installation

### From source

```sh
cargo install --path .
```

### With optional C-backed codecs

The default build is pure Rust. For faster xz and bzip2 via C libraries:

```sh
cargo install --path . --features xz2,bzip2
```

### Nix

```sh
nix build              # default (pure Rust)
nix build .#with-xz2   # with C-backed xz
nix build .#with-xz2-bzip2
```

Or run directly:

```sh
nix run . -- compress mydir -o mydir.tar.gz
```

### Prebuilt binaries

Download from the [releases page](https://codeberg.org/ryf/rz/releases).
Binaries are provided for Linux (x86_64, aarch64), macOS (x86_64, aarch64),
and Windows (x86_64).

## Usage

### Compress

```sh
# Compress a directory (format inferred from extension)
rz compress mydir -o mydir.tar.gz

# Compress multiple inputs
rz compress src/ Cargo.toml -o project.zip

# Explicit format, custom compression level
rz compress mydir -o archive -f tar-gz -l 9

# Exclude patterns (glob, repeatable)
rz compress mydir -o mydir.tar.gz --exclude '*.log' --exclude node_modules

# Read exclude patterns from a file (one per line)
rz compress mydir -o mydir.tar.gz --exclude-from .archiveignore

# Read input file list from a file (one path per line)
rz compress -T filelist.txt -o bundle.tar.gz

# Exclude version-control directories and backup files
rz compress mydir -o mydir.tar.gz --exclude-vcs --exclude-backups

# Respect .gitignore rules
rz compress mydir -o mydir.tar.gz --exclude-vcs-ignores

# By default, symlinks are stored as symlinks (tar-family and zip).
# Use -H / --follow-symlinks to archive the target's content instead.
rz compress mydir -o mydir.tar.gz -H

# Do not recurse into directories
rz compress mydir -o mydir.tar.gz --no-recursion

# Dry run: show what would be compressed without creating an archive
rz compress mydir -o mydir.tar.gz -n

# Print total bytes processed at the end
rz compress mydir -o mydir.tar.gz --totals

# Compress to stdout (tar-based formats; requires --format)
rz compress mydir -o - -f tar-gz | ssh host 'rz d - -f tar-gz'

# Short alias
rz c mydir -o mydir.tar.zst
```

### Decompress

```sh
# Extract to current directory
rz decompress mydir.tar.gz

# Extract to a specific directory
rz decompress mydir.tar.gz -o /tmp/out

# Strip the top-level directory wrapper
rz decompress mydir.tar.gz --strip-components 1

# Exclude files during extraction
rz decompress mydir.zip --exclude '*.test.js'

# Read exclude patterns from a file (one per line)
rz decompress mydir.tar.gz --exclude-from .archiveignore

# Extract only matching files (--include glob or positional paths)
rz decompress project.tar.gz --include '*.rs'
rz decompress release.tar.gz src/main.rs

# Extract a file to stdout
rz decompress release.tar.gz -O config.toml

# Read archive from stdin (tar-based formats; requires --format)
cat mydir.tar.gz | rz decompress - -f tar-gz -o /tmp/out

# Overwrite existing files
rz decompress mydir.tar.gz -F

# Skip existing files silently instead of erroring
rz decompress mydir.tar.gz --no-overwrite

# Only extract entries newer than existing files on disk
rz decompress mydir.tar.gz -u

# Flatten directory structure (extract all files into output dir)
rz decompress mydir.tar.gz -j

# Backup existing files instead of overwriting (appends .bak)
rz decompress mydir.tar.gz --backup
rz decompress mydir.tar.gz --suffix .orig

# Restore original file permissions from archive metadata
rz decompress mydir.tar.gz -P

# Dry run: show what would be extracted without writing to disk
rz decompress mydir.tar.gz -n

# Print total bytes processed at the end
rz decompress mydir.tar.gz --totals

# Short alias
rz d mydir.tar.gz
```

### List

```sh
# List archive contents
rz list mydir.tar.gz

# Detailed output (permissions, sizes)
rz list mydir.zip -l

# Filter listing
rz list mydir.tar.gz --exclude '*.log'

# Read exclude patterns from a file
rz list mydir.tar.gz --exclude-from .archiveignore

# Sort entries
rz list mydir.tar.gz --sort name
rz list mydir.tar.gz --sort size
rz list mydir.tar.gz --sort date

# Human-readable sizes
rz list mydir.tar.gz -l --human-readable

# Short alias
rz ls mydir.tar.gz
```

### Test

```sh
# Verify archive integrity (fully decompress without writing to disk)
rz test mydir.tar.gz
# ok

# Short alias
rz t mydir.tar.gz
```

### Info

```sh
# Show archive metadata
rz info mydir.tar.gz
# Format:       tar.gz
# Entries:      42
# Compressed:   1048576 bytes
# Uncompressed: 3145728 bytes

# Human-readable sizes
rz info mydir.tar.gz --human-readable
```

### Global options

| Flag               | Description                              |
|--------------------|------------------------------------------|
| `-p`, `--progress` | Show a progress bar                      |
| `-v`, `--verbose`  | Print each entry name to stderr          |
| `-q`, `--quiet`    | Suppress all non-error output            |
| `-V`, `--version`  | Print version                            |
| `-h`, `--help`     | Print help                               |

## Feature flags

| Feature  | Effect                                             |
|----------|----------------------------------------------------|
| `xz2`   | Use C-backed liblzma for xz (typically faster)     |
| `bzip2` | Enable tar.bz2 support via C-backed libbz2         |

Without these features, xz uses the pure-Rust `lzma-rust2` crate, which
streams in both directions; bzip2 is unavailable.

## Known limitations

- **tar.zst compression level**: The pure-Rust `ruzstd` encoder currently only
  supports a single compression level (`Fastest`, roughly zstd level 1).
  `--level 0` selects uncompressed framing; all other values use `Fastest`.
- **tar.zst memory usage**: The pure-Rust `ruzstd` encoder buffers the entire
  tar archive in memory before compressing. For large archives this can use
  significant RAM.
- **7z format**: Does not support `--strip-components`. Extracts contents
  flat (does not preserve the top-level directory wrapper).
- **Stdin/stdout streaming**: Only tar-based formats support `-` for
  stdin/stdout. ZIP and 7z require seekable I/O and will error.
- **Symlinks**: tar-family and zip store symlinks as links by default; pass
  `-H` / `--follow-symlinks` to archive the target's content instead. The zip
  extractor in `rz` does not yet recreate stored links on disk (they extract
  as text files containing the target path); tar does. 7z follows symlinks
  unconditionally on compress — a limitation of the `sevenz-rust2` backend.
- **Zip list -l**: Per-entry sizes and permissions are not shown for zip archives
  in long-listing mode. The upstream `zip` crate does not expose central
  directory metadata without seeking to each entry's local file header, which
  would negate the performance benefit of reading only the central directory.
  Listing zip entry names (without `-l`) is instant regardless of archive size.

## License

[GPL-3.0-only](LICENSE)
