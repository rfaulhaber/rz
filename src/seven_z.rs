use std::cell::RefCell;

use camino::{Utf8Path, Utf8PathBuf};
use sevenz_rust2::encoder_options::{AesEncoderOptions, Lzma2Options};
use sevenz_rust2::{ArchiveEntry, EncoderConfiguration, EncoderMethod, Password};

use crate::error::{Error, Result};
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

// ── Compress ──────────────────────────────────────────────────────────────────

pub fn compress(inputs: &[Utf8PathBuf], output: &Utf8Path, opts: &CompressOpts<'_>) -> Result<()> {
    let inputs = crate::filter::validate_inputs(inputs, opts)?;

    let mut writer = sevenz_rust2::ArchiveWriter::create(output)?;

    // Resolve the compression method from the requested level: 0 (or `--store`,
    // which main.rs maps to level 0) selects COPY (no compression); 1..=9 map to
    // LZMA2 presets (higher values are clamped to 9 by lzma-rust2); None keeps
    // sevenz-rust2's default LZMA2.
    let comp_cfg: Option<EncoderConfiguration> = match opts.level {
        Some(0) => Some(EncoderConfiguration::new(EncoderMethod::COPY)),
        Some(level) => Some(Lzma2Options::from_level(level).into()),
        None => None,
    };

    // The method vec mirrors the 7z encoder pipeline: each element wraps the
    // output of the previous one, so AES at index 0 is the OUTERMOST layer in
    // the archive (the first thing a reader must peel off), wrapping the
    // compression method beneath it.
    if let Some(pwd) = &opts.password {
        let aes_cfg: EncoderConfiguration =
            AesEncoderOptions::new(Password::from(pwd.as_str())).into();
        let comp_cfg = comp_cfg.unwrap_or_else(|| EncoderConfiguration::new(EncoderMethod::LZMA2));
        writer.set_content_methods(vec![aes_cfg, comp_cfg]);
    } else if let Some(comp_cfg) = comp_cfg {
        writer.set_content_methods(vec![comp_cfg]);
    }

    for input in &inputs {
        let meta = crate::filter::input_metadata(input, opts.follow_symlinks)?;
        if meta.is_dir() && opts.no_recursion {
            // Only add the directory entry, not its contents.
            continue;
        }
        if meta.is_dir() && opts.exclude_vcs_ignores {
            push_dir_vcs(&mut writer, input, opts)?;
        } else {
            let excludes = &opts.excludes;
            writer.push_source_path(input, |name| !excludes.is_match(name))?;
        }
    }
    let file = writer.finish()?;
    file.sync_all()?;
    Ok(())
}

/// Walk a directory with VCS-ignore awareness and add entries to the 7z writer.
///
/// Entries are pushed one file at a time via `push_archive_entry` with an
/// explicit archive-relative name. `push_source_path` derives the archive name
/// from the source path itself, which collapses to the bare file name when
/// that path is a single file — same-named files in different subdirectories
/// would then collide and overwrite one another in the archive.
fn push_dir_vcs(
    writer: &mut sevenz_rust2::ArchiveWriter<std::fs::File>,
    dir: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    let excludes = &opts.excludes;
    for result in crate::filter::vcs_walker(dir, opts.follow_symlinks) {
        let entry = result.map_err(|e| std::io::Error::other(e.to_string()))?;
        let fs_path = entry.path();
        let file_type = entry.file_type();

        // A non-following walk reports a symlink's own type, never its
        // target's. Skip it here to match the non-VCS branch below: it
        // walks via sevenz-rust2's `collect_file_paths`, which decides
        // whether to recurse from the raw (non-following) `DirEntry` file
        // type and so never surfaces a symlink at all, regardless of
        // `follow_symlinks`. Without this, a symlink-to-file was silently
        // dereferenced, a symlink-to-dir became an empty directory entry
        // with its contents dropped, and a dangling symlink hard-failed the
        // whole compress.
        if !opts.follow_symlinks && file_type.is_some_and(|ft| ft.is_symlink()) {
            continue;
        }

        let is_dir = file_type.is_some_and(|ft| ft.is_dir());
        if is_dir {
            continue;
        }

        let utf8_str = fs_path
            .to_str()
            .ok_or_else(|| Error::InvalidUtf8Path(fs_path.display().to_string()))?;
        let utf8_path = Utf8Path::new(utf8_str);

        // Excludes are matched against the same string the non-VCS branch's
        // `push_source_path` filter closure receives: the raw walked fs
        // path, not the archive-relative name computed below. A
        // slash-containing pattern is anchored (see `build_glob_set`), so
        // matching a different string here would make the two branches
        // disagree on which entries a pattern like `dir/sub/*` excludes.
        if excludes.is_match(utf8_str) {
            continue;
        }

        // Matches the naming the non-VCS path produces (`push_source_path`
        // called with the whole directory as its root): no leading directory
        // name, just the path relative to it.
        let archive_name = utf8_path
            .strip_prefix(dir)
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .to_string();

        let archive_entry = ArchiveEntry::from_path(utf8_path, archive_name);
        let file = fs_err::File::open(utf8_path)?;
        writer.push_archive_entry(archive_entry, Some(file))?;
    }
    Ok(())
}

