use std::io::{self, BufReader, BufWriter, Cursor};

use camino::{Utf8Path, Utf8PathBuf};
use rayon::iter::ParallelIterator;
use rayon::slice::ParallelSlice;
use ruzstd::encoding::CompressionLevel;

use crate::error::{Error, Result};
use crate::filter;
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

/// Block size for parallel zstd compression (1 MiB).
///
/// **Memory note:** the parallel compress path buffers the entire uncompressed
/// tar archive in RAM before splitting it into blocks.  For very large inputs
/// (multi-GB), peak memory usage will be at least the uncompressed archive size.
/// This is a deliberate trade-off: parallel block compression yields significant
/// throughput gains at the cost of higher memory use.
const PARALLEL_BLOCK_SIZE: usize = 1024 * 1024;

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(inputs: &[Utf8PathBuf], output: &Utf8Path, opts: &CompressOpts<'_>) -> Result<()> {
    let inputs = filter::validate_inputs(inputs, opts)?;
    let level = resolve_zstd_level(opts.level)?;

    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        builder.follow_symlinks(opts.follow_symlinks);
        filter::append_inputs(&mut builder, &inputs, opts)?;
        builder.into_inner()?;
    }

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
    let inputs = filter::validate_inputs(inputs, opts)?;
    let level = resolve_zstd_level(opts.level)?;

    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        builder.follow_symlinks(opts.follow_symlinks);
        filter::append_inputs(&mut builder, &inputs, opts)?;
        builder.into_inner()?;
    }

    parallel_zst_compress(&tar_data, &mut writer, level)?;
    Ok(())
}

