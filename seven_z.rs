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
    let use_extract_fn = !opts.includes.is_empty()
        || !opts.excludes.is_empty()
        || opts.no_overwrite
        || opts.keep_newer
        || opts.no_directory;
    if use_extract_fn {
        sevenz_rust2::decompress_file_with_extract_fn(input, output, |entry, reader, dest| {
            if !crate::filter::should_extract(&entry.name, &opts.includes, &opts.excludes) {
                return Ok(true);
            }
            if opts.no_directory && entry.is_directory {
                return Ok(true);
            }
            let out_name = if opts.no_directory {
                Utf8Path::new(&entry.name)
                    .file_name()
                    .map(String::from)
                    .unwrap_or_else(|| entry.name.clone())
            } else {
                entry.name.clone()
            };
            let out_path = dest.join(&out_name);
            if !entry.is_directory && out_path.exists() {
                if opts.keep_newer {
                    // 7z entries don't reliably expose mtime; skip if file exists.
                    return Ok(true);
                }
                if opts.no_overwrite {
                    return Ok(true);
                }
            }
            if opts.no_directory {
                // Extract to flat dest with the basename only.
                let mut out_file = fs_err::File::create(&out_path)
                    .map_err(|e| sevenz_rust2::Error::Io(e, out_name.into()))?;
                std::io::copy(reader, &mut out_file)
                    .map_err(|e| sevenz_rust2::Error::Io(e, "copy".into()))?;
                Ok(true)
            } else {
                sevenz_rust2::default_entry_extract_fn(entry, reader, dest)
            }
        })?;
    } else {
        sevenz_rust2::decompress_file(input, output)?;
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
