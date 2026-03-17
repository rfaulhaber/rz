use camino::Utf8PathBuf;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "rz", about = "Multi-format compression and decompression tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Global verbosity
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Compress files or directories
    #[command(alias = "c")]
    Compress {
        /// Input path(s)
        #[arg(required = true)]
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

        /// Number of threads (0 = all cores)
        #[arg(short, long, default_value_t = 0)]
        threads: usize,
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
        #[arg(short = 'F', long)]
        force: bool,
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
    },

    /// Show archive metadata
    Info {
        input: Utf8PathBuf,

        #[arg(short, long)]
        format: Option<Format>,
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
