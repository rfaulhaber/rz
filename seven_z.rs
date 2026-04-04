use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{Error, Result};
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(inputs: &[Utf8PathBuf], output: &Utf8Path, opts: &CompressOpts<'_>) -> Result<()> {
    let mut writer = sevenz_rust2::ArchiveWriter::create(output)?;
    for input in inputs {
        let meta = crate::filter::input_metadata(input, opts.follow_symlinks)?;
        if meta.is_dir() && opts.no_recursion {
            // Only add the directory entry, not its contents.
            continue;
        }
        if meta.is_dir() && opts.exclude_vcs_ignores {
            push_dir_vcs(&mut writer, input, opts)?;
        } else {
            let excludes = &opts.excludes;
            writer.push_source_path(input, |name| !excludes.is_match(name))?;
        }
    }
    let file = writer.finish()?;
    file.sync_all()?;
    Ok(())
}

/// Walk a directory with VCS-ignore awareness and add entries to the 7z writer.
fn push_dir_vcs(
    writer: &mut sevenz_rust2::ArchiveWriter<std::fs::File>,
    dir: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    for result in crate::filter::vcs_walker(dir, opts.follow_symlinks) {
        let entry = result.map_err(|e| std::io::Error::other(e.to_string()))?;
        let fs_path = entry.path();
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());

        if is_dir {
            continue;
        }

        let utf8_str = fs_path
            .to_str()
            .ok_or_else(|| Error::InvalidUtf8Path(fs_path.display().to_string()))?;
        let utf8_path = Utf8Path::new(utf8_str);

        let excludes = &opts.excludes;
        writer.push_source_path(utf8_path, |name| !excludes.is_match(name))?;
    }
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    if opts.strip_components > 0 {
        return Err(Error::StripComponentsUnsupported("7z".to_owned()));
    }
    // Use the fast path only when force is set and no filtering/special
    // options are active.  Otherwise we need the callback to enforce
    // overwrite guards, include/exclude, and backup logic.
    if opts.can_fast_path() {
        sevenz_rust2::decompress_file(input, output)?;
    } else {
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
                if let Some(ref suffix) = opts.backup_suffix {
                    let backup_name = format!("{}{suffix}", out_path.display());
                    fs_err::rename(&out_path, Utf8Path::new(&backup_name))
                        .map_err(|e| sevenz_rust2::Error::Io(e, backup_name.into()))?;
                } else if opts.keep_newer {
                    // 7z entries don't reliably expose mtime; skip if file exists.
                    return Ok(true);
                } else if opts.no_overwrite {
                    return Ok(true);
                } else if !opts.force {
                    let utf8 = Utf8PathBuf::from(out_path.display().to_string());
                    return Err(sevenz_rust2::Error::Io(
                        std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            format!("file already exists: {utf8} (use --force to overwrite)"),
                        ),
                        utf8.into_string().into(),
                    ));
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
    }
    Ok(())
}

// ── Decompress to writer ─────────────────────────────────────────────────────

pub fn decompress_to_writer<W: std::io::Write>(
    input: &Utf8Path,
    writer: &mut W,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
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

    Ok(ArchiveInfo {
        format: "7z",
        entry_count: archive.files.len(),
        total_uncompressed: archive.files.iter().map(|f| f.size).sum(),
        compressed_size,
    })
}
