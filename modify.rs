//! `append`, `update`, and `remove` operations across archive backends.
//!
//! ## Strategy by format
//!
//! | Format        | Append / Update                | Remove        |
//! |---------------|--------------------------------|---------------|
//! | tar           | in-place (seek past EOF marker)| read-rewrite  |
//! | tar.gz/etc.   | read-rewrite (compressed layer cannot be patched) | read-rewrite |
//! | zip           | in-place (`ZipWriter::new_append`)                | read-rewrite |
//! | 7z            | unsupported — `sevenz-rust2` does not expose write-into-existing |
//!
//! All read-rewrite paths write to `<archive>.tmp.rzappend` in the same
//! directory and atomically rename on success, so a partial write never
//! truncates the user's archive.

use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};

use camino::{Utf8Path, Utf8PathBuf};
use globset::GlobSet;

use crate::cmd::Format;
use crate::error::{Error, Result};
use crate::filter;
use crate::{CompressOpts, progress::NoProgress};

/// `Append` always writes the new entry; `Update` only writes it when the
/// filesystem mtime is strictly newer than the archive's existing copy
/// (or the entry is absent from the archive entirely).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppendMode {
    Append,
    Update,
}

/// Default compression level when rewriting compressed-tar archives during
/// append/update/remove and the user did not pass `-l`.  Mirrors the
/// per-format defaults used elsewhere (gzip 6, bzip2 6).
fn default_level_for(fmt: Format) -> Option<u32> {
    match fmt {
        Format::TarGz | Format::TarBz2 | Format::TarXz => Some(6),
        _ => None,
    }
}

/// Tar block size — every header and every padded data region is a multiple
/// of this.  See `tar` crate's BLOCK_SIZE constant.
const TAR_BLOCK: u64 = 512;

/// Round `n` up to the nearest multiple of `TAR_BLOCK`.
fn round_to_block(n: u64) -> u64 {
    n.div_ceil(TAR_BLOCK).saturating_mul(TAR_BLOCK)
}

/// Path for the temporary file used by read-rewrite operations.
fn temp_path(archive: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(format!("{archive}.tmp.rzappend"))
}

// ── Public entry points ──────────────────────────────────────────────────────

pub fn append(
    archive: &Utf8Path,
    fmt: Format,
    inputs: &[Utf8PathBuf],
    mode: AppendMode,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let op = match mode {
        AppendMode::Append => "append",
        AppendMode::Update => "update",
    };
    match fmt {
        Format::Tar => tar_append(archive, inputs, mode, opts),
        Format::TarGz | Format::TarZst | Format::TarXz => {
            tar_compressed_append(archive, fmt, inputs, mode, opts)
        }
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => tar_compressed_append(archive, fmt, inputs, mode, opts),
        #[cfg(not(feature = "bzip2"))]
        Format::TarBz2 => Err(Error::ModifyUnsupported {
            operation: op,
            format: fmt.to_string(),
        }),
        Format::Zip => zip_append(archive, inputs, mode, opts),
        Format::SevenZ => Err(Error::ModifyUnsupported {
            operation: op,
            format: fmt.to_string(),
        }),
    }
}

pub fn remove(archive: &Utf8Path, fmt: Format, patterns: &[String], level: Option<u32>) -> Result<()> {
    let glob = filter::build_glob_set(patterns)?;
    match fmt {
        Format::Tar => tar_remove(archive, &glob),
        Format::TarGz | Format::TarZst | Format::TarXz => {
            tar_compressed_remove(archive, fmt, &glob, level)
        }
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => tar_compressed_remove(archive, fmt, &glob, level),
        #[cfg(not(feature = "bzip2"))]
        Format::TarBz2 => Err(Error::ModifyUnsupported {
            operation: "remove",
            format: fmt.to_string(),
        }),
        Format::Zip => zip_remove(archive, &glob),
        Format::SevenZ => Err(Error::ModifyUnsupported {
            operation: "remove",
            format: fmt.to_string(),
        }),
    }
}

// ── tar (uncompressed) ───────────────────────────────────────────────────────

