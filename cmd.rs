use camino::Utf8PathBuf;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "rz", version, about = "Multi-format compression and decompression tool")]
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
        #[arg(short, long)]
        level: Option<u32>,

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
    },
}

#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
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
