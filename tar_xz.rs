use std::io::{BufReader, BufWriter};
#[cfg(not(feature = "xz2"))]
use std::io::Cursor;

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::Result;
use crate::filter;
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

#[cfg(feature = "xz2")]
pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
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
pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    // lzma-rs provides a one-shot compress function, so we buffer the tar
    // archive in memory first, then xz-compress to the output file.
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        builder.follow_symlinks(opts.follow_symlinks);
        filter::append_inputs(&mut builder, inputs, opts)?;
        builder.into_inner()?;
    }

    let file = fs_err::File::create(output)?;
    let mut buf = BufWriter::new(file);
    lzma_rs::xz_compress(&mut Cursor::new(tar_data), &mut buf)?;
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
    mut writer: W,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        builder.follow_symlinks(opts.follow_symlinks);
        filter::append_inputs(&mut builder, inputs, opts)?;
        builder.into_inner()?;
    }
    lzma_rs::xz_compress(&mut Cursor::new(tar_data), &mut writer)?;
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let mut archive = open_archive(input)?;
    filter::unpack_tar_filtered(&mut archive, output, opts)?;
    Ok(())
}

#[cfg(feature = "xz2")]
pub fn decompress_from_reader<R: std::io::Read>(reader: R, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let decoder = xz2::read::XzDecoder::new(BufReader::new(reader));
    let mut archive = tar::Archive::new(decoder);
    filter::unpack_tar_filtered(&mut archive, output, opts)?;
    Ok(())
}

#[cfg(not(feature = "xz2"))]
pub fn decompress_from_reader<R: std::io::Read>(reader: R, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let mut tar_data = Vec::new();
    lzma_rs::xz_decompress(&mut BufReader::new(reader), &mut tar_data)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut archive = tar::Archive::new(Cursor::new(tar_data));
    filter::unpack_tar_filtered(&mut archive, output, opts)?;
    Ok(())
}

// ── Decompress to writer ─────────────────────────────────────────────────────

pub fn decompress_to_writer<W: std::io::Write>(input: &Utf8Path, writer: &mut W, opts: &DecompressOpts<'_>) -> Result<()> {
    let mut archive = open_archive(input)?;
    filter::extract_tar_to_writer(&mut archive, writer, opts)
}

#[cfg(feature = "xz2")]
pub fn decompress_reader_to_writer<R: std::io::Read, W: std::io::Write>(reader: R, writer: &mut W, opts: &DecompressOpts<'_>) -> Result<()> {
    let decoder = xz2::read::XzDecoder::new(BufReader::new(reader));
    let mut archive = tar::Archive::new(decoder);
    filter::extract_tar_to_writer(&mut archive, writer, opts)
}

#[cfg(not(feature = "xz2"))]
pub fn decompress_reader_to_writer<R: std::io::Read, W: std::io::Write>(reader: R, writer: &mut W, opts: &DecompressOpts<'_>) -> Result<()> {
    let mut tar_data = Vec::new();
    lzma_rs::xz_decompress(&mut BufReader::new(reader), &mut tar_data)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut archive = tar::Archive::new(Cursor::new(tar_data));
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

/// Open a `.tar.xz` file and return a tar archive with xz decompression.
///
/// With the `xz2` feature this streams through the C-backed liblzma decoder.
/// Without it, the entire archive is decompressed into memory via `lzma-rs`.
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
fn open_archive(input: &Utf8Path) -> Result<tar::Archive<Cursor<Vec<u8>>>> {
    let file = fs_err::File::open(input)?;
    let mut buf = BufReader::new(file);
    let mut tar_data = Vec::new();
    lzma_rs::xz_decompress(&mut buf, &mut tar_data)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(tar::Archive::new(Cursor::new(tar_data)))
}