// ── Decompress ────────────────────────────────────────────────────────────────

pub fn decompress(input: &Utf8Path, output: &Utf8Path, opts: &DecompressOpts<'_>) -> Result<()> {
    if opts.strip_components > 0 {
        return Err(Error::StripComponentsUnsupported("7z".to_owned()));
    }
    // sevenz-rust2 entries do not expose reliable mtime metadata, so we
    // can't implement --keep-newer with real newness semantics; refuse
    // rather than silently degrading to "skip existing".
    if opts.keep_newer {
        return Err(Error::KeepNewerUnsupported("7z".to_owned()));
    }
    let file = fs_err::File::open(input)?;
    let password = opts
        .password
        .as_deref()
        .map_or_else(Password::empty, Password::from);

    // sevenz-rust2's error type cannot carry one of ours, and forcing ours
    // through `io::Error` would relabel every failure as an I/O error, so the
    // real error is parked here and rethrown once the walk unwinds.
    let parked: RefCell<Option<Error>> = RefCell::new(None);

    let walked = sevenz_rust2::decompress_with_extract_fn_and_password(
        file,
        output,
        password,
        |entry, reader, _entry_dest| match extract_entry(entry, reader, output, opts) {
            Ok(keep_walking) => Ok(keep_walking),
            Err(e) => {
                let msg = e.to_string();
                *parked.borrow_mut() = Some(e);
                Err(sevenz_rust2::Error::Io(
                    std::io::Error::other(msg),
                    entry.name.clone().into(),
                ))
            }
        },
    );

    if let Some(e) = parked.into_inner() {
        return Err(e);
    }
    walked?;
    Ok(())
}

/// Resolve one 7z entry against the output root and write it out.
///
/// sevenz-rust2 hands the extract callback `<output>/<entry name>` — the
/// entry's own destination, derived from the raw archive name with no traversal
/// filtering — rather than the output root.  Every path is therefore resolved
/// against `output` here, and `default_entry_extract_fn` is never delegated to:
/// it would re-derive the destination from the unrewritten name, ignoring
/// `--rename`/`--prefix` and the overwrite guards below.
///
/// The returned bool tells sevenz-rust2 the entry is fully handled; a skipped
/// entry still counts as handled, so this is always `true`.
fn extract_entry(
    entry: &sevenz_rust2::ArchiveEntry,
    reader: &mut dyn std::io::Read,
    output: &Utf8Path,
    opts: &DecompressOpts<'_>,
) -> Result<bool> {
    crate::filter::safe_entry_path(&entry.name)?;

    if !crate::filter::should_extract(&entry.name, &opts.includes, &opts.excludes) {
        return skip_entry(reader);
    }
    if opts.no_directory && entry.is_directory {
        return skip_entry(reader);
    }

    let base_name = if opts.no_directory {
        match Utf8Path::new(&entry.name).file_name() {
            Some(name) => Utf8PathBuf::from(name),
            None => return skip_entry(reader),
        }
    } else {
        Utf8PathBuf::from(&entry.name)
    };

    let dest_path =
        match crate::filter::apply_path_rewrites(base_name, &opts.renames, opts.prefix.as_deref())?
        {
            p if p.as_str().is_empty() => return skip_entry(reader),
            p => p,
        };

    let out_path = output.join(&dest_path);

    if entry.is_directory {
        fs_err::create_dir_all(&out_path)?;
        return Ok(true);
    }

    if let Some(parent) = out_path.parent() {
        fs_err::create_dir_all(parent)?;
    }

    if fs_err::symlink_metadata(&out_path).is_ok() {
        if let Some(suffix) = &opts.backup_suffix {
            let backup = Utf8PathBuf::from(format!("{out_path}{suffix}"));
            fs_err::rename(&out_path, &backup)?;
        } else if opts.no_overwrite {
            return skip_entry(reader);
        } else if !opts.force {
            return Err(Error::FileExists(out_path));
        }
    }

    let mut out_file = fs_err::File::create(&out_path)?;
    let written = std::io::copy(reader, &mut out_file)?;
    restore_mtime(&out_file, entry);
    opts.progress.set_entry(dest_path.as_str());
    opts.progress.inc(written);
    Ok(true)
}

