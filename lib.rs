use camino::Utf8PathBuf;
use globset::GlobSet;

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
pub struct Entry {
    pub path: Utf8PathBuf,
    pub size: u64,
    pub mtime: u64,
    pub mode: u32,
    pub is_dir: bool,
}

/// Summary metadata for an archive.
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
            progress: &NoProgress,
        }
    }

    pub(crate) fn can_fast_path(&self) -> bool {
        self.force
            && self.includes.is_empty()
            && self.excludes.is_empty()
            && !self.no_overwrite
            && !self.keep_newer
            && !self.no_directory
            && self.backup_suffix.is_none()
            && self.strip_components == 0
            && !self.preserve_permissions
    }
}
