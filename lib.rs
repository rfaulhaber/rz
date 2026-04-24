use camino::Utf8PathBuf;
use globset::GlobSet;
use serde::Serialize;

use crate::progress::{NoProgress, ProgressReport};

pub mod cmd;
pub mod error;
pub mod filter;
pub mod format;
pub mod progress;
pub mod seven_z;
pub mod tar;
#[cfg(feature = "bzip2")]
pub mod tar_bz2;
pub mod tar_gz;
pub mod tar_xz;
pub mod tar_zst;
pub mod zip;

/// Metadata for a single entry within an archive.
#[derive(Serialize)]
pub struct Entry {
    pub path: Utf8PathBuf,
    pub size: u64,
    pub mtime: u64,
    pub mode: u32,
    pub is_dir: bool,
}

/// Summary metadata for an archive.
#[derive(Serialize)]
pub struct ArchiveInfo {
    pub format: &'static str,
    pub entry_count: usize,
    pub total_uncompressed: u64,
    pub compressed_size: u64,
}

/// Options for compress operations.
pub struct CompressOpts<'a> {
    pub level: Option<u32>,
    pub excludes: GlobSet,
    pub follow_symlinks: bool,
    pub exclude_vcs_ignores: bool,
    pub no_recursion: bool,
    pub progress: &'a dyn ProgressReport,
    /// Override mtime on all entries (unix timestamp).
    pub fixed_mtime: Option<u64>,
    /// Override uid on all entries.
    pub fixed_uid: Option<u64>,
    /// Override gid on all entries.
    pub fixed_gid: Option<u64>,
    /// Override permission mode on all entries (low 12 bits).
    pub fixed_mode: Option<u32>,
    /// Include only entries with mtime strictly greater than this (unix seconds).
    pub newer_than: Option<i64>,
    /// Include only entries with mtime strictly less than this (unix seconds).
    pub older_than: Option<i64>,
}

/// Options for decompress operations.
pub struct DecompressOpts<'a> {
    pub force: bool,
    pub no_overwrite: bool,
    pub keep_newer: bool,
    pub no_directory: bool,
    pub strip_components: u32,
    pub includes: GlobSet,
    pub excludes: GlobSet,
    pub backup_suffix: Option<String>,
    pub preserve_permissions: bool,
    /// Restore owner/group on extracted entries (tar-family only).
    pub same_owner: bool,
    /// Extract only entries with mtime strictly greater than this (unix seconds).
    pub newer_than: Option<i64>,
    /// Extract only entries with mtime strictly less than this (unix seconds).
    pub older_than: Option<i64>,
    pub progress: &'a dyn ProgressReport,
}

impl CompressOpts<'_> {
    /// Construct opts with no progress reporting (for tests / programmatic use).
    pub fn new(level: Option<u32>, excludes: GlobSet) -> CompressOpts<'static> {
        CompressOpts {
            level,
            excludes,
            follow_symlinks: false,
            exclude_vcs_ignores: false,
            no_recursion: false,
            progress: &NoProgress,
            fixed_mtime: None,
            fixed_uid: None,
            fixed_gid: None,
            fixed_mode: None,
            newer_than: None,
            older_than: None,
        }
    }
}

impl DecompressOpts<'_> {
    /// Construct opts with no progress reporting (for tests / programmatic use).
    pub fn new(
        force: bool,
        strip_components: u32,
        includes: GlobSet,
        excludes: GlobSet,
    ) -> DecompressOpts<'static> {
        DecompressOpts {
            force,
            no_overwrite: false,
            keep_newer: false,
            no_directory: false,
            strip_components,
            includes,
            excludes,
            backup_suffix: None,
            preserve_permissions: false,
            same_owner: false,
            newer_than: None,
            older_than: None,
            progress: &NoProgress,
        }
    }
}
