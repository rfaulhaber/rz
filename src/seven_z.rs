use camino::{Utf8Path, Utf8PathBuf};
use sevenz_rust2::encoder_options::{AesEncoderOptions, Lzma2Options};
use sevenz_rust2::{EncoderConfiguration, EncoderMethod, Password};

use crate::error::{Error, Result};
use crate::{ArchiveInfo, CompressOpts, DecompressOpts, Entry};

/// The 7z backend can use a fast, single-call extraction path when no
/// per-entry filtering or overwrite logic is needed.  This predicate is
/// specific to 7z because every other backend streams entries through a
/// filter loop unconditionally.
fn can_fast_path(opts: &DecompressOpts<'_>) -> bool {
    // `keep_newer` and `strip_components` are rejected up-front in decompress,
    // so we don't need to check them here.
    // Rename/prefix rules require per-entry path rewriting, so the fast path
    // (single-call extraction) cannot be used when they are set.
    opts.force
        && opts.includes.is_empty()
        && opts.excludes.is_empty()
        && !opts.no_overwrite
        && !opts.no_directory
        && opts.backup_suffix.is_none()
        && !opts.preserve_permissions
        && opts.renames.is_empty()
        && opts.prefix.is_none()
}

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
fn push_dir_vcs(
    writer: &mut sevenz_rust2::ArchiveWriter<std::fs::File>,
    dir: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    for result in crate::filter::vcs_walker(dir, opts.follow_symlinks) {
        let entry = result.map_err(|e| std::io::Error::other(e.to_string()))?;
        let fs_path = entry.path();
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());

        if is_dir {
            continue;
        }

        let utf8_str = fs_path
            .to_str()
            .ok_or_else(|| Error::InvalidUtf8Path(fs_path.display().to_string()))?;
        let utf8_path = Utf8Path::new(utf8_str);

        let excludes = &opts.excludes;
        writer.push_source_path(utf8_path, |name| !excludes.is_match(name))?;
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
    // Use the fast path only when force is set and no filtering/special
    // options are active.  Otherwise we need the callback to enforce
    // overwrite guards, include/exclude, and backup logic.
    if can_fast_path(opts) {
        if let Some(pwd) = &opts.password {
            let file = fs_err::File::open(input)?;
            sevenz_rust2::decompress_with_password(file, output, Password::from(pwd.as_str()))?;
        } else {
            sevenz_rust2::decompress_file(input, output)?;
        }
    } else {
        let file = fs_err::File::open(input)?;
        let password = opts.password.as_deref().map_or_else(Password::empty, Password::from);
        sevenz_rust2::decompress_with_extract_fn_and_password(file, output, password, |entry, reader, dest| {
            // Reject entries that attempt path traversal.
            crate::filter::safe_entry_path(&entry.name).map_err(|e| {
                sevenz_rust2::Error::Io(
                    std::io::Error::other(e.to_string()),
                    entry.name.clone().into(),
                )
            })?;

            if !crate::filter::should_extract(&entry.name, &opts.includes, &opts.excludes) {
                return Ok(true);
            }
            if opts.no_directory && entry.is_directory {
                return Ok(true);
            }
            let base_name = if opts.no_directory {
                Utf8Path::new(&entry.name)
                    .file_name()
                    .map(camino::Utf8PathBuf::from)
                    .unwrap_or_else(|| camino::Utf8PathBuf::from(&entry.name))
            } else {
                camino::Utf8PathBuf::from(&entry.name)
            };
            // Apply rename rules and optional prefix.
            let rewritten = crate::filter::apply_path_rewrites(
                base_name,
                &opts.renames,
                opts.prefix.as_deref(),
            )
            .map_err(|e| {
                sevenz_rust2::Error::Io(
                    std::io::Error::other(e.to_string()),
                    entry.name.clone().into(),
                )
            })?;
            if rewritten.as_str().is_empty() {
                return Ok(true);
            }
            let out_name = rewritten.into_string();
            let out_path = dest.join(&out_name);
            if !entry.is_directory && out_path.exists() {
                if let Some(ref suffix) = opts.backup_suffix {
                    let backup_name = format!("{}{suffix}", out_path.display());
                    fs_err::rename(&out_path, Utf8Path::new(&backup_name))
                        .map_err(|e| sevenz_rust2::Error::Io(e, backup_name.into()))?;
                } else if opts.no_overwrite {
                    return Ok(true);
                } else if !opts.force {
                    let utf8 = Utf8PathBuf::from(out_path.display().to_string());
                    let err = Error::FileExists(utf8);
                    return Err(sevenz_rust2::Error::Io(
                        std::io::Error::new(std::io::ErrorKind::AlreadyExists, err.to_string()),
                        err.to_string().into(),
                    ));
                }
            }
            if opts.no_directory {
                // Extract to flat dest with the basename only.
                let mut out_file = fs_err::File::create(&out_path)
                    .map_err(|e| sevenz_rust2::Error::Io(e, out_name.into()))?;
                std::io::copy(reader, &mut out_file)
                    .map_err(|e| sevenz_rust2::Error::Io(e, "copy".into()))?;
                Ok(true)
            } else {
                sevenz_rust2::default_entry_extract_fn(entry, reader, dest)
            }
        })?;
    }
    Ok(())
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
    let password = opts.password.as_deref().map(Password::from).unwrap_or_else(Password::empty);
    sevenz_rust2::decompress_with_extract_fn_and_password(file, ".", password, |entry, reader, _dest| {
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
    })?;
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
    sevenz_rust2::decompress_with_extract_fn_and_password(file, ".", pwd, |entry, reader, _dest| {
        progress.set_entry(&entry.name);
        let written = std::io::copy(reader, &mut std::io::sink())
            .map_err(|e| sevenz_rust2::Error::Io(e, "test: reading entry".into()))?;
        progress.inc(written);
        Ok(true) // skip default extraction
    })?;
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
