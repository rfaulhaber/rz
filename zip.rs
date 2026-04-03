use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use globset::GlobSet;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::error::{Error, Result};
use crate::filter;
use crate::progress::ProgressReport;
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
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
        if meta.is_dir() {
            add_dir_recursive(&mut zip, input, name, options, &opts.excludes, opts.follow_symlinks, opts.progress)?;
        } else {
            zip.start_file(name, options)?;
            let data = fs_err::read(input)?;
            let size = data.len() as u64;
            io::Write::write_all(&mut zip, &data)?;
            opts.progress.set_entry(name);
            opts.progress.inc(size);
        }
    }

    let file = zip.finish()?;
    file.sync_all()?;
    Ok(())
}

/// Recursively add a directory and its contents to the zip archive.
fn add_dir_recursive(
    zip: &mut ZipWriter<fs_err::File>,
    dir: &Utf8Path,
    prefix: &str,
    options: SimpleFileOptions,
    excludes: &GlobSet,
    follow_symlinks: bool,
    progress: &dyn ProgressReport,
) -> Result<()> {
    zip.add_directory(format!("{prefix}/"), options)?;

    // Sort entries for deterministic archive output.
    let mut dir_entries: Vec<_> = fs_err::read_dir(dir)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    dir_entries.sort_by_key(|e| e.file_name());

    for entry in dir_entries {
        let entry_path = entry.path();
        let file_name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::InvalidUtf8Path(entry_path.display().to_string()))?;
        let entry_str = entry_path
            .to_str()
            .ok_or_else(|| Error::InvalidUtf8Path(entry_path.display().to_string()))?;
        let child = Utf8Path::new(entry_str);
        let name = format!("{prefix}/{file_name}");

        if excludes.is_match(&name) {
            continue;
        }

        let is_dir = if follow_symlinks {
            fs_err::metadata(child)?.is_dir()
        } else {
            entry.file_type()?.is_dir()
        };

        if is_dir {
            add_dir_recursive(zip, child, &name, options, excludes, follow_symlinks, progress)?;
        } else {
            zip.start_file(&name, options)?;
            let data = fs_err::read(child)?;
            let size = data.len() as u64;
            io::Write::write_all(zip, &data)?;
            progress.set_entry(&name);
            progress.inc(size);
        }
    }
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let mut archive = ZipArchive::new(file)?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = Utf8PathBuf::from(entry.name());

        // Include/exclude check against the original (pre-strip) path.
        if !filter::should_extract(name.as_str(), &opts.includes, &opts.excludes) {
            continue;
        }

        // --no-directory: skip directory entries, flatten file paths.
        if opts.no_directory && entry.is_dir() {
            continue;
        }

        let stripped = match filter::strip_components(&name, opts.strip_components) {
            Some(p) => p,
            None => continue,
        };

        let dest_path = if opts.no_directory {
            match stripped.file_name() {
                Some(name) => Utf8PathBuf::from(name),
                None => continue,
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
                if opts.keep_newer {
                    let entry_mtime = entry
                        .last_modified()
                        .map(zip_datetime_to_epoch)
                        .unwrap_or(0);
                    if filter::is_existing_newer(&out_path, entry_mtime)? {
                        continue;
                    }
                } else if opts.no_overwrite {
                    continue;
                } else if !opts.force {
                    return Err(Error::FileExists(out_path));
                }
            }
            let mut out_file = fs_err::File::create(&out_path)?;
            let written = io::copy(&mut entry, &mut out_file)?;
            opts.progress.set_entry(dest_path.as_str());
            opts.progress.inc(written);
        }
    }
    Ok(())
}

// ── Decompress to writer ─────────────────────────────────────────────────────

pub fn decompress_to_writer<W: std::io::Write>(input: &Utf8Path, writer: &mut W, opts: &DecompressOpts<'_>) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let mut archive = ZipArchive::new(file)?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = Utf8PathBuf::from(entry.name());

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
    let file = fs_err::File::open(input)?;
    let mut archive = ZipArchive::new(file)?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_owned();
        progress.set_entry(&name);
        let written = io::copy(&mut entry, &mut io::sink())?;
        progress.inc(written);
    }
    Ok(())
}

// ── List ──────────────────────────────────────────────────────────────────────

pub fn list(input: &Utf8Path) -> Result<Vec<Entry>> {
    let file = fs_err::File::open(input)?;
    let mut archive = ZipArchive::new(file)?;

    let mut entries = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i)?;
        entries.push(Entry {
            path: Utf8PathBuf::from(entry.name()),
            size: entry.size(),
            mtime: 0,
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

    let mut total_uncompressed: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i)?;
        total_uncompressed += entry.size();
    }

    Ok(ArchiveInfo {
        format: "zip",
        entry_count: archive.len(),
        total_uncompressed,
        compressed_size,
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Convert a zip `DateTime` to an approximate unix epoch (seconds since
/// 1970-01-01).  Good enough for mtime comparison.
fn zip_datetime_to_epoch(dt: zip::DateTime) -> u64 {
    let year = dt.year() as u64;
    let month = dt.month() as u64;
    let day = dt.day() as u64;
    let hour = dt.hour() as u64;
    let minute = dt.minute() as u64;
    let second = dt.second() as u64;

    // Cumulative days before each month (non-leap year).
    const MONTH_DAYS: [u64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

    let years_since_epoch = year.saturating_sub(1970);
    let leap_years = if year > 1970 {
        let y = year - 1;
        (y / 4 - y / 100 + y / 400) - (1969 / 4 - 1969 / 100 + 1969 / 400)
    } else {
        0
    };
    let mut days = years_since_epoch * 365 + leap_years;
    if (1..=12).contains(&month) {
        days += MONTH_DAYS[(month - 1) as usize];
    }
    days += day.saturating_sub(1);

    // Add leap day if past February in a leap year.
    let is_leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    if month > 2 && is_leap {
        days += 1;
    }

    days * 86400 + hour * 3600 + minute * 60 + second
}
