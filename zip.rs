use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::error::{Error, Result};
use crate::filter;
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(inputs: &[Utf8PathBuf], output: &Utf8Path, opts: &CompressOpts<'_>) -> Result<()> {
    let file = fs_err::File::create(output)?;
    let mut zip = ZipWriter::new(file);

    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(opts.level.map(i64::from));

    for input in inputs {
        let meta = filter::input_metadata(input, opts.follow_symlinks)?;
        let name = input.file_name().unwrap_or(input.as_str());
        if opts.excludes.is_match(name) {
            continue;
        }
        if !opts.follow_symlinks && meta.file_type().is_symlink() {
            write_symlink_entry(&mut zip, input, name, options, opts)?;
        } else if meta.is_dir() {
            if opts.no_recursion {
                zip.add_directory(format!("{name}/"), options)?;
            } else {
                add_dir_walked(&mut zip, input, name, options, opts)?;
            }
        } else {
            zip.start_file(name, options)?;
            let mut f = fs_err::File::open(input)?;
            let size = io::copy(&mut f, &mut zip)?;
            opts.progress.set_entry(name);
            opts.progress.inc(size);
        }
    }

    let file = zip.finish()?;
    file.sync_all()?;
    Ok(())
}

/// Walk a directory using [`filter::walk_dir`] and add entries to a zip archive.
/// Handles symlinks, regular files, and subdirectories.
fn add_dir_walked(
    zip: &mut ZipWriter<fs_err::File>,
    dir: &Utf8Path,
    prefix: &str,
    options: SimpleFileOptions,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    filter::walk_dir(dir, prefix, opts, &mut |entry| {
        let is_symlink = !opts.follow_symlinks
            && fs_err::symlink_metadata(&entry.fs_path)?
                .file_type()
                .is_symlink();

        if is_symlink {
            write_symlink_entry(zip, &entry.fs_path, &entry.archive_name, options, opts)?;
        } else if entry.is_dir {
            zip.add_directory(format!("{}/", entry.archive_name), options)?;
        } else {
            zip.start_file(&entry.archive_name, options)?;
            let mut f = fs_err::File::open(&entry.fs_path)?;
            let size = io::copy(&mut f, zip)?;
            opts.progress.set_entry(&entry.archive_name);
            opts.progress.inc(size);
        }
        Ok(())
    })
}

/// Store a symlink as a symlink entry (POSIX-style, with `S_IFLNK` mode and
/// the link target as the entry content). The `zip` crate sets `0o777`
/// permissions by default; Windows unzip tools may materialise this as a
/// regular text file containing the target path.
fn write_symlink_entry(
    zip: &mut ZipWriter<fs_err::File>,
    link_path: &Utf8Path,
    archive_name: &str,
    options: SimpleFileOptions,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let target = fs_err::read_link(link_path)?;
    let target_str = target
        .to_str()
        .ok_or_else(|| Error::InvalidUtf8Path(target.display().to_string()))?;
    zip.add_symlink_from_path(archive_name, target_str, options)?;
    opts.progress.set_entry(archive_name);
    opts.progress.inc(target_str.len() as u64);
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let (len, shared_metadata) = {
        let file = fs_err::File::open(input)?;
        let archive = ZipArchive::new(file)?;
        (archive.len(), archive.metadata())
    };

    (0..len).into_par_iter().try_for_each_init(
        || -> Option<ZipArchive<fs_err::File>> {
            let file = fs_err::File::open(input).ok()?;
            // SAFETY: metadata was parsed from the same file.
            Some(unsafe { ZipArchive::unsafe_new_with_metadata(file, shared_metadata.clone()) })
        },
        |maybe_archive, i| -> Result<()> {
            let archive = maybe_archive
                .as_mut()
                .ok_or_else(|| Error::Io(io::Error::other("failed to open zip archive")))?;
            let mut entry = archive.by_index(i)?;
            let name = Utf8PathBuf::from(entry.name());

            // Reject entries that attempt path traversal.
            filter::safe_entry_path(name.as_str())?;

            if !filter::should_extract(name.as_str(), &opts.includes, &opts.excludes) {
                return Ok(());
            }

            if opts.no_directory && entry.is_dir() {
                return Ok(());
            }

            let stripped = match filter::strip_components(&name, opts.strip_components) {
                Some(p) => p,
                None => return Ok(()),
            };

            let dest_path = if opts.no_directory {
                match stripped.file_name() {
                    Some(name) => Utf8PathBuf::from(name),
                    None => return Ok(()),
                }
            } else {
                stripped
            };

            let out_path = output.join(&dest_path);

            if entry.is_dir() {
                fs_err::create_dir_all(&out_path)?;
            } else {
                if let Some(parent) = out_path.parent() {
                    fs_err::create_dir_all(parent)?;
                }
                if fs_err::symlink_metadata(&out_path).is_ok() {
                    if let Some(ref suffix) = opts.backup_suffix {
                        let backup = Utf8PathBuf::from(format!("{out_path}{suffix}"));
                        fs_err::rename(&out_path, &backup)?;
                    } else if opts.keep_newer {
                        let entry_mtime = entry
                            .last_modified()
                            .map(zip_datetime_to_epoch)
                            .unwrap_or(0);
                        if filter::is_existing_newer(&out_path, entry_mtime)? {
                            return Ok(());
                        }
                    } else if opts.no_overwrite {
                        return Ok(());
                    } else if !opts.force {
                        return Err(Error::FileExists(out_path));
                    }
                }
                let unix_mode = entry.unix_mode();
                let mut out_file = fs_err::File::create(&out_path)?;
                let written = io::copy(&mut entry, &mut out_file)?;
                #[cfg(unix)]
                if opts.preserve_permissions
                    && let Some(mode) = unix_mode
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs_err::set_permissions(&out_path, std::fs::Permissions::from_mode(mode))?;
                }
                opts.progress.set_entry(dest_path.as_str());
                opts.progress.inc(written);
            }
            Ok(())
        },
    )?;
    Ok(())
}

