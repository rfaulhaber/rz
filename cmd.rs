use camino::Utf8PathBuf;
use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

#[derive(Debug, Parser)]
#[command(
    name = "rz",
    version,
    about = "Multi-format compression and decompression tool"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Show a progress bar
    #[arg(short, long, global = true, conflicts_with = "quiet")]
    pub progress: bool,

    /// Print each entry name to stderr as it is processed
    #[arg(short, long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Suppress all non-error output
    #[arg(short, long, global = true)]
    pub quiet: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Compress files or directories
    #[command(alias = "c")]
    Compress {
        /// Input path(s)
        #[arg(required_unless_present = "files_from")]
        input: Vec<Utf8PathBuf>,

        /// Output file (inferred if omitted)
        #[arg(short, long)]
        output: Option<Utf8PathBuf>,

        /// Format (inferred from output extension if omitted)
        #[arg(short, long)]
        format: Option<Format>,

        /// Compression level (format-dependent)
        #[arg(short, long, conflicts_with = "store")]
        level: Option<u32>,

        /// Store without compression (equivalent to --level 0)
        #[arg(short = '0', long)]
        store: bool,

        /// Exclude files matching a glob pattern (repeatable)
        #[arg(long)]
        exclude: Vec<String>,

        /// Read exclude patterns from a file (one per line)
        #[arg(long)]
        exclude_from: Vec<Utf8PathBuf>,

        /// Read input file list from a file (one path per line)
        #[arg(short = 'T', long)]
        files_from: Option<Utf8PathBuf>,

        /// Exclude version-control directories (.git, .hg, .svn, etc.)
        #[arg(long)]
        exclude_vcs: bool,

        /// Exclude backup files (*~, *.bak, #*#, .#*)
        #[arg(long)]
        exclude_backups: bool,

        /// Follow symlinks (archive target content instead of the link)
        #[arg(short = 'H', long)]
        follow_symlinks: bool,

        /// Print total bytes processed at the end
        #[arg(long)]
        totals: bool,

        /// Respect .gitignore rules when compressing
        #[arg(long)]
        exclude_vcs_ignores: bool,

        /// Do not recurse into directories
        #[arg(long)]
        no_recursion: bool,

        /// Show what would be compressed without creating an archive
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Override mtime on all entries (unix timestamp, e.g. 0 for epoch)
        #[arg(long)]
        mtime: Option<u64>,

        /// Override owner UID on all entries (e.g. 0 for root)
        #[arg(long)]
        owner: Option<u64>,

        /// Override group GID on all entries (e.g. 0 for root)
        #[arg(long)]
        group: Option<u64>,

        /// Override permission mode on all entries (octal, e.g. 644)
        #[arg(long, value_parser = parse_octal_mode)]
        mode: Option<u32>,

        /// Include only entries with mtime strictly newer than DATE
        /// (RFC 3339, `YYYY-MM-DD`, or `@<unix-seconds>`; tar-family only)
        #[arg(long, value_name = "DATE", value_parser = parse_date)]
        newer_than: Option<i64>,

        /// Include only entries with mtime strictly older than DATE (tar-family only)
        #[arg(long, value_name = "DATE", value_parser = parse_date)]
        older_than: Option<i64>,
    },

    /// Decompress an archive
    #[command(alias = "d")]
    Decompress {
        /// Input archive
        input: Utf8PathBuf,

        /// Output directory (default: current dir)
        #[arg(short, long)]
        output: Option<Utf8PathBuf>,

        /// Format (inferred from extension/magic bytes if omitted)
        #[arg(short, long)]
        format: Option<Format>,

        /// Overwrite existing files
        #[arg(short = 'F', long, conflicts_with_all = ["no_overwrite", "keep_newer"])]
        force: bool,

        /// Skip existing files silently instead of erroring
        #[arg(long, conflicts_with = "keep_newer")]
        no_overwrite: bool,

        /// Only extract entries newer than existing files on disk
        #[arg(short = 'u', long)]
        keep_newer: bool,

        /// Flatten directory structure (extract all files into output dir)
        #[arg(short = 'j', long)]
        no_directory: bool,

        /// Write extracted file contents to stdout instead of disk
        #[arg(short = 'O', long)]
        to_stdout: bool,

        /// Strip N leading path components during extraction
        #[arg(long, default_value_t = 0)]
        strip_components: u32,

        /// Exclude entries matching a glob pattern (repeatable)
        #[arg(long)]
        exclude: Vec<String>,

        /// Read exclude patterns from a file (one per line)
        #[arg(long)]
        exclude_from: Vec<Utf8PathBuf>,

        /// Include only entries matching a glob pattern (repeatable)
        #[arg(long)]
        include: Vec<String>,

        /// Print total bytes processed at the end
        #[arg(long)]
        totals: bool,

        /// Rename existing files instead of overwriting (appends .bak by default)
        #[arg(long, conflicts_with_all = ["force", "no_overwrite", "keep_newer"])]
        backup: bool,

        /// Suffix for backup files (implies --backup, default: .bak)
        #[arg(long, conflicts_with_all = ["force", "no_overwrite", "keep_newer"])]
        suffix: Option<String>,

        /// Restore original file permissions from archive metadata
        #[arg(short = 'P', long)]
        preserve_permissions: bool,

        /// Restore original owner/group (Unix + root only)
        #[arg(long, visible_alias = "numeric-owner")]
        same_owner: bool,

        /// Extract only entries with mtime strictly newer than DATE
        /// (RFC 3339, `YYYY-MM-DD`, or `@<unix-seconds>`)
        #[arg(long, value_name = "DATE", value_parser = parse_date)]
        newer_than: Option<i64>,

        /// Extract only entries with mtime strictly older than DATE
        #[arg(long, value_name = "DATE", value_parser = parse_date)]
        older_than: Option<i64>,

        /// Show what would be extracted without writing to disk
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Extract only these specific paths from the archive
        paths: Vec<String>,
    },

    /// List archive contents
    #[command(alias = "ls")]
    List {
        input: Utf8PathBuf,

        #[arg(short, long)]
        format: Option<Format>,

        /// Show detailed info (size, date, permissions)
        #[arg(short, long)]
        long: bool,

        /// Exclude entries matching a glob pattern (repeatable)
        #[arg(long)]
        exclude: Vec<String>,

        /// Read exclude patterns from a file (one per line)
        #[arg(long)]
        exclude_from: Vec<Utf8PathBuf>,

        /// Sort entries by field
        #[arg(long)]
        sort: Option<SortField>,

        /// Show sizes in human-readable format (KB, MB, GB)
        #[arg(long)]
        human_readable: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Test archive integrity (fully decompress without writing to disk)
    #[command(alias = "t")]
    Test {
        input: Utf8PathBuf,

        #[arg(short, long)]
        format: Option<Format>,
    },

    /// Show archive metadata
    Info {
        input: Utf8PathBuf,

        #[arg(short, long)]
        format: Option<Format>,

        /// Show sizes in human-readable format (KB, MB, GB)
        #[arg(long)]
        human_readable: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// List supported archive formats
    Formats {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Generate shell completions
    Completions {
        /// Target shell
        shell: Shell,
    },

    /// Generate a man page
    Man,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Zip,
    Tar,    // tar (no compression)
    TarGz,  // tar + gzip
    TarZst, // tar + zstd
    TarXz,  // tar + xz
    TarBz2, // tar + bzip2
    SevenZ, // 7z
}

#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
pub enum SortField {
    Name,
    Size,
    Date,
}

/// Parse a user-supplied date spec into a Unix timestamp (seconds since epoch).
///
/// Accepts three spellings, in order of preference:
///   * `@<unix>` — literal seconds since the epoch, e.g. `@1700000000`
///   * RFC 3339, e.g. `2024-01-02T03:04:05Z` or `2024-01-02T03:04:05+02:00`
///   * Date-only `YYYY-MM-DD`, interpreted as midnight UTC
///
/// The result is i64-sign-extended so callers can express pre-1970 dates, but
/// in practice every tar header stores a u64-ish mtime, so negatives get
/// clamped later.
pub fn parse_date(s: &str) -> std::result::Result<i64, String> {
    if let Some(rest) = s.strip_prefix('@') {
        return rest
            .parse::<i64>()
            .map_err(|e| format!("invalid unix timestamp `{s}`: {e}"));
    }

    // Try full RFC 3339 first — covers offsets and `Z`.
    if let Ok(dt) = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339) {
        return Ok(dt.unix_timestamp());
    }

    // Fall back to date-only (midnight UTC).
    let date_fmt = time::macros::format_description!("[year]-[month]-[day]");
    if let Ok(date) = time::Date::parse(s, date_fmt) {
        let dt = date.with_hms(0, 0, 0).map_err(|e| e.to_string())?;
        return Ok(dt.assume_utc().unix_timestamp());
    }

    Err(format!(
        "invalid date `{s}` (expected RFC 3339 like \
         `2024-01-02T03:04:05Z`, a date `2024-01-02`, or `@<unix-seconds>`)"
    ))
}

/// Parse an octal permission mode.  Accepts `"644"`, `"0644"`, and `"0o644"`.
/// Rejects values with set bits outside the low 12 bits (setuid/setgid/sticky
/// plus the standard rwx triads).
fn parse_octal_mode(s: &str) -> std::result::Result<u32, String> {
    let stripped = s
        .strip_prefix("0o")
        .or_else(|| s.strip_prefix("0O"))
        .unwrap_or(s);
    let mode =
        u32::from_str_radix(stripped, 8).map_err(|e| format!("invalid octal mode `{s}`: {e}"))?;
    if mode & !0o7777 != 0 {
        return Err(format!(
            "mode `{s}` has bits outside the 12-bit permission range (max 7777)"
        ));
    }
    Ok(mode)
}
