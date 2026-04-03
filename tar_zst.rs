use std::io::{BufReader, BufWriter, Cursor};

use camino::{Utf8Path, Utf8PathBuf};
use ruzstd::decoding::StreamingDecoder;
use ruzstd::encoding::CompressionLevel;

use crate::error::{Error, Result};
use crate::filter;
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    // ruzstd's compressor is pull-based (Read → Write), so we build the tar
    // archive in memory first, then compress to the output file.
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        builder.follow_symlinks(opts.follow_symlinks);
        for input in inputs {
            let meta = filter::input_metadata(input, opts.follow_symlinks)?;
            let name = input.file_name().unwrap_or(input.as_str());
            if opts.excludes.is_match(name) {
                continue;
            }
            if meta.is_dir() {
                filter::append_dir_filtered(&mut builder, input, name, &opts.excludes, opts.follow_symlinks, opts.progress)?;
            } else {
                let size = meta.len();
                builder.append_path_with_name(input, name)?;
                opts.progress.set_entry(name);
                opts.progress.inc(size);
            }
        }
        builder.into_inner()?;
    }

    let level = match opts.level {
        Some(0) => CompressionLevel::Uncompressed,
        _ => CompressionLevel::Fastest,
    };

    let file = fs_err::File::create(output)?;
    let mut buf = BufWriter::new(file);
    ruzstd::encoding::compress(Cursor::new(tar_data), &mut buf, level);
    let file = buf.into_inner().map_err(std::io::Error::other)?;
    file.sync_all()?;

    Ok(())
}

// ── Compress to writer ───────────────────────────────────────────────────────

pub fn compress_to_writer<W: std::io::Write>(
    inputs: &[Utf8PathBuf],
    mut writer: W,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    // ruzstd's compressor is pull-based, so buffer tar in memory first.
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        builder.follow_symlinks(opts.follow_symlinks);
        for input in inputs {
            let meta = filter::input_metadata(input, opts.follow_symlinks)?;
            let name = input.file_name().unwrap_or(input.as_str());
            if opts.excludes.is_match(name) {
                continue;
            }
            if meta.is_dir() {
                filter::append_dir_filtered(&mut builder, input, name, &opts.excludes, opts.follow_symlinks, opts.progress)?;
            } else {
                let size = meta.len();
                builder.append_path_with_name(input, name)?;
                opts.progress.set_entry(name);
                opts.progress.inc(size);
            }
        }
        builder.into_inner()?;
    }

    let level = match opts.level {
        Some(0) => CompressionLevel::Uncompressed,
        _ => CompressionLevel::Fastest,
    };

    ruzstd::encoding::compress(Cursor::new(tar_data), &mut writer, level);
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let decoder = open_decoder(input)?;
    let mut archive = tar::Archive::new(decoder);
    filter::unpack_tar_filtered(&mut archive, output, opts)?;
    Ok(())
}

pub fn decompress_from_reader<R: std::io::BufRead>(reader: R, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let decoder = StreamingDecoder::new(reader)
        .map_err(std::io::Error::other)?;
    let mut archive = tar::Archive::new(decoder);
    filter::unpack_tar_filtered(&mut archive, output, opts)?;
    Ok(())
}

// ── Decompress to writer ─────────────────────────────────────────────────────

pub fn decompress_to_writer<W: std::io::Write>(input: &Utf8Path, writer: &mut W, opts: &DecompressOpts<'_>) -> Result<()> {
    let decoder = open_decoder(input)?;
    let mut archive = tar::Archive::new(decoder);
    filter::extract_tar_to_writer(&mut archive, writer, opts)
}

pub fn decompress_reader_to_writer<R: std::io::BufRead, W: std::io::Write>(reader: R, writer: &mut W, opts: &DecompressOpts<'_>) -> Result<()> {
    let decoder = StreamingDecoder::new(reader)
        .map_err(std::io::Error::other)?;
    let mut archive = tar::Archive::new(decoder);
    filter::extract_tar_to_writer(&mut archive, writer, opts)
}

// ── Test ──────────────────────────────────────────────────────────────────────

pub fn test(input: &Utf8Path, progress: &dyn crate::progress::ProgressReport) -> Result<()> {
    let decoder = open_decoder(input)?;
    let mut archive = tar::Archive::new(decoder);
    filter::verify_tar_entries(&mut archive, progress)
}

// ── List ──────────────────────────────────────────────────────────────────────

pub fn list(input: &Utf8Path) -> Result<Vec<Entry>> {
    let decoder = open_decoder(input)?;
    let mut archive = tar::Archive::new(decoder);

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

    let decoder = open_decoder(input)?;
    let mut archive = tar::Archive::new(decoder);

    let mut entry_count: usize = 0;
    let mut total_uncompressed: u64 = 0;
    for entry in archive.entries()? {
        let entry = entry?;
        total_uncompressed += entry.header().size()?;
        entry_count += 1;
    }

    Ok(ArchiveInfo {
        format: "tar.zst",
        entry_count,
        total_uncompressed,
        compressed_size,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Open a `.tar.zst` file and return a streaming zstd decoder.
fn open_decoder(
    input: &Utf8Path,
) -> Result<StreamingDecoder<BufReader<fs_err::File>, ruzstd::decoding::FrameDecoder>> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    StreamingDecoder::new(buf).map_err(|e| std::io::Error::other(e).into())
}
