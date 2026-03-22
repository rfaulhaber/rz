use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{Error, Result};
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let mut writer = sevenz_rust2::ArchiveWriter::create(output)?;
    for input in inputs {
        let excludes = &opts.excludes;
        writer.push_source_path(input, |name| !excludes.is_match(name))?;
    }
    let file = writer.finish()?;
    file.sync_all()?;
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    if opts.strip_components > 0 {
        return Err(Error::StripComponentsUnsupported("7z".to_owned()));
    }
    if opts.includes.is_empty() && opts.excludes.is_empty() {
        sevenz_rust2::decompress_file(input, output)?;
    } else {
        sevenz_rust2::decompress_file_with_extract_fn(input, output, |entry, reader, dest| {
            if !crate::filter::should_extract(&entry.name, &opts.includes, &opts.excludes) {
                return Ok(true);
            }
            sevenz_rust2::default_entry_extract_fn(entry, reader, dest)
        })?;
    }
    Ok(())
}

// ── Decompress to writer ─────────────────────────────────────────────────────

pub fn decompress_to_writer<W: std::io::Write>(input: &Utf8Path, writer: &mut W, opts: &DecompressOpts<'_>) -> Result<()> {
    sevenz_rust2::decompress_file_with_extract_fn(input, ".", |entry, reader, _dest| {
        if entry.is_directory {
            return Ok(true);
        }
        if !crate::filter::should_extract(&entry.name, &opts.includes, &opts.excludes) {
            return Ok(true);
        }
        opts.progress.set_entry(&entry.name);
        std::io::copy(reader, writer)
            .map_err(|e| sevenz_rust2::Error::Io(e, "decompress to writer".into()))?;
        Ok(true) // skip default extraction
    })?;
    Ok(())
}

// ── Test ──────────────────────────────────────────────────────────────────────

pub fn test(input: &Utf8Path, progress: &dyn crate::progress::ProgressReport) -> Result<()> {
    sevenz_rust2::decompress_file_with_extract_fn(input, ".", |entry, reader, _dest| {
        progress.set_entry(&entry.name);
        let written = std::io::copy(reader, &mut std::io::sink())
            .map_err(|e| sevenz_rust2::Error::Io(e, "test: reading entry".into()))?;
        progress.inc(written);
        Ok(true) // skip default extraction
    })?;
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