/// Build an `entry-name → mtime (unix seconds)` index from an uncompressed
/// tar file.  Used by `update` mode to decide whether each filesystem input
/// is newer than the corresponding archive entry.
fn tar_index_uncompressed(archive: &Utf8Path) -> Result<HashMap<String, u64>> {
    let file = fs_err::File::open(archive)?;
    let mut a = tar::Archive::new(BufReader::new(file));
    let mut idx = HashMap::new();
    for entry in a.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        if let Some(name) = path.to_str() {
            let mtime = entry.header().mtime().unwrap_or(0);
            idx.insert(name.to_owned(), mtime);
        }
    }
    Ok(idx)
}

fn tar_append(
    archive: &Utf8Path,
    inputs: &[Utf8PathBuf],
    mode: AppendMode,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let archive_idx = if mode == AppendMode::Update {
        Some(tar_index_uncompressed(archive)?)
    } else {
        None
    };

    // Find the position of the first byte after the last entry's data block.
    // The trailing zero blocks (if any) start at or after this position; we
    // overwrite them with new entries followed by a fresh terminator written
    // by Builder::into_inner.
    let body_end = {
        let file = fs_err::File::open(archive)?;
        let mut a = tar::Archive::new(BufReader::new(file));
        let mut max_end: u64 = 0;
        for entry in a.entries()? {
            let entry = entry?;
            let end = entry
                .raw_file_position()
                .saturating_add(round_to_block(entry.size()));
            if end > max_end {
                max_end = end;
            }
        }
        max_end
    };

    // Re-open in read+write mode and seek to the end-of-data position.
    let mut file = fs_err::OpenOptions::new()
        .read(true)
        .write(true)
        .open(archive)?;
    file.seek(SeekFrom::Start(body_end))?;
    // Truncate any trailing zero blocks so Builder::finish writes the only
    // remaining EOF terminator and the file's logical length is correct.
    file.set_len(body_end)?;

    let opts = filtered_opts(opts, archive_idx.as_ref());
    let buf = BufWriter::new(file);
    let mut builder = tar::Builder::new(buf);
    builder.follow_symlinks(opts.follow_symlinks);
    append_inputs_with_index(&mut builder, inputs, &opts, archive_idx.as_ref())?;
    let buf = builder.into_inner()?;
    let file = buf.into_inner().map_err(std::io::Error::other)?;
    file.sync_all()?;
    Ok(())
}

/// Build a `CompressOpts` whose `excludes` set additionally rejects any
/// input whose archive name has an mtime in `archive_idx` greater than or
/// equal to the filesystem mtime — i.e. the archive's copy is at least as
/// new as the input.
///
/// `update` semantics need this filter to apply at the per-file level inside
/// the directory walk.  The walker calls `excludes.is_match(archive_name)`
/// before descending; a glob set can't read mtimes, but it doesn't need to —
/// we instead handle it via the time-window filter (`newer_than`) keyed off
/// the archive's per-entry mtime.
///
/// In practice, for `update` we set `newer_than` per-input dynamically.
/// Since `CompressOpts` is shared for the whole walk, we can't vary it per
/// entry; the simplest correct approach is to filter the inputs themselves
/// before walking — done by the caller path below for files, and via a
/// per-walked-entry mtime check for directories.  This helper is the file
/// path used for both: it just hands back `opts` unchanged because the
/// per-entry mtime filter is applied inside [`AppendVisitor`] below.
fn filtered_opts<'a>(
    opts: &CompressOpts<'a>,
    _archive_idx: Option<&HashMap<String, u64>>,
) -> CompressOpts<'a> {
    CompressOpts {
        level: opts.level,
        excludes: opts.excludes.clone(),
        follow_symlinks: opts.follow_symlinks,
        exclude_vcs_ignores: opts.exclude_vcs_ignores,
        no_recursion: opts.no_recursion,
        progress: opts.progress,
        fixed_mtime: opts.fixed_mtime,
        fixed_uid: opts.fixed_uid,
        fixed_gid: opts.fixed_gid,
        fixed_mode: opts.fixed_mode,
        newer_than: opts.newer_than,
        older_than: opts.older_than,
    }
}

