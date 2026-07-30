use std::collections::BTreeMap;
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use zip::write::SimpleFileOptions;
use zip::{AesMode, CompressionMethod, ZipArchive, ZipWriter};

use crate::error::{Error, Result};
use crate::filter;
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

/// Apply source-file metadata from `meta` to a zip `FileOptions`: the Unix
/// permission bits (notably the executable bit) on Unix, and the modification
/// time everywhere — the crate's default stamps the moment of compression,
/// which discards the source mtime entirely.  Timestamps outside the DOS
/// datetime range (pre-1980) keep the crate default.
///
/// Not used for symlink entries: the `zip` crate sets their mode (`S_IFLNK`)
/// itself, and overriding it would break symlink round-tripping.
pub(crate) fn with_unix_mode<'k>(
    options: zip::write::FileOptions<'k, ()>,
    meta: &std::fs::Metadata,
) -> zip::write::FileOptions<'k, ()> {
    let options = match zip_datetime_from_meta(meta) {
        Some(dt) => options.last_modified_time(dt),
        None => options,
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        options.unix_permissions(meta.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        options
    }
}

/// Convert a filesystem mtime into a zip `DateTime` (UTC components, the
/// inverse of [`zip_datetime_to_epoch`]).  `None` when the metadata carries no
/// usable time or it falls outside the representable 1980–2107 DOS range.
fn zip_datetime_from_meta(meta: &std::fs::Metadata) -> Option<zip::DateTime> {
    let secs = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let odt = time::OffsetDateTime::from_unix_timestamp(i64::try_from(secs).ok()?).ok()?;
    zip::DateTime::from_date_and_time(
        odt.year().try_into().ok()?,
        odt.month() as u8,
        odt.day(),
        odt.hour(),
        odt.minute(),
        odt.second(),
    )
    .ok()
}

/// Translate a requested compression level into the method/level pair the
/// `zip` crate accepts.
///
/// Level 0 — what `--store` maps to — means "no compression", but the crate's
/// deflate range starts at 1 and its `Stored` method rejects *any* explicit
/// level, so the method and the level have to be chosen together.
pub(crate) fn compression_settings(level: Option<u32>) -> (CompressionMethod, Option<i64>) {
    match level {
        Some(0) => (CompressionMethod::Stored, None),
        other => (CompressionMethod::Deflated, other.map(i64::from)),
    }
}

pub fn compress(inputs: &[Utf8PathBuf], output: &Utf8Path, opts: &CompressOpts<'_>) -> Result<()> {
    let inputs = filter::validate_inputs(inputs, opts)?;

    let file = fs_err::File::create(output)?;
    let result = write_archive(file, &inputs, opts);
    if result.is_err() {
        // `ZipWriter`'s `Drop` finalises whatever was written, so bailing out
        // mid-run would otherwise leave a valid-but-empty archive on disk.
        let _ = fs_err::remove_file(output);
    }
    result
}

