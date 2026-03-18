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
    opts: &CompressOpts,
) -> Result<()> {
    // ruzstd's compressor is pull-based (Read → Write), so we build the tar
    // archive in memory first, then compress to the output file.
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        for input in inputs {
            let meta = fs_err::symlink_metadata(input)?;
            let name = input.file_name().unwrap_or(input.as_str());
            if opts.excludes.is_match(name) {
                continue;
            }
            if meta.is_dir() {
                filter::append_dir_filtered(&mut builder, input, name, &opts.excludes)?;
            } else {
                builder.append_path_with_name(input, name)?;
            }
        }
        builder.into_inner()?;
    }

    let file = fs_err::File::create(output)?;
    let mut buf = BufWriter::new(file);
    ruzstd::encoding::compress(Cursor::new(tar_data), &mut buf, CompressionLevel::Fastest);
    let file = buf.into_inner().map_err(std::io::Error::other)?;
    file.sync_all()?;

    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts) -> Result<()> {
    let decoder = open_decoder(input)?;
    let mut archive = tar::Archive::new(decoder);
    filter::unpack_tar_filtered(&mut archive, output, opts)?;
    Ok(())
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
