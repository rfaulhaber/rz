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
| tar+xz    | `.tar.xz`, `.txz`     | LZMA2            | `lzma-rs` / `xz2` |
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

# Extract only matching files (--include glob or positional paths)
rz decompress project.tar.gz --include '*.rs'
rz decompress release.tar.gz src/main.rs

# Extract a file to stdout
rz decompress release.tar.gz -O config.toml

# Read archive from stdin (tar-based formats; requires --format)
cat mydir.tar.gz | rz decompress - -f tar-gz -o /tmp/out

# Overwrite existing files
rz decompress mydir.tar.gz -F

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
```

### Global options

| Flag               | Description                              |
|--------------------|------------------------------------------|
| `-p`, `--progress` | Show a progress bar                      |
| `-v`, `--verbose`  | Print each entry name to stderr          |
| `-V`, `--version`  | Print version                            |
| `-h`, `--help`     | Print help                               |

## Feature flags

| Feature  | Effect                                             |
|----------|----------------------------------------------------|
| `xz2`   | Use C-backed liblzma for xz (streaming, faster)    |
| `bzip2` | Enable tar.bz2 support via C-backed libbz2         |

Without these features, xz uses the pure-Rust `lzma-rs` crate (buffers the
archive in memory before compressing) and bzip2 is unavailable.

## Known limitations

- **tar.zst compression level**: The pure-Rust `ruzstd` encoder currently only
  supports a single compression level (`Fastest`, roughly zstd level 1).
  `--level 0` selects uncompressed framing; all other values use `Fastest`.
- **tar.xz / tar.zst memory usage**: Without the `xz2` feature, xz and zstd
  compression buffers the entire tar archive in memory before compressing.
  For large archives, consider enabling the `xz2` feature for streaming xz.
- **7z format**: Does not support `--strip-components`. Extracts contents
  flat (does not preserve the top-level directory wrapper).
- **Stdin/stdout streaming**: Only tar-based formats support `-` for
  stdin/stdout. ZIP and 7z require seekable I/O and will error.
- **Symlinks**: Followed during compression rather than stored as symlinks.

## License

[GPL-3.0-only](LICENSE)