fn write_archive(
    file: fs_err::File,
    inputs: &[Utf8PathBuf],
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let mut zip = ZipWriter::new(std::io::BufWriter::new(file));

    let (method, level) = compression_settings(opts.level);
    let base_options = SimpleFileOptions::default()
        .compression_method(method)
        .compression_level(level);

    // When encryption is active the lifetime of `FileOptions` is tied to the
    // password string, so we must hold the two branches separately.
    // When encryption is active the lifetime of `FileOptions` is tied to the
    // password string.  Both `FileOptions<'static, ()>` (no password) and
    // `FileOptions<'_, ()>` (with password) satisfy the same trait bounds, so
    // we dispatch through the same helpers — just from two branches to keep the
    // borrow checker happy about the lifetime of `options`.
    if let Some(ref pwd) = opts.password {
        let options = base_options.with_aes_encryption(AesMode::Aes256, pwd.as_str());
        for input in inputs {
            let meta = filter::input_metadata(input, opts.follow_symlinks)?;
            let name = input.file_name().unwrap_or(input.as_str());
            if !opts.follow_symlinks && meta.file_type().is_symlink() {
                write_symlink_entry(&mut zip, input, name, options, opts)?;
            } else if meta.is_dir() {
                if opts.no_recursion {
                    zip.add_directory(format!("{name}/"), with_unix_mode(options, &meta))?;
                } else {
                    add_dir_walked(&mut zip, input, name, options, opts)?;
                }
            } else {
                zip.start_file(name, with_unix_mode(options, &meta))?;
                let mut f = fs_err::File::open(input)?;
                let size = io::copy(&mut f, &mut zip)?;
                opts.progress.set_entry(name);
                opts.progress.inc(size);
            }
        }
    } else {
        for input in inputs {
            let meta = filter::input_metadata(input, opts.follow_symlinks)?;
            let name = input.file_name().unwrap_or(input.as_str());
            if !opts.follow_symlinks && meta.file_type().is_symlink() {
                write_symlink_entry(&mut zip, input, name, base_options, opts)?;
            } else if meta.is_dir() {
                if opts.no_recursion {
                    zip.add_directory(format!("{name}/"), with_unix_mode(base_options, &meta))?;
                } else {
                    add_dir_walked(&mut zip, input, name, base_options, opts)?;
                }
            } else {
                zip.start_file(name, with_unix_mode(base_options, &meta))?;
                let mut f = fs_err::File::open(input)?;
                let size = io::copy(&mut f, &mut zip)?;
                opts.progress.set_entry(name);
                opts.progress.inc(size);
            }
        }
    }

    let file = zip.finish()?.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    Ok(())
}

/// Walk a directory using [`filter::walk_dir`] and add entries to a zip archive.
/// Handles symlinks, regular files, and subdirectories.
fn add_dir_walked<'k>(
    zip: &mut ZipWriter<std::io::BufWriter<fs_err::File>>,
    dir: &Utf8Path,
    prefix: &str,
    options: zip::write::FileOptions<'k, ()>,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    filter::walk_dir(dir, prefix, opts, &mut |entry| {
        let link_meta = fs_err::symlink_metadata(&entry.fs_path)?;
        let is_symlink = !opts.follow_symlinks && link_meta.file_type().is_symlink();

        if is_symlink {
            write_symlink_entry(zip, &entry.fs_path, &entry.archive_name, options, opts)?;
        } else {
            // Store the mode of the object actually being archived — the target
            // when following a symlink, the entry itself otherwise.
            let meta = if opts.follow_symlinks && link_meta.file_type().is_symlink() {
                filter::input_metadata(&entry.fs_path, true)?
            } else {
                link_meta
            };
            let entry_options = with_unix_mode(options, &meta);
            if entry.is_dir {
                zip.add_directory(format!("{}/", entry.archive_name), entry_options)?;
            } else {
                zip.start_file(&entry.archive_name, entry_options)?;
                let mut f = fs_err::File::open(&entry.fs_path)?;
                let size = io::copy(&mut f, zip)?;
                opts.progress.set_entry(&entry.archive_name);
                opts.progress.inc(size);
            }
        }
        Ok(())
    })
}

/// Store a symlink as a symlink entry (POSIX-style, with `S_IFLNK` mode and
/// the link target as the entry content). The `zip` crate sets `0o777`
/// permissions by default; Windows unzip tools may materialise this as a
/// regular text file containing the target path.
fn write_symlink_entry<'k>(
    zip: &mut ZipWriter<std::io::BufWriter<fs_err::File>>,
    link_path: &Utf8Path,
    archive_name: &str,
    options: zip::write::FileOptions<'k, ()>,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let target = fs_err::read_link(link_path)?;
    let target_str = target
        .to_str()
        .ok_or_else(|| Error::InvalidUtf8Path(target.display().to_string()))?;
    zip.add_symlink_from_path(archive_name, target_str, options)?;
    opts.progress.set_entry(archive_name);
    opts.progress.inc(target_str.len() as u64);
    Ok(())
}

