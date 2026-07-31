//! `append`, `update`, and `remove` operations across archive backends.
//!
//! ## Strategy by format
//!
//! | Format        | Append / Update                | Remove        |
//! |---------------|--------------------------------|---------------|
//! | tar           | in-place (seek past EOF marker)| read-rewrite  |
//! | tar.gz/etc.   | read-rewrite (compressed layer cannot be patched) | read-rewrite |
//! | zip           | read-rewrite (`ZipWriter` cannot re-add an archived name) | read-rewrite |
//! | 7z            | unsupported — `sevenz-rust2` does not expose write-into-existing |
//!
//! All read-rewrite paths write to `<archive>.tmp.rzappend` in the same
//! directory and atomically rename on success, so a partial write never
//! truncates the user's archive.  The in-place tar path cannot use a rename,
//! so it rolls the file back to its pre-append length instead.

use std::collections::{HashMap, HashSet};
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
    // Stat every input up front: the tar backend mutates the archive before it
    // reaches the walk, so an unreadable path must abort while the archive is
    // still untouched.
    let inputs = &filter::validate_inputs(inputs, opts)?;
    match fmt {
        Format::Tar => tar_append(archive, inputs, mode, opts),
        Format::TarGz | Format::TarZst | Format::TarXz => {
            tar_compressed_append(archive, fmt, inputs, mode, opts)
        }
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => tar_compressed_append(archive, fmt, inputs, mode, opts),
        #[cfg(not(feature = "bzip2"))]
        Format::TarBz2 => Err(Error::FormatFeatureDisabled {
            format: fmt.to_string(),
            feature: "bzip2",
        }),
        Format::Zip => zip_append(archive, inputs, mode, opts),
        Format::SevenZ => Err(Error::ModifyUnsupported {
            operation: op,
            format: fmt.to_string(),
        }),
    }
}

pub fn remove(
    archive: &Utf8Path,
    fmt: Format,
    patterns: &[String],
    level: Option<u32>,
) -> Result<()> {
    let glob = filter::build_glob_set(patterns)?;
    match fmt {
        Format::Tar => tar_remove(archive, &glob),
        Format::TarGz | Format::TarZst | Format::TarXz => {
            tar_compressed_remove(archive, fmt, &glob, level)
        }
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => tar_compressed_remove(archive, fmt, &glob, level),
        #[cfg(not(feature = "bzip2"))]
        Format::TarBz2 => Err(Error::FormatFeatureDisabled {
            format: fmt.to_string(),
            feature: "bzip2",
        }),
        Format::Zip => zip_remove(archive, &glob),
        Format::SevenZ => Err(Error::ModifyUnsupported {
            operation: "remove",
            format: fmt.to_string(),
        }),
    }
}

// ── tar (uncompressed) ───────────────────────────────────────────────────────

fn tar_append(
    archive: &Utf8Path,
    inputs: &[Utf8PathBuf],
    mode: AppendMode,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    // One block-level scan yields both the update index and the physical end
    // of the last entry — where the trailing zero blocks begin, which the
    // append overwrites with new entries and a fresh terminator.  The scan
    // is header-aware (GNU sparse, pax size overrides, long names), which
    // tar-rs iteration is not: `Entry::size()` reports a sparse entry's
    // expanded logical size, and trusting it here used to extend the archive
    // across the hole.
    let (archive_idx, body_end) = {
        let file = fs_err::File::open(archive)?;
        let mut buf = BufReader::new(file);
        let scan = crate::tar_raw::scan(&mut buf)?;
        let idx = (mode == AppendMode::Update).then(|| {
            scan.entries
                .iter()
                .map(|e| (written_name(&e.name), e.mtime))
                .collect::<HashMap<String, u64>>()
        });
        (idx, scan.body_end)
    };

    let opts = filtered_opts(opts, archive_idx.as_ref());
    let res = tar_write_appended(archive, body_end, inputs, &opts, archive_idx.as_ref());
    if res.is_err() {
        // A failure part-way through the walk leaves the new entries half
        // written, and the abandoned builder's `Drop` stamps an EOF terminator
        // on top of them.  Cutting back to `body_end` and re-terminating
        // yields an archive holding exactly the entries it started with.
        let _ = tar_truncate_and_terminate(archive, body_end);
    }
    res
}

