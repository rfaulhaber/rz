use std::io::{self, BufReader, BufWriter};

use camino::{Utf8Path, Utf8PathBuf};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::error::{Error, Result};

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    level: Option<u32>,
) -> Result<()> {
    let level = level.unwrap_or(6);
    let file = fs_err::File::create(output)?;
    let buf = BufWriter::new(file);
    let gz = GzEncoder::new(buf, Compression::new(level));
    let mut builder = tar::Builder::new(gz);

    for input in inputs {
        let meta = fs_err::symlink_metadata(input)?;
        if meta.is_dir() {
            builder.append_dir_all(input.file_name().unwrap_or(input.as_str()), input)?;
        } else {
            builder.append_path_with_name(input, input.file_name().unwrap_or(input.as_str()))?;
        }
    }

    // Explicit finalization: Builder → GzEncoder → BufWriter → File
    let gz = builder.into_inner()?;
    let buf = gz.finish()?;
    let file = buf.into_inner().map_err(io::Error::other)?;
    file.sync_all()?;

    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, force: bool) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let gz = GzDecoder::new(buf);
    let mut archive = tar::Archive::new(gz);
    archive.set_overwrite(force);
    archive.unpack(output)?;
    Ok(())
}

// ── List ──────────────────────────────────────────────────────────────────────

pub struct Entry {
    pub path: Utf8PathBuf,
    pub size: u64,
    pub mtime: u64,
    pub mode: u32,
    pub is_dir: bool,
}

pub fn list(input: &Utf8Path) -> Result<Vec<Entry>> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let gz = GzDecoder::new(buf);
    let mut archive = tar::Archive::new(gz);

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

pub struct Info {
    pub format: &'static str,
    pub entry_count: usize,
    pub total_uncompressed: u64,
    pub compressed_size: u64,
}

pub fn info(input: &Utf8Path) -> Result<Info> {
    let compressed_size = fs_err::metadata(input)?.len();

    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let gz = GzDecoder::new(buf);
    let mut archive = tar::Archive::new(gz);

    let mut entry_count: usize = 0;
    let mut total_uncompressed: u64 = 0;
    for entry in archive.entries()? {
        let entry = entry?;
        total_uncompressed += entry.header().size()?;
        entry_count += 1;
    }

    Ok(Info {
        format: "tar.gz",
        entry_count,
        total_uncompressed,
        compressed_size,
    })
}