/// Upper bound on a symlink target this crate is willing to read.
///
/// Linux caps symlink targets at PATH_MAX (4096) and every other platform is
/// stricter, so double that is generous for anything legitimate while keeping
/// a hostile entry from becoming an unbounded read.
const MAX_SYMLINK_TARGET: u64 = 8 * 1024;

/// Extract a zip symlink entry to `out_path`.
///
/// On Unix, creates a real symlink via `std::os::unix::fs::symlink`.  On other
/// platforms, falls back to writing the link target as a plain text file —
/// mirroring what typical Windows unzip tools do when they encounter a POSIX
/// symlink entry they can't materialise.
///
/// Rejects absolute targets and any `..` component via
/// [`filter::safe_link_target`] — the same guard the tar path uses — so a later
/// entry cannot be written *through* a freshly-created symlink into territory
/// outside the output root (the classic symlink-based zip-slip).
///
/// When `existed` is true, any existing path at `out_path` is removed first,
/// because `symlink(2)` fails if the target already exists.
fn extract_symlink_entry(
    entry: &mut zip::read::ZipFile<'_, fs_err::File>,
    out_path: &Utf8Path,
    existed: bool,
    dest_path: &Utf8Path,
) -> Result<u64> {
    // Never size this from `entry.size()`.  That is the central directory's
    // uncompressed_size, which the crate returns verbatim and never validates
    // against the actual content, so a ZIP64 entry can declare u64::MAX with
    // six bytes of payload behind it.  A failed `Vec::with_capacity` calls
    // `handle_alloc_error`, which aborts rather than unwinding — no amount of
    // panic-free discipline in this crate can catch that.
    let mut target_bytes = Vec::new();
    let read = io::copy(
        &mut io::Read::take(entry, MAX_SYMLINK_TARGET),
        &mut target_bytes,
    )?;
    if read >= MAX_SYMLINK_TARGET {
        return Err(Error::SymlinkTargetTooLong {
            path: dest_path.to_owned(),
            max: MAX_SYMLINK_TARGET,
        });
    }

    let target = std::str::from_utf8(&target_bytes)
        .map_err(|_| Error::InvalidUtf8Path(dest_path.to_string()))?;

    filter::safe_link_target(dest_path.as_str(), target)?;

    if existed {
        fs_err::remove_file(out_path)?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, out_path)?;
    }
    #[cfg(not(unix))]
    {
        use std::io::Write;
        let mut f = fs_err::File::create(out_path)?;
        f.write_all(target_bytes.as_slice())?;
    }
    Ok(target_bytes.len() as u64)
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    let (groups, shared_metadata) = {
        let file = fs_err::File::open(input)?;
        let mut archive = ZipArchive::new(file)?;
        let metadata = archive.metadata();
        (plan_destinations(&mut archive, opts)?, metadata)
    };

    let password = opts.password.clone();
    groups.into_par_iter().try_for_each_init(
        || -> Option<ZipArchive<fs_err::File>> {
            let file = fs_err::File::open(input).ok()?;
            // SAFETY: metadata was parsed from the same file.
            Some(unsafe { ZipArchive::unsafe_new_with_metadata(file, shared_metadata.clone()) })
        },
        |maybe_archive, (dest_path, indices)| -> Result<()> {
            let archive = maybe_archive
                .as_mut()
                .ok_or_else(|| Error::Io(io::Error::other("failed to open zip archive")))?;
            for index in indices {
                extract_entry(
                    archive,
                    index,
                    &dest_path,
                    output,
                    opts,
                    password.as_deref(),
                )?;
            }
            Ok(())
        },
    )?;
    Ok(())
}

