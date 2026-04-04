use std::io::{self, BufReader, BufWriter, Cursor};

use camino::{Utf8Path, Utf8PathBuf};
use rayon::iter::ParallelIterator;
use rayon::slice::ParallelSlice;
use ruzstd::encoding::CompressionLevel;

use crate::error::Result;
use crate::filter;
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

/// Block size for parallel zstd compression (1 MiB).
const PARALLEL_BLOCK_SIZE: usize = 1024 * 1024;

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        builder.follow_symlinks(opts.follow_symlinks);
        filter::append_inputs(&mut builder, inputs, opts)?;
        builder.into_inner()?;
    }

    let level = match opts.level {
        Some(0) => CompressionLevel::Uncompressed,
        _ => CompressionLevel::Fastest,
    };

    let file = fs_err::File::create(output)?;
    let mut buf = BufWriter::new(file);
    parallel_zst_compress(&tar_data, &mut buf, level)?;
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
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        builder.follow_symlinks(opts.follow_symlinks);
        filter::append_inputs(&mut builder, inputs, opts)?;
        builder.into_inner()?;
    }

    let level = match opts.level {
        Some(0) => CompressionLevel::Uncompressed,
        _ => CompressionLevel::Fastest,
    };

    parallel_zst_compress(&tar_data, &mut writer, level)?;
    Ok(())
}

/// Compress data in parallel zstd blocks.
///
/// Splits input into independently-compressed frames and writes them
/// sequentially. Concatenated zstd frames are valid — decoders transparently
/// join them.
fn parallel_zst_compress<W: io::Write>(
    data: &[u8],
    writer: &mut W,
    level: CompressionLevel,
) -> io::Result<()> {
    let compressed: Vec<Vec<u8>> = data
        .par_chunks(PARALLEL_BLOCK_SIZE)
        .map(|chunk| {
            let mut buf = Vec::with_capacity(chunk.len());
            ruzstd::encoding::compress(Cursor::new(chunk), &mut buf, level);
            buf
        })
        .collect();

    for block in &compressed {
        writer.write_all(block)?;
    }
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let decoder = open_decoder(input)?;
    let mut archive = tar::Archive::new(decoder);
    filter::unpack_tar_filtered(&mut archive, output, opts)?;
    Ok(())
}

pub fn decompress_from_reader<R: std::io::Read>(reader: R, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let decoder = MultiFrameDecoder::new(reader)
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

pub fn decompress_reader_to_writer<R: std::io::Read, W: std::io::Write>(reader: R, writer: &mut W, opts: &DecompressOpts<'_>) -> Result<()> {
    let decoder = MultiFrameDecoder::new(reader)
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
    filter::list_tar_entries(&mut archive)
}

// ── Info ──────────────────────────────────────────────────────────────────────

pub fn info(input: &Utf8Path) -> Result<ArchiveInfo> {
    let compressed_size = fs_err::metadata(input)?.len();

    let decoder = open_decoder(input)?;
    let mut archive = tar::Archive::new(decoder);
    let (entry_count, total_uncompressed) = filter::count_tar_entries(&mut archive)?;

    Ok(ArchiveInfo {
        format: "tar.zst",
        entry_count,
        total_uncompressed,
        compressed_size,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Open a `.tar.zst` file and return a multi-frame zstd decoder.
fn open_decoder(input: &Utf8Path) -> Result<MultiFrameDecoder<BufReader<fs_err::File>>> {
    let file = fs_err::File::open(input)?;
    let buf = BufReader::new(file);
    MultiFrameDecoder::new(buf).map_err(Into::into)
}

/// Zstd decoder that handles multiple concatenated frames.
///
/// `ruzstd::StreamingDecoder` only decodes a single frame. When parallel
/// compression produces multiple frames, this wrapper detects frame boundaries
/// via `into_inner()` and re-initialises a new decoder for each subsequent
/// frame.
struct MultiFrameDecoder<R: io::Read> {
    state: DecoderState<R>,
}

enum DecoderState<R: io::Read> {
    Active(Box<ruzstd::decoding::StreamingDecoder<R, ruzstd::decoding::FrameDecoder>>),
    /// Source is available between frames (previous decoder finished).
    Between(R),
    /// All frames consumed or source exhausted.
    Done,
}

impl<R: io::Read> MultiFrameDecoder<R> {
    fn new(source: R) -> io::Result<Self> {
        let decoder = ruzstd::decoding::StreamingDecoder::new(source)
            .map_err(io::Error::other)?;
        Ok(Self {
            state: DecoderState::Active(Box::new(decoder)),
        })
    }
}

impl<R: io::Read> io::Read for MultiFrameDecoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match &mut self.state {
                DecoderState::Active(decoder) => {
                    let n = decoder.read(buf)?;
                    if n > 0 {
                        return Ok(n);
                    }
                    // Frame exhausted — reclaim source for next frame.
                    let old = std::mem::replace(&mut self.state, DecoderState::Done);
                    if let DecoderState::Active(decoder) = old {
                        self.state = DecoderState::Between(decoder.into_inner());
                    }
                }
                DecoderState::Between(_) => {
                    let old = std::mem::replace(&mut self.state, DecoderState::Done);
                    if let DecoderState::Between(source) = old {
                        match ruzstd::decoding::StreamingDecoder::new(source) {
                            Ok(decoder) => {
                                self.state = DecoderState::Active(Box::new(decoder));
                            }
                            Err(_) => {
                                // No more valid frames (e.g. EOF reached during
                                // header read) — treat as end of stream.
                                return Ok(0);
                            }
                        }
                    }
                }
                DecoderState::Done => return Ok(0),
            }
        }
    }
}