/// Overwrite the trailing zero blocks of `archive` with `inputs` and a fresh
/// EOF terminator.  Mutates the archive in place, so a failure here must be
/// followed by [`tar_truncate_and_terminate`].
fn tar_write_appended(
    archive: &Utf8Path,
    body_end: u64,
    inputs: &[Utf8PathBuf],
    opts: &CompressOpts<'_>,
    archive_idx: Option<&HashMap<String, u64>>,
) -> Result<()> {
    let mut file = fs_err::OpenOptions::new()
        .read(true)
        .write(true)
        .open(archive)?;
    file.seek(SeekFrom::Start(body_end))?;
    // Truncate any trailing zero blocks so Builder::finish writes the only
    // remaining EOF terminator and the file's logical length is correct.
    file.set_len(body_end)?;

    let buf = BufWriter::new(file);
    let mut builder = tar::Builder::new(buf);
    builder.follow_symlinks(opts.follow_symlinks);
    append_inputs_with_index(&mut builder, inputs, opts, archive_idx)?;
    let buf = builder.into_inner()?;
    let file = buf.into_inner().map_err(std::io::Error::other)?;
    file.sync_all()?;
    Ok(())
}

/// Cut `archive` back to `body_end` and write a fresh two-block EOF
/// terminator, leaving a valid tar holding exactly the entries that ended at
/// `body_end`.
fn tar_truncate_and_terminate(archive: &Utf8Path, body_end: u64) -> Result<()> {
    let mut file = fs_err::OpenOptions::new()
        .read(true)
        .write(true)
        .open(archive)?;
    file.set_len(body_end)?;
    file.seek(SeekFrom::Start(body_end))?;
    file.write_all(&[0u8; 2 * TAR_BLOCK as usize])?;
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
        ignore_failed_read: opts.ignore_failed_read,
        password: opts.password.clone(),
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
        let name = filter::input_base_name(input)?;
        if opts.excludes.is_match(&name) {
            continue;
        }
        if meta.is_dir() {
            // For directories we walk children individually so we can apply
            // the per-name index lookup at file granularity.
            walk_and_append_with_index(builder, input, &name, opts, idx)?;
        } else {
            if !update_wants(&name, &meta, idx) {
                continue;
            }
            append_one_file(builder, input, &name, opts)?;
            opts.progress.set_entry(&name);
            opts.progress.inc(meta.len());
        }
    }
    Ok(())
}

