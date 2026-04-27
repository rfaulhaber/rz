use std::io::{BufReader, BufWriter};

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::Result;
use crate::filter;
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

#[cfg(feature = "xz2")]
pub fn compress(inputs: &[Utf8PathBuf], output: &Utf8Path, opts: &CompressOpts<'_>) -> Result<()> {
    let level = opts.level.unwrap_or(6);
    let file = fs_err::File::create(output)?;
    let buf = BufWriter::new(file);
    let encoder = xz2::write::XzEncoder::new(buf, level);
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(opts.follow_symlinks);

    filter::append_inputs(&mut builder, inputs, opts)?;

    let encoder = builder.into_inner()?;
    let buf = encoder.finish()?;
    let file = buf.into_inner().map_err(std::io::Error::other)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(feature = "xz2"))]
pub fn compress(inputs: &[Utf8PathBuf], output: &Utf8Path, opts: &CompressOpts<'_>) -> Result<()> {
    let level = opts.level.unwrap_or(6);
    let file = fs_err::File::create(output)?;
    let buf = BufWriter::new(file);
    let encoder = lzma_rust2::XzWriter::new(buf, lzma_rust2::XzOptions::with_preset(level))?;
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(opts.follow_symlinks);

    filter::append_inputs(&mut builder, inputs, opts)?;

    let encoder = builder.into_inner()?;
    let buf = encoder.finish()?;
    let file = buf.into_inner().map_err(std::io::Error::other)?;
    file.sync_all()?;
    Ok(())
}

// ── Compress to writer ───────────────────────────────────────────────────────

#[cfg(feature = "xz2")]
pub fn compress_to_writer<W: std::io::Write>(
    inputs: &[Utf8PathBuf],
    writer: W,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let level = opts.level.unwrap_or(6);
    let buf = BufWriter::new(writer);
    let encoder = xz2::write::XzEncoder::new(buf, level);
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(opts.follow_symlinks);

    filter::append_inputs(&mut builder, inputs, opts)?;

    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok(())
}

#[cfg(not(feature = "xz2"))]
pub fn compress_to_writer<W: std::io::Write>(
    inputs: &[Utf8PathBuf],
    writer: W,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let level = opts.level.unwrap_or(6);
    let encoder = lzma_rust2::XzWriter::new(writer, lzma_rust2::XzOptions::with_preset(level))?;
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(opts.follow_symlinks);

    filter::append_inputs(&mut builder, inputs, opts)?;

    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let mut archive = open_archive(input)?;
    filter::unpack_tar_filtered(&mut archive, output, opts)?;
    Ok(())
}

#[cfg(feature = "xz2")]
pub fn decompress_from_reader<R: std::io::Read>(
    reader: R,
    output: &Utf8Path,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    let decoder = xz2::read::XzDecoder::new(BufReader::new(reader));
    let mut archive = tar::Archive::new(decoder);
    filter::unpack_tar_filtered(&mut archive, output, opts)?;
    Ok(())
}

#[cfg(not(feature = "xz2"))]
pub fn decompress_from_reader<R: std::io::Read>(
    reader: R,
    output: &Utf8Path,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    let decoder = lzma_rust2::XzReader::new(BufReader::new(reader), true);
    let mut archive = tar::Archive::new(decoder);
    filter::unpack_tar_filtered(&mut archive, output, opts)?;
    Ok(())
}

// ── Decompress to writer ─────────────────────────────────────────────────────

pub fn decompress_to_writer<W: std::io::Write>(
    input: &Utf8Path,
    writer: &mut W,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    let mut archive = open_archive(input)?;
    filter::extract_tar_to_writer(&mut archive, writer, opts)
}

#[cfg(feature = "xz2")]
pub fn decompress_reader_to_writer<R: std::io::Read, W: std::io::Write>(
    reader: R,
    writer: &mut W,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    let decoder = xz2::read::XzDecoder::new(BufReader::new(reader));
    let mut archive = tar::Archive::new(decoder);
    filter::extract_tar_to_writer(&mut archive, writer, opts)
}

#[cfg(not(feature = "xz2"))]
pub fn decompress_reader_to_writer<R: std::io::Read, W: std::io::Write>(
    reader: R,
    writer: &mut W,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    let decoder = lzma_rust2::XzReader::new(BufReader::new(reader), true);
    let mut archive = tar::Archive::new(decoder);
    filter::extract_tar_to_writer(&mut archive, writer, opts)
}

// ── Test ──────────────────────────────────────────────────────────────────────

pub fn test(input: &Utf8Path, progress: &dyn crate::progress::ProgressReport) -> Result<()> {
    let mut archive = open_archive(input)?;
    filter::verify_tar_entries(&mut archive, progress)
}

// ── List ──────────────────────────────────────────────────────────────────────

pub fn list(input: &Utf8Path) -> Result<Vec<Entry>> {
    let mut archive = open_archive(input)?;
    filter::list_tar_entries(&mut archive)
}

// ── Info ──────────────────────────────────────────────────────────────────────

pub fn info(input: &Utf8Path) -> Result<ArchiveInfo> {
    let compressed_size = fs_err::metadata(input)?.len();

    let mut archive = open_archive(input)?;
    let (entry_count, total_uncompressed) = filter::count_tar_entries(&mut archive)?;

    Ok(ArchiveInfo {
        format: "tar.xz",
        entry_count,
        total_uncompressed,
        compressed_size,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Open a `.tar.xz` file and return a tar archive with streaming xz decompression.
#[cfg(feature = "xz2")]
fn open_archive(
    input: &Utf8Path,
) -> Result<tar::Archive<xz2::read::XzDecoder<BufReader<fs_err::File>>>> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let decoder = xz2::read::XzDecoder::new(buf);
    Ok(tar::Archive::new(decoder))
}

#[cfg(not(feature = "xz2"))]
fn open_archive(
    input: &Utf8Path,
) -> Result<tar::Archive<lzma_rust2::XzReader<BufReader<fs_err::File>>>> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let decoder = lzma_rust2::XzReader::new(buf, true);
    Ok(tar::Archive::new(decoder))
}
