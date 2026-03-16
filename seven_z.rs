use camino::{Utf8Path, Utf8PathBuf};

use crate::error::Result;
use crate::{ArchiveInfo, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    _level: Option<u32>,
) -> Result<()> {
    let mut writer = sevenz_rust2::ArchiveWriter::create(output)?;
    for input in inputs {
        writer.push_source_path(input, |_| true)?;
    }
    let file = writer.finish()?;
    file.sync_all()?;
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, _force: bool) -> Result<()> {
    sevenz_rust2::decompress_file(input, output)?;
    Ok(())
}

// ── List ──────────────────────────────────────────────────────────────────────

pub fn list(input: &Utf8Path) -> Result<Vec<Entry>> {
    let archive = sevenz_rust2::Archive::open(input)?;
    let mut entries = Vec::new();
    for file in &archive.files {
        let path = Utf8PathBuf::from(&file.name);
        entries.push(Entry {
            path,
            size: file.size,
            mtime: 0,
            mode: 0,
            is_dir: file.is_directory,
        });
    }
    Ok(entries)
}

// ── Info ──────────────────────────────────────────────────────────────────────

pub fn info(input: &Utf8Path) -> Result<ArchiveInfo> {
    let compressed_size = fs_err::metadata(input)?.len();
    let archive = sevenz_rust2::Archive::open(input)?;

    let mut entry_count: usize = 0;
    let mut total_uncompressed: u64 = 0;
    for file in &archive.files {
        total_uncompressed += file.size;
        entry_count += 1;
    }

    Ok(ArchiveInfo {
        format: "7z",
        entry_count,
        total_uncompressed,
        compressed_size,
    })
}