/// The name tar actually stores for a walked path: `tar::Header::set_path`
/// drops `.` components (and redundant separators) at encoding time, so
/// `update <archive> .` walks names like `./f.txt` while the archive holds
/// `f.txt`.  Every index key, gate lookup, and keep decision goes through
/// this so all three agree on the written spelling; a bare `.` (the root
/// entry of an archive built from `.`) is kept, matching what the builder
/// emits for it.  Trailing-slash normalization falls out for free: a
/// trailing `/` is an empty component.
fn written_name(name: &str) -> String {
    let parts: Vec<&str> = name
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

/// Decide whether an update-walk entry gets (re)written: strictly newer on
/// disk than the archive's copy, or absent from the archive entirely.
///
/// The single decision point shared by [`walk_and_append_with_index`] (which
/// writes) and [`plan_update_names`] (which predicts what will be written so
/// the copy pass can drop the stale copies) — one rule, so the two walks
/// cannot drift.  Directory entries are gated the same way as files: their
/// unconditional re-emission used to duplicate every directory on every
/// update.
fn update_wants(name: &str, meta: &std::fs::Metadata, idx: &HashMap<String, u64>) -> bool {
    is_newer_than_archive(&written_name(name), mtime_secs(meta), idx)
}

fn walk_and_append_with_index<W: Write>(
    builder: &mut tar::Builder<W>,
    dir: &Utf8Path,
    prefix: &str,
    opts: &CompressOpts<'_>,
    idx: &HashMap<String, u64>,
) -> Result<()> {
    if opts.no_recursion {
        let meta = filter::input_metadata(dir, opts.follow_symlinks)?;
        if update_wants(prefix, &meta, idx) {
            builder.append_dir(prefix, dir.as_std_path())?;
        }
        return Ok(());
    }
    filter::walk_dir(dir, prefix, opts, &mut |entry| {
        let meta = filter::input_metadata(&entry.fs_path, opts.follow_symlinks)?;
        if !update_wants(&entry.archive_name, &meta, idx) {
            return Ok(());
        }
        if entry.is_dir {
            builder.append_dir(&entry.archive_name, entry.fs_path.as_std_path())?;
        } else {
            append_one_file(builder, &entry.fs_path, &entry.archive_name, opts)?;
            opts.progress.set_entry(&entry.archive_name);
            opts.progress.inc(meta.len());
        }
        Ok(())
    })
}

/// Predict the archive names an update walk over `inputs` will write, so the
/// read-rewrite copy pass can skip the entries being superseded instead of
/// carrying both versions.  Mirrors [`append_inputs_with_index`] exactly via
/// the shared [`update_wants`] gate; names are normalized without trailing
/// slashes to match the index keys.
fn plan_update_names(
    inputs: &[Utf8PathBuf],
    opts: &CompressOpts<'_>,
    idx: &HashMap<String, u64>,
) -> Result<HashSet<String>> {
    let mut planned = HashSet::new();
    for input in inputs {
        let meta = filter::input_metadata(input, opts.follow_symlinks)?;
        let name = filter::input_base_name(input)?;
        if opts.excludes.is_match(&name) {
            continue;
        }
        if meta.is_dir() {
            if opts.no_recursion {
                if update_wants(&name, &meta, idx) {
                    planned.insert(written_name(&name));
                }
                continue;
            }
            filter::walk_dir(input, &name, opts, &mut |entry| {
                let entry_meta = filter::input_metadata(&entry.fs_path, opts.follow_symlinks)?;
                if update_wants(&entry.archive_name, &entry_meta, idx) {
                    planned.insert(written_name(&entry.archive_name));
                }
                Ok(())
            })?;
        } else if update_wants(&name, &meta, idx) {
            planned.insert(written_name(&name));
        }
    }
    Ok(planned)
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
        // Must use the multi-frame decoder: `tar_zst::compress` emits one
        // independent zstd frame per 1 MiB chunk, so a single-frame decoder
        // here would stop at the first frame and silently truncate the archive
        // during the read-rewrite.
        Format::TarZst => Ok(Box::new(crate::tar_zst::MultiFrameDecoder::new(buf)?)),
        Format::TarXz => xz_read(buf),
        // MultiBzDecoder for the same reason as MultiGzDecoder above:
        // pbzip2/lbzip2 output is a train of independent streams, and the
        // single-stream decoder would silently truncate the read-rewrite at
        // the first stream boundary.
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => Ok(Box::new(bzip2::read::MultiBzDecoder::new(buf))),
        #[cfg(not(feature = "bzip2"))]
        Format::TarBz2 => Err(Error::FormatFeatureDisabled {
            format: fmt.to_string(),
            feature: "bzip2",
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
        Format::TarGz => Ok(Box::new(GzHandle::new(
            writer,
            crate::tar_gz::validate_level(level.unwrap_or(6))?,
        ))),
        Format::TarZst => Ok(Box::new(ZstHandle::new(writer, level)?)),
        Format::TarXz => xz_writer(writer, level.unwrap_or(6)),
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => Ok(Box::new(Bz2Handle::new(writer, level.unwrap_or(6))?)),
        #[cfg(not(feature = "bzip2"))]
        Format::TarBz2 => Err(Error::FormatFeatureDisabled {
            format: fmt.to_string(),
            feature: "bzip2",
        }),
        _ => Err(Error::ModifyUnsupported {
            operation: "rewrite",
            format: fmt.to_string(),
        }),
    }
}

#[cfg(feature = "xz2")]
fn xz_writer(writer: BufWriter<fs_err::File>, level: u32) -> Result<Box<dyn EncoderHandle>> {
    let level = crate::tar_xz::validate_level(level)?;
    Ok(Box::new(XzHandle::new(writer, level)))
}

#[cfg(not(feature = "xz2"))]
fn xz_writer(writer: BufWriter<fs_err::File>, level: u32) -> Result<Box<dyn EncoderHandle>> {
    let level = crate::tar_xz::validate_level(level)?;
    Ok(Box::new(LzmaRust2XzHandle::new(writer, level)?))
}

/// Trait for opaque encoder pipelines used by `tar_compressed_append`.  Each
/// implementation owns a tar builder pointed at a buffered, compressed
/// writer and exposes only the pieces the caller needs: a mutable reference
/// to the builder for entry writes, and a `finish` that flushes everything
/// down through the file.
trait EncoderHandle {
    /// Read-rewrite copy of every existing entry from `reader` into the
    /// internal writer, then return so the caller can append new entries via
    /// [`Self::append_inputs`].
    ///
    /// `keep` returns `true` when the entry should be carried over.  Kept
    /// entry groups are copied byte-for-byte via [`crate::tar_raw`], so GNU
    /// sparse maps, pax records, and long-name extensions survive verbatim.
    fn copy_existing(
        &mut self,
        reader: &mut dyn Read,
        keep: &mut dyn FnMut(&str) -> bool,
    ) -> Result<()>;
    fn append_inputs(
        &mut self,
        inputs: &[Utf8PathBuf],
        opts: &CompressOpts<'_>,
        archive_idx: Option<&HashMap<String, u64>>,
    ) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

/// Copy every entry group `keep` approves from `reader` straight into the
/// builder's underlying writer, byte-for-byte, via [`crate::tar_raw`].
///
/// Writing past the builder is safe here: nothing has been appended yet, so
/// the builder holds no partial state, and raw groups are whole 512-byte
/// blocks — exactly what `Builder::append_*` would produce.  The byte-exact
/// copy is what makes GNU sparse entries survive: their physical layout
/// (base header, extended sparse blocks, compacted data) has no lossless
/// representation through the tar crate's high-level API.
fn copy_raw_entries<W: Write>(
    builder: &mut tar::Builder<W>,
    reader: &mut dyn Read,
    keep: &mut dyn FnMut(&str) -> bool,
) -> Result<()> {
    crate::tar_raw::copy_entries(reader, builder.get_mut(), keep)
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
    ) -> Result<()> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        copy_raw_entries(b, reader, keep)
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
    ) -> Result<()> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        copy_raw_entries(b, reader, keep)
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
        // Compress into a buffer first: `ruzstd` writes to its drain with
        // `.unwrap()`, so writing straight to `out` would panic on a real I/O
        // error (ENOSPC etc.) instead of propagating it.  Writing to a `Vec` is
        // infallible; the fallible write to the file happens here where `?` can
        // catch it.
        let mut compressed = Vec::new();
        ruzstd::encoding::compress(std::io::Cursor::new(&tar_data), &mut compressed, self.level);
        out.write_all(&compressed)?;
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
    ) -> Result<()> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        copy_raw_entries(b, reader, keep)
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
    ) -> Result<()> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        copy_raw_entries(b, reader, keep)
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
    ) -> Result<()> {
        let b = self.builder.as_mut().ok_or_else(builder_taken_err)?;
        copy_raw_entries(b, reader, keep)
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
    let tmp = temp_path(archive);
    let res = tar_compressed_append_into(archive, &tmp, fmt, inputs, mode, opts);
    if res.is_err() {
        let _ = fs_err::remove_file(&tmp);
    }
    res
}

