use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use globset::GlobSet;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::error::{Error, Result};
use crate::filter;
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    opts: &CompressOpts,
) -> Result<()> {
    let file = fs_err::File::create(output)?;
    let mut zip = ZipWriter::new(file);

    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(opts.level.map(i64::from));

    for input in inputs {
        let meta = fs_err::symlink_metadata(input)?;
        let name = input.file_name().unwrap_or(input.as_str());
        if opts.excludes.is_match(name) {
            continue;
        }
        if meta.is_dir() {
            add_dir_recursive(&mut zip, input, name, options, &opts.excludes)?;
        } else {
            zip.start_file(name, options)?;
            let data = fs_err::read(input)?;
            io::Write::write_all(&mut zip, &data)?;
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

        if entry.file_type()?.is_dir() {
            add_dir_recursive(zip, child, &name, options, excludes)?;
        } else {
            zip.start_file(&name, options)?;
            let data = fs_err::read(child)?;
            io::Write::write_all(zip, &data)?;
        }
    }
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let mut archive = ZipArchive::new(file)?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = Utf8PathBuf::from(entry.name());

        // Exclude check against the original (pre-strip) path.
        let check_path = name.as_str().trim_end_matches('/');
        if opts.excludes.is_match(check_path) {
            continue;
        }

        let stripped = match filter::strip_components(&name, opts.strip_components) {
            Some(p) => p,
            None => continue,
        };

        let out_path = output.join(&stripped);

        if entry.is_dir() {
            fs_err::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs_err::create_dir_all(parent)?;
            }
            if !opts.force && fs_err::symlink_metadata(&out_path).is_ok() {
                return Err(Error::FileExists(out_path));
            }
            let mut out_file = fs_err::File::create(&out_path)?;
            io::copy(&mut entry, &mut out_file)?;
        }
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