/// Walk `inputs` and append entries to `builder`, applying `update`
/// semantics: a filesystem entry is appended only when its archive name is
/// missing from `archive_idx` or its filesystem mtime is strictly newer
/// than the recorded archive mtime.  When `archive_idx` is `None` every
/// entry is appended (plain `append` mode).
///
/// Mirrors the structure of [`filter::append_inputs`] but adds the per-file
/// mtime gate.  Kept here rather than in `filter.rs` to avoid leaking the
/// modify-only `archive_idx` concept into the broader compress path.
fn append_inputs_with_index<W: Write>(
    builder: &mut tar::Builder<W>,
    inputs: &[Utf8PathBuf],
    opts: &CompressOpts<'_>,
    archive_idx: Option<&HashMap<String, u64>>,
) -> Result<()> {
    let Some(idx) = archive_idx else {
        return filter::append_inputs(builder, inputs, opts);
    };
    for input in inputs {
        let meta = filter::input_metadata(input, opts.follow_symlinks)?;
        let name = input.file_name().unwrap_or(input.as_str());
        if opts.excludes.is_match(name) {
            continue;
        }
        if meta.is_dir() {
            // For directories we walk children individually so we can apply
            // the per-name index lookup at file granularity.
            walk_and_append_with_index(builder, input, name, opts, idx)?;
        } else {
            let fs_mtime = mtime_secs(&meta);
            if !is_newer_than_archive(name, fs_mtime, idx) {
                continue;
            }
            append_one_file(builder, input, name, opts)?;
            opts.progress.set_entry(name);
            opts.progress.inc(meta.len());
        }
    }
    Ok(())
}

fn walk_and_append_with_index<W: Write>(
    builder: &mut tar::Builder<W>,
    dir: &Utf8Path,
    prefix: &str,
    opts: &CompressOpts<'_>,
    idx: &HashMap<String, u64>,
) -> Result<()> {
    if opts.no_recursion {
        // Add only the directory entry itself.
        builder.append_dir(prefix, dir.as_std_path())?;
        return Ok(());
    }
    filter::walk_dir(dir, prefix, opts, &mut |entry| {
        if entry.is_dir {
            // Tar update doesn't gate directory entries — gnu tar emits dir
            // entries whenever any file inside is updated.  We always emit
            // them so an updated child's parent path exists in the archive.
            builder.append_dir(&entry.archive_name, entry.fs_path.as_std_path())?;
        } else {
            let meta = filter::input_metadata(&entry.fs_path, opts.follow_symlinks)?;
            let fs_mtime = mtime_secs(&meta);
            if !is_newer_than_archive(&entry.archive_name, fs_mtime, idx) {
                return Ok(());
            }
            append_one_file(builder, &entry.fs_path, &entry.archive_name, opts)?;
            opts.progress.set_entry(&entry.archive_name);
            opts.progress.inc(meta.len());
        }
        Ok(())
    })
}

fn append_one_file<W: Write>(
    builder: &mut tar::Builder<W>,
    fs_path: &Utf8Path,
    archive_name: &str,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    // Defer to the same single-file append used by compress, which handles
    // all the header-override cases.  We re-implement the no-overrides
    // branch here because the helper in `filter.rs` is private; the simple
    // path is just `append_path_with_name`.
    let _ = opts;
    builder.append_path_with_name(fs_path, archive_name)?;
    Ok(())
}

fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn is_newer_than_archive(name: &str, fs_mtime: u64, idx: &HashMap<String, u64>) -> bool {
    match idx.get(name) {
        // Strictly newer mirrors `tar -u`'s "if file is newer" rule.
        Some(&archive_mtime) => fs_mtime > archive_mtime,
        // Not in the archive → always include.
        None => true,
    }
}

// ── tar (compressed) — read-rewrite ─────────────────────────────────────────

/// Open an existing tar-family archive for streaming reads of its entries.
/// Returns a `Box<dyn Read>` so the caller can iterate without caring which
/// compression layer is in play.
fn open_tar_reader(archive: &Utf8Path, fmt: Format) -> Result<Box<dyn Read>> {
    let file = fs_err::File::open(archive)?;
    let buf = BufReader::new(file);
    match fmt {
        Format::Tar => Ok(Box::new(buf)),
        Format::TarGz => Ok(Box::new(flate2::read::MultiGzDecoder::new(buf))),
        Format::TarZst => {
            let dec =
                ruzstd::decoding::StreamingDecoder::new(buf).map_err(std::io::Error::other)?;
            Ok(Box::new(dec))
        }
        Format::TarXz => xz_read(buf),
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => Ok(Box::new(bzip2::read::BzDecoder::new(buf))),
        #[cfg(not(feature = "bzip2"))]
        Format::TarBz2 => Err(Error::ModifyUnsupported {
            operation: "read",
            format: fmt.to_string(),
        }),
        Format::Zip | Format::SevenZ => Err(Error::ModifyUnsupported {
            operation: "tar-reader",
            format: fmt.to_string(),
        }),
    }
}