/// Compress data in parallel zstd blocks.
///
/// Splits input into independently-compressed frames and writes them
/// sequentially. Concatenated zstd frames are valid — decoders transparently
/// join them.
///
/// The `ruzstd` encoder writes to its drain with `.unwrap()` internally, so we
/// always compress into an in-memory buffer (writing to a `Vec` is infallible)
/// and then write that buffer to `writer` ourselves — otherwise a write error
/// on the real output (ENOSPC, a closed pipe) would panic instead of returning
/// a clean `io::Error`.
fn parallel_zst_compress<W: io::Write>(
    data: &[u8],
    writer: &mut W,
    level: CompressionLevel,
) -> io::Result<()> {
    // For inputs at or below a single block, skip rayon dispatch entirely.
    // Concatenated frames are fine, but a single frame with no intermediate
    // Vec<Vec<u8>> and no thread-pool setup is strictly cheaper.
    if data.len() <= PARALLEL_BLOCK_SIZE {
        let mut buf = Vec::new();
        ruzstd::encoding::compress(Cursor::new(data), &mut buf, level);
        writer.write_all(&buf)?;
        return Ok(());
    }

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

pub fn decompress_from_reader<R: std::io::Read>(
    reader: R,
    output: &Utf8Path,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    let decoder = MultiFrameDecoder::new(reader).map_err(std::io::Error::other)?;
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
    let decoder = open_decoder(input)?;
    let mut archive = tar::Archive::new(decoder);
    filter::extract_tar_to_writer(&mut archive, writer, opts)
}

pub fn decompress_reader_to_writer<R: std::io::Read, W: std::io::Write>(
    reader: R,
    writer: &mut W,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    let decoder = MultiFrameDecoder::new(reader).map_err(std::io::Error::other)?;
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

/// List entries from an arbitrary reader (e.g. stdin).
pub fn list_from_reader<R: std::io::Read>(reader: R) -> Result<Vec<Entry>> {
    let decoder = MultiFrameDecoder::new(reader)?;
    let mut archive = tar::Archive::new(decoder);
    filter::list_tar_entries(&mut archive)
}

/// Verify entries from an arbitrary reader (e.g. stdin).
pub fn test_from_reader<R: std::io::Read>(
    reader: R,
    progress: &dyn crate::progress::ProgressReport,
) -> Result<()> {
    let decoder = MultiFrameDecoder::new(reader)?;
    let mut archive = tar::Archive::new(decoder);
    filter::verify_tar_entries(&mut archive, progress)
}

// ── Info ──────────────────────────────────────────────────────────────────────

pub fn info(input: &Utf8Path) -> Result<ArchiveInfo> {
    let compressed_size = fs_err::metadata(input)?.len();

    let decoder = open_decoder(input)?;
    let mut archive = tar::Archive::new(decoder);
    let (entry_count, total_uncompressed) = filter::count_tar_entries(&mut archive)?;

    Ok(ArchiveInfo {
        format: "tar-zst",
        entry_count,
        total_uncompressed,
        compressed_size,
    })
}

/// Stream archive metadata from an arbitrary reader (e.g. stdin).
///
/// The reader is wrapped in a [`filter::CountingReader`] *before* the zstd
/// decoder, so `compressed_size` reflects the raw compressed bytes consumed —
/// the stdin equivalent of stat'ing the file.
pub fn info_from_reader<R: std::io::Read>(reader: R) -> Result<ArchiveInfo> {
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counting = filter::CountingReader::new(reader, std::sync::Arc::clone(&counter));
    let decoder = MultiFrameDecoder::new(counting).map_err(std::io::Error::other)?;
    let mut archive = tar::Archive::new(decoder);
    let (entry_count, total_uncompressed) = filter::count_tar_entries(&mut archive)?;

    Ok(ArchiveInfo {
        format: "tar-zst",
        entry_count,
        total_uncompressed,
        compressed_size: counter.load(std::sync::atomic::Ordering::Relaxed),
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Map an optional user-supplied compression level to a `ruzstd` level.
///
/// The pure-Rust `ruzstd` encoder only supports `Uncompressed` and `Fastest`.
/// Rather than silently ignoring the user's level, we accept `None` (default →
/// Fastest) and `Some(0)` (→ Uncompressed) and reject everything else.
fn resolve_zstd_level(level: Option<u32>) -> Result<CompressionLevel> {
    match level {
        None => Ok(CompressionLevel::Fastest),
        Some(0) => Ok(CompressionLevel::Uncompressed),
        Some(_) => Err(Error::ZstdLevelUnsupported),
    }
}

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
///
/// Exposed to the crate so the `modify` read-rewrite path can decode the same
/// multi-frame archives this module produces — a single-frame decoder there
/// would silently truncate any archive larger than one block.
pub(crate) struct MultiFrameDecoder<R: io::Read> {
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
    pub(crate) fn new(source: R) -> io::Result<Self> {
        let decoder = ruzstd::decoding::StreamingDecoder::new(source).map_err(io::Error::other)?;
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

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor, Read, Write};

    use ruzstd::encoding::{CompressionLevel, compress};

    use super::{MultiFrameDecoder, parallel_zst_compress};

    /// A writer that fails every write — stands in for a full disk or a closed
    /// pipe.  `ruzstd` writes to its drain with `.unwrap()`, so if we passed
    /// this straight to it the compress would panic; the production code routes
    /// ruzstd's output through an in-memory buffer so the error surfaces here.
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"))
        }
    }

    #[test]
    fn single_frame_compress_propagates_write_error() {
        // Small payload → single-frame path (the one that used to hand the real
        // writer to ruzstd).  A write error must come back as Err, not a panic.
        let data = b"payload".repeat(100);
        let mut writer = FailingWriter;
        let result = parallel_zst_compress(&data, &mut writer, CompressionLevel::Fastest);
        assert!(
            result.is_err(),
            "write error must propagate as Err, not panic"
        );
    }

    /// Compress `data` as a single zstd frame using the same encoder the
    /// production code uses for each parallel block.
    fn encode_frame(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        compress(Cursor::new(data), &mut out, CompressionLevel::Fastest);
        out
    }

    #[test]
    fn decodes_single_frame() -> io::Result<()> {
        let payload = b"hello multi-frame world".repeat(10);
        let encoded = encode_frame(&payload);
        let mut decoded = Vec::new();
        MultiFrameDecoder::new(Cursor::new(&encoded))?.read_to_end(&mut decoded)?;
        assert_eq!(decoded, payload);
        Ok(())
    }

    #[test]
    fn decodes_concatenated_frames() -> io::Result<()> {
        // Simulate the output of `compress_parallel` — multiple independent
        // frames written back-to-back into a single stream.
        let a = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec();
        let b = b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_vec();
        let c = b"cccccccccccccccccccccccccccccc".to_vec();
        let mut stream = Vec::new();
        stream.extend_from_slice(&encode_frame(&a));
        stream.extend_from_slice(&encode_frame(&b));
        stream.extend_from_slice(&encode_frame(&c));

        let mut decoded = Vec::new();
        MultiFrameDecoder::new(Cursor::new(&stream))?.read_to_end(&mut decoded)?;

        let mut expected = a.clone();
        expected.extend_from_slice(&b);
        expected.extend_from_slice(&c);
        assert_eq!(decoded, expected);
        Ok(())
    }

    #[test]
    fn small_buffer_reads_across_frame_boundary() -> io::Result<()> {
        // Read with a tiny buffer so we force the state machine to transition
        // Active -> Between -> Active multiple times.
        let chunks: Vec<Vec<u8>> = (0..4).map(|i| vec![b'a' + i; 128]).collect();
        let mut stream = Vec::new();
        for chunk in &chunks {
            stream.extend_from_slice(&encode_frame(chunk));
        }

        let mut decoder = MultiFrameDecoder::new(Cursor::new(&stream))?;
        let mut decoded = Vec::new();
        let mut small = [0u8; 7]; // deliberately awkward size
        loop {
            let n = decoder.read(&mut small)?;
            if n == 0 {
                break;
            }
            decoded.extend_from_slice(&small[..n]);
        }

        let mut expected = Vec::new();
        for chunk in &chunks {
            expected.extend_from_slice(chunk);
        }
        assert_eq!(decoded, expected);
        Ok(())
    }
}
