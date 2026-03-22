use std::io::{BufReader, BufWriter};

use bzip2::read::BzDecoder;
use bzip2::write::BzEncoder;
use bzip2::Compression;
use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{Error, Result};
use crate::filter;
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let compression = match opts.level {
        Some(l) => Compression::try_new(l)
            .ok_or_else(|| std::io::Error::other("bzip2 compression level must be 1..=9"))?,
        None => Compression::default(),
    };
    let file = fs_err::File::create(output)?;
    let buf = BufWriter::new(file);
    let bz = BzEncoder::new(buf, compression);
    let mut builder = tar::Builder::new(bz);

    for input in inputs {
        let meta = fs_err::symlink_metadata(input)?;
        let name = input.file_name().unwrap_or(input.as_str());
        if opts.excludes.is_match(name) {
            continue;
        }
        if meta.is_dir() {
            filter::append_dir_filtered(&mut builder, input, name, &opts.excludes, opts.progress)?;
        } else {
            let size = meta.len();
            builder.append_path_with_name(input, name)?;
            opts.progress.set_entry(name);
            opts.progress.inc(size);
        }
    }

    // Explicit finalization: Builder → BzEncoder → BufWriter → File
    let bz = builder.into_inner()?;
    let buf = bz.finish()?;
    let file = buf.into_inner().map_err(std::io::Error::other)?;
    file.sync_all()?;

    Ok(())
}

// ── Compress to writer ───────────────────────────────────────────────────────

pub fn compress_to_writer<W: std::io::Write>(
    inputs: &[Utf8PathBuf],
    writer: W,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let compression = match opts.level {
        Some(l) => Compression::try_new(l)
            .ok_or_else(|| std::io::Error::other("bzip2 compression level must be 1..=9"))?,
        None => Compression::default(),
    };
    let buf = BufWriter::new(writer);
    let bz = BzEncoder::new(buf, compression);
    let mut builder = tar::Builder::new(bz);

    for input in inputs {
        let meta = fs_err::symlink_metadata(input)?;
        let name = input.file_name().unwrap_or(input.as_str());
        if opts.excludes.is_match(name) {
            continue;
        }
        if meta.is_dir() {
            filter::append_dir_filtered(&mut builder, input, name, &opts.excludes, opts.progress)?;
        } else {
            let size = meta.len();
            builder.append_path_with_name(input, name)?;
            opts.progress.set_entry(name);
            opts.progress.inc(size);
        }
    }

    let bz = builder.into_inner()?;
    bz.finish()?;
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    decompress_from_reader(buf, output, opts)
}

pub fn decompress_from_reader<R: std::io::Read>(reader: R, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let bz = BzDecoder::new(reader);
    let mut archive = tar::Archive::new(bz);
    filter::unpack_tar_filtered(&mut archive, output, opts)?;
    Ok(())
}

// ── Decompress to writer ─────────────────────────────────────────────────────

pub fn decompress_to_writer<W: std::io::Write>(input: &Utf8Path, writer: &mut W, opts: &DecompressOpts<'_>) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    decompress_reader_to_writer(buf, writer, opts)
}

pub fn decompress_reader_to_writer<R: std::io::Read, W: std::io::Write>(reader: R, writer: &mut W, opts: &DecompressOpts<'_>) -> Result<()> {
    let bz = BzDecoder::new(reader);
    let mut archive = tar::Archive::new(bz);
    filter::extract_tar_to_writer(&mut archive, writer, opts)
}

// ── Test ──────────────────────────────────────────────────────────────────────

pub fn test(input: &Utf8Path, progress: &dyn crate::progress::ProgressReport) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let bz = BzDecoder::new(buf);
    let mut archive = tar::Archive::new(bz);
    filter::verify_tar_entries(&mut archive, progress)
}

// ── List ──────────────────────────────────────────────────────────────────────

pub fn list(input: &Utf8Path) -> Result<Vec<Entry>> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let bz = BzDecoder::new(buf);
    let mut archive = tar::Archive::new(bz);

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
    let bz = BzDecoder::new(buf);
    let mut archive = tar::Archive::new(bz);

    let mut entry_count: usize = 0;
    let mut total_uncompressed: u64 = 0;
    for entry in archive.entries()? {
        let entry = entry?;
        total_uncompressed += entry.header().size()?;
        entry_count += 1;
    }

    Ok(ArchiveInfo {
        format: "tar.bz2",
        entry_count,
        total_uncompressed,
        compressed_size,
    })
}