/// Resolve every entry's destination up front and bucket the archive indices by
/// it, keeping each bucket in ascending index order.
///
/// Flattening (`--no-directory`), `--strip-components` and rename rules all
/// routinely collapse several entries onto one destination.  Handing a whole
/// bucket to a single worker is what keeps a destination owned by exactly one
/// thread, so the overwrite decision and the write that follows it stay
/// indivisible and the surviving file is the archive's last entry for that
/// path, exactly as in a serial run.
fn plan_destinations(
    archive: &mut ZipArchive<fs_err::File>,
    opts: &DecompressOpts<'_>,
) -> Result<Vec<(Utf8PathBuf, Vec<usize>)>> {
    let mut groups: BTreeMap<Utf8PathBuf, Vec<usize>> = BTreeMap::new();
    for index in 0..archive.len() {
        // Raw access: names and directory-ness come from the central directory,
        // so planning costs no decompression and needs no password.
        let entry = archive.by_index_raw(index)?;
        let name = Utf8PathBuf::from(entry.name());
        let is_dir = entry.is_dir();
        drop(entry);

        if let Some(dest) = resolve_destination(&name, is_dir, opts)? {
            groups.entry(dest).or_default().push(index);
        }
    }
    Ok(groups.into_iter().collect())
}

/// Run one entry name through the filter and rewrite chain, yielding its
/// destination relative to the output root, or `None` when the entry is
/// filtered out.
fn resolve_destination(
    name: &Utf8Path,
    is_dir: bool,
    opts: &DecompressOpts<'_>,
) -> Result<Option<Utf8PathBuf>> {
    // Reject entries that attempt path traversal.
    filter::safe_entry_path(name.as_str())?;

    if !filter::should_extract(name.as_str(), &opts.includes, &opts.excludes) {
        return Ok(None);
    }

    if opts.no_directory && is_dir {
        return Ok(None);
    }

    let stripped = match filter::strip_components(name, opts.strip_components) {
        Some(p) => p,
        None => return Ok(None),
    };

    let dest_path = if opts.no_directory {
        match stripped.file_name() {
            Some(name) => Utf8PathBuf::from(name),
            None => return Ok(None),
        }
    } else {
        stripped
    };

    // Apply rename rules and optional prefix.
    match filter::apply_path_rewrites(dest_path, &opts.renames, opts.prefix.as_deref())? {
        p if p.as_str().is_empty() => Ok(None),
        p => Ok(canonicalize_dest(&p)),
    }
}

