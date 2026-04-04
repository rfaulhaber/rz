use std::io::{self, BufReader, BufWriter};

use camino::{Utf8Path, Utf8PathBuf};
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rayon::iter::ParallelIterator;
use rayon::slice::ParallelSlice;

use crate::error::Result;
use crate::filter;
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

/// Block size for parallel gzip compression (1 MiB).
const PARALLEL_BLOCK_SIZE: usize = 1024 * 1024;

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let level = opts.level.unwrap_or(6);

    // Buffer the tar archive in memory, then gzip-compress in parallel blocks.
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        builder.follow_symlinks(opts.follow_symlinks);
        filter::append_inputs(&mut builder, inputs, opts)?;
        builder.into_inner()?;
    }

    let file = fs_err::File::create(output)?;
    let mut buf = BufWriter::new(file);
    parallel_gz_compress(&tar_data, &mut buf, level)?;
    let file = buf.into_inner().map_err(io::Error::other)?;
    file.sync_all()?;

    Ok(())
}

// ── Compress to writer ───────────────────────────────────────────────────────

pub fn compress_to_writer<W: std::io::Write>(
    inputs: &[Utf8PathBuf],
    mut writer: W,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let level = opts.level.unwrap_or(6);

    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        builder.follow_symlinks(opts.follow_symlinks);
        filter::append_inputs(&mut builder, inputs, opts)?;
        builder.into_inner()?;
    }

    parallel_gz_compress(&tar_data, &mut writer, level)?;
    Ok(())
}

/// Compress data in parallel gzip blocks.
///
/// Splits input into independently-compressed blocks and concatenates the
/// results. Concatenated gzip streams are valid per RFC 1952 — decompressors
/// transparently join them.
fn parallel_gz_compress<W: io::Write>(data: &[u8], writer: &mut W, level: u32) -> io::Result<()> {
    let compressed: Vec<Vec<u8>> = data
        .par_chunks(PARALLEL_BLOCK_SIZE)
        .map(|chunk| {
            let buf = Vec::with_capacity(chunk.len());
            let mut enc = GzEncoder::new(buf, Compression::new(level));
            io::Write::write_all(&mut enc, chunk)?;
            enc.finish()
        })
        .collect::<io::Result<Vec<_>>>()?;

    for block in &compressed {
        writer.write_all(block)?;
    }
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    decompress_from_reader(buf, output, opts)
}

pub fn decompress_from_reader<R: std::io::Read>(reader: R, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let gz = MultiGzDecoder::new(reader);
    let mut archive = tar::Archive::new(gz);
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
    let gz = MultiGzDecoder::new(reader);
    let mut archive = tar::Archive::new(gz);
    filter::extract_tar_to_writer(&mut archive, writer, opts)
}

// ── Test ──────────────────────────────────────────────────────────────────────

pub fn test(input: &Utf8Path, progress: &dyn crate::progress::ProgressReport) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let gz = MultiGzDecoder::new(buf);
    let mut archive = tar::Archive::new(gz);
    filter::verify_tar_entries(&mut archive, progress)
}

// ── List ──────────────────────────────────────────────────────────────────────

pub fn list(input: &Utf8Path) -> Result<Vec<Entry>> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let gz = MultiGzDecoder::new(buf);
    let mut archive = tar::Archive::new(gz);
    filter::list_tar_entries(&mut archive)
}

// ── Info ──────────────────────────────────────────────────────────────────────

pub fn info(input: &Utf8Path) -> Result<ArchiveInfo> {
    let compressed_size = fs_err::metadata(input)?.len();

    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    let gz = MultiGzDecoder::new(buf);
    let mut archive = tar::Archive::new(gz);
    let (entry_count, total_uncompressed) = filter::count_tar_entries(&mut archive)?;

    Ok(ArchiveInfo {
        format: "tar.gz",
        entry_count,
        total_uncompressed,
        compressed_size,
    })
}
