use camino::Utf8PathBuf;
use globset::GlobSet;

pub mod cmd;
pub mod error;
pub mod filter;
pub mod format;
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
pub struct CompressOpts {
    pub level: Option<u32>,
    pub excludes: GlobSet,
}

/// Options for decompress operations.
pub struct DecompressOpts {
    pub force: bool,
    pub strip_components: u32,
    pub excludes: GlobSet,
}