// ── Decompress to writer ─────────────────────────────────────────────────────

pub fn decompress_to_writer<W: std::io::Write>(
    input: &Utf8Path,
    writer: &mut W,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let mut archive = ZipArchive::new(file)?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = Utf8PathBuf::from(entry.name());

        // Reject entries that attempt path traversal.
        filter::safe_entry_path(name.as_str())?;

        if !filter::should_extract(name.as_str(), &opts.includes, &opts.excludes) {
            continue;
        }

        let stripped = match filter::strip_components(&name, opts.strip_components) {
            Some(p) => p,
            None => continue,
        };

        if entry.is_dir() {
            continue;
        }

        opts.progress.set_entry(stripped.as_str());
        io::copy(&mut entry, writer)?;
    }
    Ok(())
}

// ── Test ──────────────────────────────────────────────────────────────────────

pub fn test(input: &Utf8Path, progress: &dyn crate::progress::ProgressReport) -> Result<()> {
    let (len, shared_metadata) = {
        let file = fs_err::File::open(input)?;
        let archive = ZipArchive::new(file)?;
        (archive.len(), archive.metadata())
    };

    (0..len).into_par_iter().try_for_each_init(
        || -> Option<ZipArchive<fs_err::File>> {
            let file = fs_err::File::open(input).ok()?;
            // SAFETY: metadata was parsed from the same file.
            Some(unsafe { ZipArchive::unsafe_new_with_metadata(file, shared_metadata.clone()) })
        },
        |maybe_archive, i| -> Result<()> {
            let archive = maybe_archive
                .as_mut()
                .ok_or_else(|| Error::Io(io::Error::other("failed to open zip archive")))?;
            let mut entry = archive.by_index(i)?;
            let name = entry.name().to_owned();
            progress.set_entry(&name);
            let written = io::copy(&mut entry, &mut io::sink())?;
            progress.inc(written);
            Ok(())
        },
    )?;
    Ok(())
}

// ── List ──────────────────────────────────────────────────────────────────────

pub fn list(input: &Utf8Path) -> Result<Vec<Entry>> {
    let file = fs_err::File::open(input)?;
    let mut archive = ZipArchive::new(file)?;
    let mut entries = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i)?;
        entries.push(Entry {
            path: Utf8PathBuf::from(entry.name()),
            size: entry.size(),
            mtime: entry
                .last_modified()
                .map(zip_datetime_to_epoch)
                .unwrap_or(0),
            mode: entry.unix_mode().unwrap_or(0),
            is_dir: entry.is_dir(),
        });
    }
    Ok(entries)
}

// ── Info ──────────────────────────────────────────────────────────────────────

pub fn info(input: &Utf8Path) -> Result<ArchiveInfo> {
    let compressed_size = fs_err::metadata(input)?.len();

    let file = fs_err::File::open(input)?;
    let mut archive = ZipArchive::new(file)?;
    let entry_count = archive.len();

    // Fast path: decompressed_size() reads from the already-parsed central
    // directory with zero per-entry I/O.  Falls back to by_index_raw() only
    // when the archive uses data descriptors (uncommon).
    let total_uncompressed = match archive.decompressed_size() {
        Some(size) => u64::try_from(size).unwrap_or(u64::MAX),
        None => {
            let mut total: u64 = 0;
            for i in 0..entry_count {
                let entry = archive.by_index_raw(i)?;
                total += entry.size();
            }
            total
        }
    };

    Ok(ArchiveInfo {
        format: "zip",
        entry_count,
        total_uncompressed,
        compressed_size,
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Convert a zip `DateTime` to a unix epoch (seconds since 1970-01-01).
/// Returns 0 for any invalid or pre-epoch date.
fn zip_datetime_to_epoch(dt: zip::DateTime) -> u64 {
    let Some(month) = time::Month::try_from(dt.month()).ok() else {
        return 0;
    };
    let Some(date) = time::Date::from_calendar_date(dt.year() as i32, month, dt.day()).ok() else {
        return 0;
    };
    let Some(time) = time::Time::from_hms(dt.hour(), dt.minute(), dt.second()).ok() else {
        return 0;
    };

    let stamp = time::PrimitiveDateTime::new(date, time)
        .assume_utc()
        .unix_timestamp();
    if stamp >= 0 { stamp as u64 } else { 0 }
}