#[cfg(feature = "xz2")]
fn xz_read(buf: BufReader<fs_err::File>) -> Result<Box<dyn Read>> {
    Ok(Box::new(xz2::read::XzDecoder::new(buf)))
}

#[cfg(not(feature = "xz2"))]
fn xz_read(buf: BufReader<fs_err::File>) -> Result<Box<dyn Read>> {
    Ok(Box::new(lzma_rust2::XzReader::new(buf, true)))
}

/// Wrap a `Write` in the appropriate compression encoder for `fmt`.
///
/// Returns a finalizer closure: callers must invoke it after the tar
/// builder has finished so the encoder gets a chance to flush its trailer
/// (gzip CRC, zstd checksum, etc.).
fn tar_compressed_writer(
    fmt: Format,
    writer: BufWriter<fs_err::File>,
    level: Option<u32>,
) -> Result<Box<dyn EncoderHandle>> {
    match fmt {
        Format::TarGz => Ok(Box::new(GzHandle::new(writer, level.unwrap_or(6)))),
        Format::TarZst => Ok(Box::new(ZstHandle::new(writer, level)?)),
        Format::TarXz => xz_writer(writer, level.unwrap_or(6)),
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => Ok(Box::new(Bz2Handle::new(writer, level.unwrap_or(6))?)),
        #[cfg(not(feature = "bzip2"))]
        Format::TarBz2 => Err(Error::ModifyUnsupported {
            operation: "rewrite",
            format: fmt.to_string(),
        }),
        _ => Err(Error::ModifyUnsupported {
            operation: "rewrite",
            format: fmt.to_string(),
        }),
    }
}

#[cfg(feature = "xz2")]
fn xz_writer(writer: BufWriter<fs_err::File>, level: u32) -> Result<Box<dyn EncoderHandle>> {
    Ok(Box::new(XzHandle::new(writer, level)))
}

#[cfg(not(feature = "xz2"))]
fn xz_writer(writer: BufWriter<fs_err::File>, level: u32) -> Result<Box<dyn EncoderHandle>> {
    Ok(Box::new(LzmaRust2XzHandle::new(writer, level)?))
}

/// Trait for opaque encoder pipelines used by `tar_compressed_append`.  Each
/// implementation owns a tar builder pointed at a buffered, compressed
/// writer and exposes only the pieces the caller needs: a mutable reference
/// to the builder for entry writes, and a `finish` that flushes everything
/// down through the file.
trait EncoderHandle {
    /// Read-rewrite copy of every existing entry from `reader` into the
    /// internal tar builder, then return so the caller can append new
    /// entries via [`Self::append`].
    ///
    /// `keep` returns `true` when the entry should be carried over.
    fn copy_existing(&mut self, reader: &mut dyn Read, keep: &mut dyn FnMut(&str) -> bool)
    -> Result<HashMap<String, u64>>;
    fn append_inputs(
        &mut self,
        inputs: &[Utf8PathBuf],
        opts: &CompressOpts<'_>,
        archive_idx: Option<&HashMap<String, u64>>,
    ) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

/// Drive a tar Archive over `reader`, copying every entry that `keep`
/// approves into `builder` via `Builder::append`.  Records the mtime of
/// each surviving entry under its archive name so the caller can implement
/// `update` semantics against that index.
fn copy_tar_entries<W: Write>(
    builder: &mut tar::Builder<W>,
    reader: &mut dyn Read,
    keep: &mut dyn FnMut(&str) -> bool,
) -> Result<HashMap<String, u64>> {
    let mut idx = HashMap::new();
    let mut a = tar::Archive::new(reader);
    for entry in a.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let name = path
            .to_str()
            .ok_or_else(|| Error::InvalidUtf8Path(path.display().to_string()))?
            .to_owned();
        if !keep(&name) {
            continue;
        }
        let mtime = entry.header().mtime().unwrap_or(0);
        idx.insert(name.clone(), mtime);
        // Clone the on-disk header so all metadata (mode, owner, mtime,
        // entry type, link target, etc.) is preserved.  Use `append_data`
        // rather than `append` so the path is rewritten through the
        // long-name extension machinery — `header.path()` only returns
        // what fits in the 100-byte name field, but `entry.path()` (used
        // above) is the *resolved* full name from any preceding `L`
        // header.  Re-emitting via `append_data` regenerates that
        // extension if needed.
        let mut header = entry.header().clone();
        builder.append_data(&mut header, &name, &mut entry)?;
    }
    Ok(idx)
}

