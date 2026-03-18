use std::io::{BufReader, BufWriter};

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{Error, Result};
use crate::{ArchiveInfo, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    _level: Option<u32>,
) -> Result<()> {
    let file = fs_err::File::create(output)?;
    let buf = BufWriter::new(file);
    let mut builder = tar::Builder::new(buf);

    for input in inputs {
        let meta = fs_err::symlink_metadata(input)?;
        if meta.is_dir() {
            builder.append_dir_all(input.file_name().unwrap_or(input.as_str()), input)?;
        } else {
            builder.append_path_with_name(input, input.file_name().unwrap_or(input.as_str()))?;
        }
    }

    let buf = builder.into_inner()?;
    let file = buf.into_inner().map_err(std::io::Error::other)?;
    file.sync_all()?;

    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, force: bool) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let mut archive = tar::Archive::new(buf);
    archive.set_overwrite(force);
    archive.unpack(output)?;
    Ok(())
}

// ── List ──────────────────────────────────────────────────────────────────────

pub fn list(input: &Utf8Path) -> Result<Vec<Entry>> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let mut archive = tar::Archive::new(buf);

    let mut entries = Vec::new();
    for entry in archive.entries()? {
        let entry = entry?;
        let header = entry.header();
        let path = entry.path()?;
        let path = Utf8PathBuf::try_from(path.into_owned())
            .map_err(|e| Error::InvalidUtf8Path(e.into_path_buf().display().to_string()))?;
        entries.push(Entry {
            path,
            size: header.size()?,
            mtime: header.mtime()?,
            mode: header.mode()?,
            is_dir: header.entry_type().is_dir(),
        });
    }
    Ok(entries)
}

// ── Info ──────────────────────────────────────────────────────────────────────

pub fn info(input: &Utf8Path) -> Result<ArchiveInfo> {
    let compressed_size = fs_err::metadata(input)?.len();

    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let mut archive = tar::Archive::new(buf);

    let mut entry_count: usize = 0;
    let mut total_uncompressed: u64 = 0;
    for entry in archive.entries()? {
        let entry = entry?;
        total_uncompressed += entry.header().size()?;
        entry_count += 1;
    }

    Ok(ArchiveInfo {
        format: "tar",
        entry_count,
        total_uncompressed,
        compressed_size,
    })
}