fn tar_compressed_append_into(
    archive: &Utf8Path,
    tmp: &Utf8Path,
    fmt: Format,
    inputs: &[Utf8PathBuf],
    mode: AppendMode,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let level = opts.level.or_else(|| default_level_for(fmt));

    // Update needs the archive's name→mtime index *before* the copy pass:
    // the keep closure below drops the stale copies of entries about to be
    // rewritten.  A keep-all copy used to leave both versions in the archive
    // — extraction still yielded the newer one (last wins), but list/info
    // reported doubled counts and the archive grew without bound.  The extra
    // decompression pass is the price of a correct plan.
    let (archive_idx, superseded) = if mode == AppendMode::Update {
        let mut reader = open_tar_reader(archive, fmt)?;
        let scan = crate::tar_raw::scan(&mut reader)?;
        let idx: HashMap<String, u64> = scan
            .entries
            .iter()
            .map(|e| (written_name(&e.name), e.mtime))
            .collect();
        let mut planned = plan_update_names(inputs, opts, &idx)?;
        // Rescue any planned drop that a *kept* hard-link entry still points
        // at: the re-appended copy lands after the link, and extractors
        // resolve hard links against what is already on disk, so the link
        // would dangle.  Keeping the stale copy mirrors what plain `append`
        // always did — the appended version still wins by coming last.
        // Fixpoint: rescuing a link entry re-activates its own target.
        loop {
            let rescued: Vec<String> = scan
                .entries
                .iter()
                .filter(|e| !planned.contains(&written_name(&e.name)))
                .filter_map(|e| e.hardlink_target.as_deref().map(written_name))
                .filter(|target| planned.contains(target))
                .collect();
            if rescued.is_empty() {
                break;
            }
            for target in rescued {
                planned.remove(&target);
            }
        }
        (Some(idx), planned)
    } else {
        (None, HashSet::new())
    };

    let out_file = fs_err::File::create(tmp)?;
    let out_buf = BufWriter::new(out_file);
    let mut handle = tar_compressed_writer(fmt, out_buf, level)?;

    let mut reader = open_tar_reader(archive, fmt)?;
    let mut keep = |name: &str| !superseded.contains(&written_name(name));
    handle.copy_existing(&mut reader, &mut keep)?;

    handle.append_inputs(inputs, opts, archive_idx.as_ref())?;
    handle.finish()?;

    fs_err::rename(tmp, archive)?;
    Ok(())
}