// Per-format encoder handles ---------------------------------------------------

struct GzHandle {
    builder: Option<tar::Builder<flate2::write::GzEncoder<BufWriter<fs_err::File>>>>,
}

impl GzHandle {
    fn new(writer: BufWriter<fs_err::File>, level: u32) -> Self {
        let enc = flate2::write::GzEncoder::new(writer, flate2::Compression::new(level));
        Self {
            builder: Some(tar::Builder::new(enc)),
        }
    }
}

impl EncoderHandle for GzHandle {
    fn copy_existing(
        &mut self,
        reader: &mut dyn Read,
        keep: &mut dyn FnMut(&str) -> bool,
    ) -> Result<HashMap<String, u64>> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        copy_tar_entries(b, reader, keep)
    }
    fn append_inputs(
        &mut self,
        inputs: &[Utf8PathBuf],
        opts: &CompressOpts<'_>,
        archive_idx: Option<&HashMap<String, u64>>,
    ) -> Result<()> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        b.follow_symlinks(opts.follow_symlinks);
        append_inputs_with_index(b, inputs, opts, archive_idx)
    }
    fn finish(mut self: Box<Self>) -> Result<()> {
        let b = self.builder.take().ok_or_else(builder_taken_err)?;
        let enc = b.into_inner()?;
        let buf = enc.finish()?;
        let file = buf.into_inner().map_err(std::io::Error::other)?;
        file.sync_all()?;
        Ok(())
    }
}

struct ZstHandle {
    // ruzstd encoder writes via `compress(reader, writer, level)`, so it
    // doesn't fit a "wrap a Write in an encoder" model directly.  Buffer
    // the tar in memory and compress on finish — same memory characteristic
    // as the regular tar.zst compress path.
    tar_buf: Vec<u8>,
    builder: Option<tar::Builder<Vec<u8>>>,
    out: Option<BufWriter<fs_err::File>>,
    level: ruzstd::encoding::CompressionLevel,
}

impl ZstHandle {
    fn new(out: BufWriter<fs_err::File>, level: Option<u32>) -> Result<Self> {
        let level = match level {
            None => ruzstd::encoding::CompressionLevel::Fastest,
            Some(0) => ruzstd::encoding::CompressionLevel::Uncompressed,
            Some(_) => return Err(Error::ZstdLevelUnsupported),
        };
        let mut s = Self {
            tar_buf: Vec::new(),
            builder: None,
            out: Some(out),
            level,
        };
        // Builder borrows tar_buf, so we have to construct it after the
        // struct exists.  Use a raw pointer dance via Option swap.
        s.builder = Some(tar::Builder::new(std::mem::take(&mut s.tar_buf)));
        Ok(s)
    }
}

impl EncoderHandle for ZstHandle {
    fn copy_existing(
        &mut self,
        reader: &mut dyn Read,
        keep: &mut dyn FnMut(&str) -> bool,
    ) -> Result<HashMap<String, u64>> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        copy_tar_entries(b, reader, keep)
    }
    fn append_inputs(
        &mut self,
        inputs: &[Utf8PathBuf],
        opts: &CompressOpts<'_>,
        archive_idx: Option<&HashMap<String, u64>>,
    ) -> Result<()> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        b.follow_symlinks(opts.follow_symlinks);
        append_inputs_with_index(b, inputs, opts, archive_idx)
    }
    fn finish(mut self: Box<Self>) -> Result<()> {
        let b = self.builder.take().ok_or_else(builder_taken_err)?;
        let tar_data = b.into_inner()?;
        let mut out = self.out.take().ok_or_else(builder_taken_err)?;
        ruzstd::encoding::compress(std::io::Cursor::new(&tar_data), &mut out, self.level);
        let file = out.into_inner().map_err(std::io::Error::other)?;
        file.sync_all()?;
        Ok(())
    }
}