/// Rebuild a destination path from its `Normal` components only, dropping any
/// `CurDir` (`.`) components in the process.
///
/// `plan_destinations` uses the return value both as the write path and as
/// the `BTreeMap` grouping key, so this must be the canonical form: without
/// it, `./f.bin` and `f.bin` compare as different keys — even though
/// `output.join()` sends both to the same file — and the two groups can be
/// handed to different rayon workers, which then race to write the same
/// path.  `safe_entry_path` has already rejected `..` and absolute paths, so
/// only `CurDir` and `Normal` components can remain here. Rebuilding through
/// `push` (rather than keeping the original string) also drops any trailing
/// slash a directory entry's name carried, so `d/` and `d` collapse to the
/// identical path string rather than one of them still ending in `/` and
/// tripping `File::create` with "Is a directory".
fn canonicalize_dest(path: &Utf8Path) -> Option<Utf8PathBuf> {
    let mut out = Utf8PathBuf::new();
    for component in path.components() {
        if let camino::Utf8Component::Normal(part) = component {
            out.push(part);
        }
    }
    if out.as_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Extract a single entry to the destination [`plan_destinations`] resolved for
/// it, applying the overwrite policy.
fn extract_entry(
    archive: &mut ZipArchive<fs_err::File>,
    index: usize,
    dest_path: &Utf8Path,
    output: &Utf8Path,
    opts: &DecompressOpts<'_>,
    password: Option<&str>,
) -> Result<()> {
    let mut entry = open_zip_entry(archive, index, password)?;
    let out_path = output.join(dest_path);

    if entry.is_dir() {
        fs_err::create_dir_all(&out_path)?;
        return Ok(());
    }

    if let Some(parent) = out_path.parent() {
        fs_err::create_dir_all(parent)?;
    }
    let existed = fs_err::symlink_metadata(&out_path).is_ok();
    if existed {
        if let Some(ref suffix) = opts.backup_suffix {
            let backup = Utf8PathBuf::from(format!("{out_path}{suffix}"));
            fs_err::rename(&out_path, &backup)?;
        } else if opts.keep_newer {
            let entry_mtime = entry
                .last_modified()
                .map(zip_datetime_to_epoch)
                .unwrap_or(0);
            if filter::is_existing_newer(&out_path, entry_mtime)? {
                return Ok(());
            }
        } else if opts.no_overwrite {
            return Ok(());
        } else if !opts.force {
            return Err(Error::FileExists(out_path));
        }
    }

    if entry.is_symlink() {
        let written = extract_symlink_entry(&mut entry, &out_path, existed, dest_path)?;
        opts.progress.set_entry(dest_path.as_str());
        opts.progress.inc(written);
    } else {
        let unix_mode = entry.unix_mode();
        // If overwriting an existing symlink, remove it first so the new file
        // replaces the link rather than the link's target.
        if existed
            && fs_err::symlink_metadata(&out_path)?
                .file_type()
                .is_symlink()
        {
            fs_err::remove_file(&out_path)?;
        }
        let mut out_file = fs_err::File::create(&out_path)?;
        let written = io::copy(&mut entry, &mut out_file)?;
        #[cfg(unix)]
        if opts.preserve_permissions
            && let Some(mode) = unix_mode
        {
            use std::os::unix::fs::PermissionsExt;
            fs_err::set_permissions(&out_path, std::fs::Permissions::from_mode(mode & 0o7777))?;
        }
        opts.progress.set_entry(dest_path.as_str());
        opts.progress.inc(written);
    }
    Ok(())
}

// ── Decompress to writer ─────────────────────────────────────────────────────

pub fn decompress_to_writer<W: std::io::Write>(
    input: &Utf8Path,
    writer: &mut W,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let mut archive = ZipArchive::new(file)?;

    for i in 0..archive.len() {
        let mut entry = open_zip_entry(&mut archive, i, opts.password.as_deref())?;
        let name = Utf8PathBuf::from(entry.name());

        // Reject entries that attempt path traversal.
        filter::safe_entry_path(name.as_str())?;

        if !filter::should_extract(name.as_str(), &opts.includes, &opts.excludes) {
            continue;
        }

        let stripped = match filter::strip_components(&name, opts.strip_components) {
            Some(p) => p,
            None => continue,
        };

        if entry.is_dir() {
            continue;
        }

        // Apply rename rules and optional prefix.
        let display_path =
            match filter::apply_path_rewrites(stripped, &opts.renames, opts.prefix.as_deref())? {
                p if p.as_str().is_empty() => continue,
                p => p,
            };

        opts.progress.set_entry(display_path.as_str());
        io::copy(&mut entry, writer)?;
    }
    Ok(())
}

// ── Test ──────────────────────────────────────────────────────────────────────

pub fn test(
    input: &Utf8Path,
    password: Option<&str>,
    progress: &dyn crate::progress::ProgressReport,
) -> Result<()> {
    let (len, shared_metadata) = {
        let file = fs_err::File::open(input)?;
        let archive = ZipArchive::new(file)?;
        (archive.len(), archive.metadata())
    };

    let password = password.map(str::to_owned);
    (0..len).into_par_iter().try_for_each_init(
        || -> Option<ZipArchive<fs_err::File>> {
            let file = fs_err::File::open(input).ok()?;
            // SAFETY: metadata was parsed from the same file.
            Some(unsafe { ZipArchive::unsafe_new_with_metadata(file, shared_metadata.clone()) })
        },
        |maybe_archive, i| -> Result<()> {
            let archive = maybe_archive
                .as_mut()
                .ok_or_else(|| Error::Io(io::Error::other("failed to open zip archive")))?;
            let mut entry = open_zip_entry(archive, i, password.as_deref())?;
            let name = entry.name().to_owned();
            progress.set_entry(&name);
            let written = io::copy(&mut entry, &mut io::sink())?;
            progress.inc(written);
            Ok(())
        },
    )?;
    Ok(())
}

// ── List ──────────────────────────────────────────────────────────────────────

pub fn list(input: &Utf8Path) -> Result<Vec<Entry>> {
    let file = fs_err::File::open(input)?;
    let mut archive = ZipArchive::new(file)?;
    let mut entries = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i)?;
        entries.push(Entry {
            path: Utf8PathBuf::from(entry.name()),
            size: entry.size(),
            mtime: entry
                .last_modified()
                .map(zip_datetime_to_epoch)
                .unwrap_or(0),
            mode: entry.unix_mode().unwrap_or(0),
            is_dir: entry.is_dir(),
        });
    }
    Ok(entries)
}