fn tar_remove(archive: &Utf8Path, glob: &GlobSet) -> Result<()> {
    // Read-rewrite under a temp file, even for uncompressed tar.  Could be
    // done in place by truncating after each surviving entry, but the temp
    // file approach is simpler and atomic via rename.
    let tmp = temp_path(archive);
    let res = tar_remove_into(archive, &tmp, glob);
    if res.is_err() {
        let _ = fs_err::remove_file(&tmp);
    }
    res
}

fn tar_remove_into(archive: &Utf8Path, tmp: &Utf8Path, glob: &GlobSet) -> Result<()> {
    let out_file = fs_err::File::create(tmp)?;
    let out_buf = BufWriter::new(out_file);
    let mut builder = tar::Builder::new(out_buf);

    let in_file = fs_err::File::open(archive)?;
    let mut reader: Box<dyn Read> = Box::new(BufReader::new(in_file));
    let mut keep = |name: &str| !glob.is_match(name.trim_end_matches('/'));
    copy_raw_entries(&mut builder, &mut reader, &mut keep)?;

    let buf = builder.into_inner()?;
    let file = buf.into_inner().map_err(std::io::Error::other)?;
    file.sync_all()?;
    fs_err::rename(tmp, archive)?;
    Ok(())
}

fn tar_compressed_remove(
    archive: &Utf8Path,
    fmt: Format,
    glob: &GlobSet,
    level: Option<u32>,
) -> Result<()> {
    let tmp = temp_path(archive);
    let res = tar_compressed_remove_into(archive, &tmp, fmt, glob, level);
    if res.is_err() {
        let _ = fs_err::remove_file(&tmp);
    }
    res
}