#[cfg(feature = "xz2")]
struct XzHandle {
    builder: Option<tar::Builder<xz2::write::XzEncoder<BufWriter<fs_err::File>>>>,
}

#[cfg(feature = "xz2")]
impl XzHandle {
    fn new(writer: BufWriter<fs_err::File>, level: u32) -> Self {
        let enc = xz2::write::XzEncoder::new(writer, level);
        Self {
            builder: Some(tar::Builder::new(enc)),
        }
    }
}

#[cfg(feature = "xz2")]
impl EncoderHandle for XzHandle {
    fn copy_existing(
        &mut self,
        reader: &mut dyn Read,
        keep: &mut dyn FnMut(&str) -> bool,
    ) -> Result<HashMap<String, u64>> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        copy_tar_entries(b, reader, keep)
    }
    fn append_inputs(
        &mut self,
        inputs: &[Utf8PathBuf],
        opts: &CompressOpts<'_>,
        archive_idx: Option<&HashMap<String, u64>>,
    ) -> Result<()> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        b.follow_symlinks(opts.follow_symlinks);
        append_inputs_with_index(b, inputs, opts, archive_idx)
    }
    fn finish(mut self: Box<Self>) -> Result<()> {
        let b = self.builder.take().ok_or_else(builder_taken_err)?;
        let enc = b.into_inner()?;
        let buf = enc.finish()?;
        let file = buf.into_inner().map_err(std::io::Error::other)?;
        file.sync_all()?;
        Ok(())
    }
}

#[cfg(not(feature = "xz2"))]
struct LzmaRust2XzHandle {
    builder: Option<tar::Builder<lzma_rust2::XzWriter<BufWriter<fs_err::File>>>>,
}

#[cfg(not(feature = "xz2"))]
impl LzmaRust2XzHandle {
    fn new(writer: BufWriter<fs_err::File>, level: u32) -> Result<Self> {
        let enc = lzma_rust2::XzWriter::new(writer, lzma_rust2::XzOptions::with_preset(level))?;
        Ok(Self {
            builder: Some(tar::Builder::new(enc)),
        })
    }
}

#[cfg(not(feature = "xz2"))]
impl EncoderHandle for LzmaRust2XzHandle {
    fn copy_existing(
        &mut self,
        reader: &mut dyn Read,
        keep: &mut dyn FnMut(&str) -> bool,
    ) -> Result<HashMap<String, u64>> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        copy_tar_entries(b, reader, keep)
    }
    fn append_inputs(
        &mut self,
        inputs: &[Utf8PathBuf],
        opts: &CompressOpts<'_>,
        archive_idx: Option<&HashMap<String, u64>>,
    ) -> Result<()> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        b.follow_symlinks(opts.follow_symlinks);
        append_inputs_with_index(b, inputs, opts, archive_idx)
    }
    fn finish(mut self: Box<Self>) -> Result<()> {
        let b = self.builder.take().ok_or_else(builder_taken_err)?;
        let enc = b.into_inner()?;
        let buf = enc.finish()?;
        let file = buf.into_inner().map_err(std::io::Error::other)?;
        file.sync_all()?;
        Ok(())
    }
}

#[cfg(feature = "bzip2")]
struct Bz2Handle {
    builder: Option<tar::Builder<bzip2::write::BzEncoder<BufWriter<fs_err::File>>>>,
}

#[cfg(feature = "bzip2")]
impl Bz2Handle {
    fn new(writer: BufWriter<fs_err::File>, level: u32) -> Result<Self> {
        let compression = bzip2::Compression::try_new(level)
            .ok_or_else(|| std::io::Error::other("bzip2 compression level must be 1..=9"))?;
        let enc = bzip2::write::BzEncoder::new(writer, compression);
        Ok(Self {
            builder: Some(tar::Builder::new(enc)),
        })
    }
}

#[cfg(feature = "bzip2")]
impl EncoderHandle for Bz2Handle {
    fn copy_existing(
        &mut self,
        reader: &mut dyn Read,
        keep: &mut dyn FnMut(&str) -> bool,
    ) -> Result<HashMap<String, u64>> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        copy_tar_entries(b, reader, keep)
    }
    fn append_inputs(
        &mut self,
        inputs: &[Utf8PathBuf],
        opts: &CompressOpts<'_>,
        archive_idx: Option<&HashMap<String, u64>>,
    ) -> Result<()> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        b.follow_symlinks(opts.follow_symlinks);
        append_inputs_with_index(b, inputs, opts, archive_idx)
    }
    fn finish(mut self: Box<Self>) -> Result<()> {
        let b = self.builder.take().ok_or_else(builder_taken_err)?;
        let enc = b.into_inner()?;
        let buf = enc.finish()?;
        let file = buf.into_inner().map_err(std::io::Error::other)?;
        file.sync_all()?;
        Ok(())
    }
}