/// Consume an entry's payload without writing it anywhere.
///
/// Entry readers are bounded views over one shared solid-block stream, so an
/// entry that is filtered out still has to be read to its end: leaving bytes
/// behind makes every following entry in the block decode from a misaligned
/// offset and fail its CRC check.
fn skip_entry(reader: &mut dyn std::io::Read) -> Result<bool> {
    std::io::copy(reader, &mut std::io::sink())?;
    Ok(true)
}

/// Best-effort mtime restoration, matching what sevenz-rust2's default
/// extractor does.  A filesystem that refuses the timestamp must not fail an
/// otherwise-complete extraction.
fn restore_mtime(file: &fs_err::File, entry: &sevenz_rust2::ArchiveEntry) {
    if !entry.has_last_modified_date {
        return;
    }
    let times = std::fs::FileTimes::new().set_modified(entry.last_modified_date.into());
    let _ = file.file().set_times(times);
}

// ── Decompress to writer ─────────────────────────────────────────────────────

pub fn decompress_to_writer<W: std::io::Write>(
    input: &Utf8Path,
    writer: &mut W,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    if opts.strip_components > 0 {
        return Err(Error::StripComponentsUnsupported("7z".to_owned()));
    }
    let file = fs_err::File::open(input)?;
    let password = opts
        .password
        .as_deref()
        .map(Password::from)
        .unwrap_or_else(Password::empty);
    sevenz_rust2::decompress_with_extract_fn_and_password(
        file,
        ".",
        password,
        |entry, reader, _dest| {
            // Reject entries that attempt path traversal.
            crate::filter::safe_entry_path(&entry.name).map_err(|e| {
                sevenz_rust2::Error::Io(
                    std::io::Error::other(e.to_string()),
                    entry.name.clone().into(),
                )
            })?;

            if entry.is_directory {
                return Ok(true);
            }
            if !crate::filter::should_extract(&entry.name, &opts.includes, &opts.excludes) {
                return Ok(true);
            }
            if opts.no_directory {
                let display_name = Utf8Path::new(&entry.name)
                    .file_name()
                    .unwrap_or(&entry.name);
                opts.progress.set_entry(display_name);
            } else {
                opts.progress.set_entry(&entry.name);
            }
            std::io::copy(reader, writer)
                .map_err(|e| sevenz_rust2::Error::Io(e, "decompress to writer".into()))?;
            Ok(true) // skip default extraction
        },
    )?;
    Ok(())
}

// ── Test ──────────────────────────────────────────────────────────────────────

pub fn test(
    input: &Utf8Path,
    password: Option<&str>,
    progress: &dyn crate::progress::ProgressReport,
) -> Result<()> {
    let file = fs_err::File::open(input)?;
    let pwd = password.map_or_else(Password::empty, Password::from);
    sevenz_rust2::decompress_with_extract_fn_and_password(
        file,
        ".",
        pwd,
        |entry, reader, _dest| {
            progress.set_entry(&entry.name);
            let written = std::io::copy(reader, &mut std::io::sink())
                .map_err(|e| sevenz_rust2::Error::Io(e, "test: reading entry".into()))?;
            progress.inc(written);
            Ok(true) // skip default extraction
        },
    )?;
    Ok(())
}

// ── List ──────────────────────────────────────────────────────────────────────

pub fn list(input: &Utf8Path) -> Result<Vec<Entry>> {
    let archive = sevenz_rust2::Archive::open(input)?;
    let mut entries = Vec::new();
    for file in &archive.files {
        let path = Utf8PathBuf::from(&file.name);
        entries.push(Entry {
            path,
            size: file.size,
            mtime: 0,
            mode: 0,
            is_dir: file.is_directory,
        });
    }
    Ok(entries)
}

// ── Info ──────────────────────────────────────────────────────────────────────

pub fn info(input: &Utf8Path) -> Result<ArchiveInfo> {
    let compressed_size = fs_err::metadata(input)?.len();
    let archive = sevenz_rust2::Archive::open(input)?;

    Ok(ArchiveInfo {
        format: "7z",
        entry_count: archive.files.len(),
        // Saturating fold — `Iterator::sum` on u64 panics on overflow in
        // debug and wraps in release; either is a bad outcome for a
        // potentially adversarial archive.
        total_uncompressed: archive
            .files
            .iter()
            .fold(0u64, |acc, f| acc.saturating_add(f.size)),
        compressed_size,
    })
}