fn tar_compressed_remove_into(
    archive: &Utf8Path,
    tmp: &Utf8Path,
    fmt: Format,
    glob: &GlobSet,
    level: Option<u32>,
) -> Result<()> {
    let level = level.or_else(|| default_level_for(fmt));
    let out_file = fs_err::File::create(tmp)?;
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

    fs_err::rename(tmp, archive)?;
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

/// An entry the append/update walk decided to write, resolved before the
/// output is opened so the rewrite knows which archived names it supersedes.
enum PlannedZipEntry {
    File {
        name: String,
        fs_path: Utf8PathBuf,
    },
    /// Symlink stored as a symlink entry (compress-side semantics) rather
    /// than being dereferenced into a regular file.
    Symlink {
        name: String,
        fs_path: Utf8PathBuf,
    },
    /// Bare directory entry, emitted only under `--no-recursion`.
    Dir {
        name: String,
    },
}

impl PlannedZipEntry {
    fn name(&self) -> &str {
        match self {
            Self::File { name, .. } | Self::Symlink { name, .. } | Self::Dir { name } => name,
        }
    }
}

/// Plan a walked filesystem object as the right zip entry kind: a symlink
/// entry when it is a symlink and links are not being followed, a regular
/// file otherwise.
fn plan_zip_file(name: String, fs_path: Utf8PathBuf, follow_symlinks: bool) -> Result<PlannedZipEntry> {
    let is_symlink =
        !follow_symlinks && fs_err::symlink_metadata(&fs_path)?.file_type().is_symlink();
    Ok(if is_symlink {
        PlannedZipEntry::Symlink { name, fs_path }
    } else {
        PlannedZipEntry::File { name, fs_path }
    })
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

    let planned = plan_zip_entries(inputs, opts, archive_idx.as_ref())?;
    if planned.is_empty() {
        return Ok(());
    }

    // `ZipWriter::new_append` seeds its name set from the central directory and
    // rejects any name already there, which is exactly the case `update` hits.
    // Rewriting instead lets a planned entry supersede the archived copy —
    // Info-ZIP's behaviour — and keeps the original intact on failure.
    let tmp = temp_path(archive);
    let res = zip_rewrite_with(archive, &tmp, &planned, opts);
    if res.is_err() {
        let _ = fs_err::remove_file(&tmp);
    }
    res
}

/// Resolve which filesystem entries an append/update would write, without
/// touching the archive.
fn plan_zip_entries(
    inputs: &[Utf8PathBuf],
    opts: &CompressOpts<'_>,
    archive_idx: Option<&HashMap<String, u64>>,
) -> Result<Vec<PlannedZipEntry>> {
    let mut planned = Vec::new();
    for input in inputs {
        let meta = filter::input_metadata(input, opts.follow_symlinks)?;
        let name = filter::input_base_name(input)?;
        if opts.excludes.is_match(&name) {
            continue;
        }
        if meta.is_dir() {
            if opts.no_recursion {
                planned.push(PlannedZipEntry::Dir { name });
                continue;
            }
            filter::walk_dir(input, &name, opts, &mut |entry| {
                if entry.is_dir {
                    // Directory entries carry no data; unzippers materialise
                    // the leading path components of each file anyway, so the
                    // archive's existing ones are left alone.
                    return Ok(());
                }
                let meta = filter::input_metadata(&entry.fs_path, opts.follow_symlinks)?;
                if !should_add_zip_entry(&entry.archive_name, &meta, archive_idx) {
                    return Ok(());
                }
                if filter::skip_unarchivable_special(&meta, &entry.archive_name) {
                    return Ok(());
                }
                planned.push(plan_zip_file(
                    entry.archive_name,
                    entry.fs_path,
                    opts.follow_symlinks,
                )?);
                Ok(())
            })?;
        } else if should_add_zip_entry(&name, &meta, archive_idx) {
            if filter::skip_unarchivable_special(&meta, &name) {
                continue;
            }
            planned.push(plan_zip_file(name, input.clone(), opts.follow_symlinks)?);
        }
    }
    Ok(planned)
}

/// Carry one archived entry into a rewrite's output.
///
/// `raw_copy_file` rebuilds the entry options from `ZipFile::options()`, whose
/// `unix_permissions` masks the mode to `0o777` before the writer re-ORs
/// `S_IFREG` — flattening a carried symlink into a regular file whose content
/// is the target path, and a directory entry into a zero-length regular file.
/// Those two kinds are re-written through the typed APIs (`add_symlink` /
/// `add_directory`), which set the right type bits; everything else keeps the
/// verbatim compressed-bytes copy.  An encrypted symlink cannot be re-read
/// without its password, so it falls back to the raw copy.
fn carry_zip_entry<R, W>(
    src: &mut zip::ZipArchive<R>,
    index: usize,
    dst: &mut zip::ZipWriter<W>,
) -> Result<()>
where
    R: Read + std::io::Seek,
    W: Write + std::io::Seek,
{
    let raw = src.by_index_raw(index)?;
    let name = raw.name().to_owned();
    let mode = raw.unix_mode();
    let mtime = raw.last_modified().filter(zip::DateTime::is_valid);
    let is_dir = raw.is_dir();
    let rewrite_symlink = raw.is_symlink() && !raw.encrypted();
    drop(raw);

    let mut options = zip::write::SimpleFileOptions::default();
    if let Some(dt) = mtime {
        options = options.last_modified_time(dt);
    }
    if let Some(mode) = mode {
        options = options.unix_permissions(mode);
    }

    if is_dir {
        dst.add_directory(name, options)?;
    } else if rewrite_symlink {
        let entry = src.by_index(index)?;
        let mut target = Vec::new();
        entry
            .take(crate::zip::MAX_SYMLINK_TARGET + 1)
            .read_to_end(&mut target)?;
        if target.len() as u64 > crate::zip::MAX_SYMLINK_TARGET {
            return Err(Error::SymlinkTargetTooLong {
                path: name.into(),
                max: crate::zip::MAX_SYMLINK_TARGET,
            });
        }
        let target = String::from_utf8(target)
            .map_err(|e| Error::InvalidUtf8Path(String::from_utf8_lossy(e.as_bytes()).into_owned()))?;
        dst.add_symlink(name, target, options)?;
    } else {
        dst.raw_copy_file(src.by_index_raw(index)?)?;
    }
    Ok(())
}

/// Copy every archived entry the plan does not supersede into `tmp`, write the
/// planned entries after them, then rename `tmp` over `archive`.
fn zip_rewrite_with(
    archive: &Utf8Path,
    tmp: &Utf8Path,
    planned: &[PlannedZipEntry],
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let superseded: HashSet<&str> = planned.iter().map(PlannedZipEntry::name).collect();

    let in_file = fs_err::File::open(archive)?;
    let mut src = zip::ZipArchive::new(in_file)?;
    let out_file = fs_err::File::create(tmp)?;
    let mut dst = zip::ZipWriter::new(out_file);

    for i in 0..src.len() {
        let skip = superseded.contains(src.by_index_raw(i)?.name().trim_end_matches('/'));
        if skip {
            continue;
        }
        carry_zip_entry(&mut src, i, &mut dst)?;
    }

    let (method, level) = crate::zip::compression_settings(opts.level);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(method)
        .compression_level(level);

    for entry in planned {
        match entry {
            PlannedZipEntry::Dir { name } => {
                dst.add_directory(format!("{name}/"), options)?;
            }
            PlannedZipEntry::Symlink { name, fs_path } => {
                crate::zip::write_symlink_entry(&mut dst, fs_path, name, options, opts)?;
            }
            PlannedZipEntry::File { name, fs_path } => {
                let meta = filter::input_metadata(fs_path, opts.follow_symlinks)?;
                dst.start_file(name, crate::zip::with_unix_mode(options, &meta))?;
                let mut f = fs_err::File::open(fs_path)?;
                let size = std::io::copy(&mut f, &mut dst)?;
                opts.progress.set_entry(name);
                opts.progress.inc(size);
            }
        }
    }

    let file = dst.finish()?;
    file.sync_all()?;
    fs_err::rename(tmp, archive)?;
    Ok(())
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
    let res = zip_remove_into(archive, &tmp, glob);
    if res.is_err() {
        let _ = fs_err::remove_file(&tmp);
    }
    res
}

fn zip_remove_into(archive: &Utf8Path, tmp: &Utf8Path, glob: &GlobSet) -> Result<()> {
    let in_file = fs_err::File::open(archive)?;
    let mut src = zip::ZipArchive::new(in_file)?;
    let out_file = fs_err::File::create(tmp)?;
    let mut dst = zip::ZipWriter::new(out_file);

    for i in 0..src.len() {
        let name = src.by_index_raw(i)?.name().to_owned();
        if glob.is_match(name.trim_end_matches('/')) {
            continue;
        }
        carry_zip_entry(&mut src, i, &mut dst)?;
    }

    let file = dst.finish()?;
    file.sync_all()?;
    fs_err::rename(tmp, archive)?;
    Ok(())
}