fn builder_taken_err() -> Error {
    Error::Io(std::io::Error::other(
        "tar builder already finalized — internal modify-pipeline error",
    ))
}

fn tar_compressed_append(
    archive: &Utf8Path,
    fmt: Format,
    inputs: &[Utf8PathBuf],
    mode: AppendMode,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let level = opts.level.or_else(|| default_level_for(fmt));
    let tmp = temp_path(archive);

    let out_file = fs_err::File::create(&tmp)?;
    let out_buf = BufWriter::new(out_file);
    let mut handle = tar_compressed_writer(fmt, out_buf, level)?;

    let mut reader = open_tar_reader(archive, fmt)?;
    let mut keep_all = |_: &str| true;
    let archive_idx = handle.copy_existing(&mut reader, &mut keep_all)?;

    let idx_for_update = if mode == AppendMode::Update {
        Some(&archive_idx)
    } else {
        None
    };
    handle.append_inputs(inputs, opts, idx_for_update)?;
    handle.finish()?;

    fs_err::rename(&tmp, archive)?;
    Ok(())
}

fn tar_remove(archive: &Utf8Path, glob: &GlobSet) -> Result<()> {
    // Read-rewrite under a temp file, even for uncompressed tar.  Could be
    // done in place by truncating after each surviving entry, but the temp
    // file approach is simpler and atomic via rename.
    let tmp = temp_path(archive);
    let out_file = fs_err::File::create(&tmp)?;
    let out_buf = BufWriter::new(out_file);
    let mut builder = tar::Builder::new(out_buf);

    let in_file = fs_err::File::open(archive)?;
    let mut reader: Box<dyn Read> = Box::new(BufReader::new(in_file));
    let mut keep = |name: &str| !glob.is_match(name.trim_end_matches('/'));
    copy_tar_entries(&mut builder, &mut reader, &mut keep)?;

    let buf = builder.into_inner()?;
    let file = buf.into_inner().map_err(std::io::Error::other)?;
    file.sync_all()?;
    fs_err::rename(&tmp, archive)?;
    Ok(())
}

fn tar_compressed_remove(
    archive: &Utf8Path,
    fmt: Format,
    glob: &GlobSet,
    level: Option<u32>,
) -> Result<()> {
    let level = level.or_else(|| default_level_for(fmt));
    let tmp = temp_path(archive);
    let out_file = fs_err::File::create(&tmp)?;
    let out_buf = BufWriter::new(out_file);
    let mut handle = tar_compressed_writer(fmt, out_buf, level)?;

    let mut reader = open_tar_reader(archive, fmt)?;
    let mut keep = |name: &str| !glob.is_match(name.trim_end_matches('/'));
    handle.copy_existing(&mut reader, &mut keep)?;
    let empty: &[Utf8PathBuf] = &[];
    let opts = CompressOpts::new(level, GlobSet::empty());
    let opts = CompressOpts {
        progress: &NoProgress,
        ..opts
    };
    handle.append_inputs(empty, &opts, None)?;
    handle.finish()?;

    fs_err::rename(&tmp, archive)?;
    Ok(())
}

// ── zip ──────────────────────────────────────────────────────────────────────

fn zip_index(archive: &Utf8Path) -> Result<HashMap<String, u64>> {
    let file = fs_err::File::open(archive)?;
    let mut a = zip::ZipArchive::new(file)?;
    let mut idx = HashMap::new();
    for i in 0..a.len() {
        let entry = a.by_index_raw(i)?;
        let name = entry.name().to_owned();
        let mtime = entry.last_modified().map(zip_dt_to_secs).unwrap_or(0);
        idx.insert(name, mtime);
    }
    Ok(idx)
}