// ── Info ──────────────────────────────────────────────────────────────────────

pub fn info(input: &Utf8Path) -> Result<ArchiveInfo> {
    let compressed_size = fs_err::metadata(input)?.len();

    let file = fs_err::File::open(input)?;
    let mut archive = ZipArchive::new(file)?;
    let entry_count = archive.len();

    // Fast path: decompressed_size() reads from the already-parsed central
    // directory with zero per-entry I/O.  Falls back to by_index_raw() only
    // when the archive uses data descriptors (uncommon).
    let total_uncompressed = match archive.decompressed_size() {
        Some(size) => u64::try_from(size).unwrap_or(u64::MAX),
        None => {
            // Saturating add — a corrupt or adversarial archive could claim
            // per-entry sizes that sum past u64::MAX; we report u64::MAX in
            // that case rather than panicking (debug) or wrapping (release).
            let mut total: u64 = 0;
            for i in 0..entry_count {
                let entry = archive.by_index_raw(i)?;
                total = total.saturating_add(entry.size());
            }
            total
        }
    };

    Ok(ArchiveInfo {
        format: "zip",
        entry_count,
        total_uncompressed,
        compressed_size,
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Open a zip entry by index, decrypting with `password` when provided.
///
/// If no password is supplied but the entry IS encrypted, returns
/// `Error::PasswordRequired` rather than a cryptic `UnsupportedArchive` error.
fn open_zip_entry<'a>(
    archive: &'a mut ZipArchive<fs_err::File>,
    index: usize,
    password: Option<&str>,
) -> Result<zip::read::ZipFile<'a, fs_err::File>> {
    if let Some(pwd) = password {
        Ok(archive.by_index_decrypt(index, pwd.as_bytes())?)
    } else {
        // Peek at the raw entry to check if it's encrypted before attempting
        // to open it without a password.
        let encrypted = archive.by_index_raw(index)?.encrypted();
        if encrypted {
            return Err(Error::PasswordRequired);
        }
        Ok(archive.by_index(index)?)
    }
}

/// Convert a zip `DateTime` to a unix epoch (seconds since 1970-01-01).
/// Returns 0 for any invalid or pre-epoch date.
fn zip_datetime_to_epoch(dt: zip::DateTime) -> u64 {
    let Some(month) = time::Month::try_from(dt.month()).ok() else {
        return 0;
    };
    let Some(date) = time::Date::from_calendar_date(dt.year() as i32, month, dt.day()).ok() else {
        return 0;
    };
    let Some(time) = time::Time::from_hms(dt.hour(), dt.minute(), dt.second()).ok() else {
        return 0;
    };

    let stamp = time::PrimitiveDateTime::new(date, time)
        .assume_utc()
        .unix_timestamp();
    if stamp >= 0 { stamp as u64 } else { 0 }
}