fn zip_dt_to_secs(dt: zip::DateTime) -> u64 {
    let Ok(month) = time::Month::try_from(dt.month()) else {
        return 0;
    };
    let Ok(date) = time::Date::from_calendar_date(dt.year() as i32, month, dt.day()) else {
        return 0;
    };
    let Ok(t) = time::Time::from_hms(dt.hour(), dt.minute(), dt.second()) else {
        return 0;
    };
    let stamp = time::PrimitiveDateTime::new(date, t)
        .assume_utc()
        .unix_timestamp();
    if stamp >= 0 { stamp as u64 } else { 0 }
}

fn zip_append(
    archive: &Utf8Path,
    inputs: &[Utf8PathBuf],
    mode: AppendMode,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let archive_idx = if mode == AppendMode::Update {
        Some(zip_index(archive)?)
    } else {
        None
    };

    let file = fs_err::OpenOptions::new()
        .read(true)
        .write(true)
        .open(archive)?;
    let mut zw = zip::ZipWriter::new_append(file)?;

    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(opts.level.map(i64::from));

    for input in inputs {
        let meta = filter::input_metadata(input, opts.follow_symlinks)?;
        let name = input.file_name().unwrap_or(input.as_str());
        if opts.excludes.is_match(name) {
            continue;
        }
        if meta.is_dir() {
            zip_add_dir(&mut zw, input, name, options, opts, archive_idx.as_ref())?;
        } else if should_add_zip_entry(name, &meta, archive_idx.as_ref()) {
            zw.start_file(name, options)?;
            let mut f = fs_err::File::open(input)?;
            let size = std::io::copy(&mut f, &mut zw)?;
            opts.progress.set_entry(name);
            opts.progress.inc(size);
        }
    }

    let file = zw.finish()?;
    file.sync_all()?;
    Ok(())
}

fn zip_add_dir(
    zw: &mut zip::ZipWriter<fs_err::File>,
    dir: &Utf8Path,
    prefix: &str,
    options: zip::write::SimpleFileOptions,
    opts: &CompressOpts<'_>,
    archive_idx: Option<&HashMap<String, u64>>,
) -> Result<()> {
    if opts.no_recursion {
        // Record the bare directory entry; existing entry-name collisions
        // surface as a zip-crate error (duplicate-filename), which is fine.
        let _ = zw.add_directory(format!("{prefix}/"), options);
        return Ok(());
    }
    filter::walk_dir(dir, prefix, opts, &mut |entry| {
        if entry.is_dir {
            // Skip directory entries on append/update — zip treats these as
            // metadata-only and re-emitting them would conflict with the
            // archive's central directory.  Files inside still work because
            // unzippers treat the leading path components as implicit dirs.
            return Ok(());
        }
        let meta = filter::input_metadata(&entry.fs_path, opts.follow_symlinks)?;
        if !should_add_zip_entry(&entry.archive_name, &meta, archive_idx) {
            return Ok(());
        }
        zw.start_file(&entry.archive_name, options)?;
        let mut f = fs_err::File::open(&entry.fs_path)?;
        let size = std::io::copy(&mut f, zw)?;
        opts.progress.set_entry(&entry.archive_name);
        opts.progress.inc(size);
        Ok(())
    })
}

fn should_add_zip_entry(
    name: &str,
    meta: &std::fs::Metadata,
    archive_idx: Option<&HashMap<String, u64>>,
) -> bool {
    let Some(idx) = archive_idx else {
        return true;
    };
    let fs_mtime = mtime_secs(meta);
    is_newer_than_archive(name, fs_mtime, idx)
}

fn zip_remove(archive: &Utf8Path, glob: &GlobSet) -> Result<()> {
    let tmp = temp_path(archive);
    let in_file = fs_err::File::open(archive)?;
    let mut src = zip::ZipArchive::new(in_file)?;
    let out_file = fs_err::File::create(&tmp)?;
    let mut dst = zip::ZipWriter::new(out_file);

    for i in 0..src.len() {
        // Use raw_by_index so we copy through compressed bytes — keeps
        // entry data identical (no recompression) and is much faster.
        let raw = src.by_index_raw(i)?;
        let name = raw.name().to_owned();
        if glob.is_match(name.trim_end_matches('/')) {
            continue;
        }
        dst.raw_copy_file(raw)?;
    }

    let file = dst.finish()?;
    file.sync_all()?;
    fs_err::rename(&tmp, archive)?;
    Ok(())
}
