use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use camino::{Utf8Path, Utf8PathBuf};
use clap::{CommandFactory, Parser};

use rz_archive::cmd::{Cli, Command, Format, PasswordArgs, SortField};
use rz_archive::error::{Error, Result};
use rz_archive::filter;
use rz_archive::format::{resolve_compress_format, resolve_input_format};
use rz_archive::modify::{self, AppendMode};
use rz_archive::progress::{BarProgress, NoProgress, ProgressReport, VerboseReport};
#[cfg(feature = "bzip2")]
use rz_archive::tar_bz2;
use rz_archive::{CompressOpts, DecompressOpts, seven_z, tar, tar_gz, tar_xz, tar_zst, zip};

/// Resolve a password from any of the three password-source flags.
///
/// Returns `Ok(None)` when no flag is set.  Returns an error when:
/// - `--password-stdin` is set and stdin is empty.
/// - `--password-file PATH` is set and the first line is empty or the file
///   cannot be read.
fn resolve_password(args: &PasswordArgs) -> Result<Option<String>> {
    if args.password_stdin {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).map_err(Error::Io)?;
        // Strip one trailing \r\n or \n.
        if buf.ends_with('\n') {
            buf.pop();
            if buf.ends_with('\r') {
                buf.pop();
            }
        }
        if buf.is_empty() {
            return Err(Error::EmptyPassword);
        }
        return Ok(Some(buf));
    }
    if let Some(ref path) = args.password_file {
        let content = fs_err::read_to_string(path)?;
        let line = content.lines().next().unwrap_or("").to_owned();
        if line.is_empty() {
            return Err(Error::EmptyPassword);
        }
        return Ok(Some(line));
    }
    if let Some(ref pw) = args.password {
        return Ok(Some(pw.clone()));
    }
    Ok(None)
}

/// Reject encryption flags for formats that don't support it (everything
/// except zip and 7z).
fn reject_encryption_for_non_supported(fmt: &Format, password: &Option<String>) -> Result<()> {
    if password.is_none() {
        return Ok(());
    }
    if matches!(fmt, Format::Zip | Format::SevenZ) {
        return Ok(());
    }
    Err(Error::EncryptionUnsupported(fmt.to_string()))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(n) = cli.threads
        && n > 0
    {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global();
    }
    if let Err(e) = run(cli) {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "rz: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Returns `true` when the path is the conventional stdin/stdout placeholder.
fn is_stdio(path: &str) -> bool {
    path == "-"
}

/// Returns `true` when the format requires seekable I/O (not streamable).
fn requires_seek(fmt: &Format) -> bool {
    matches!(fmt, Format::Zip | Format::SevenZ)
}

/// Reproducibility overrides (`--mtime`, `--owner`, `--group`, `--mode`)
/// require writing per-entry metadata that zip and 7z don't expose through
/// our underlying writers: zip has no UID/GID field in its central directory,
/// and `sevenz-rust2::ArchiveWriter` has no per-entry metadata hook.  Rather
/// than silently no-op the flags, reject up front with a clear pointer to the
/// tar-family formats that do support reproducibility.
fn reject_reproducibility_for_non_tar(
    fmt: &Format,
    mtime: Option<u64>,
    owner: Option<u64>,
    group: Option<u64>,
    mode: Option<u32>,
    newer_than: Option<i64>,
    older_than: Option<i64>,
) -> Result<()> {
    let is_tar_family = matches!(
        fmt,
        Format::Tar | Format::TarGz | Format::TarZst | Format::TarXz | Format::TarBz2
    );
    if is_tar_family {
        return Ok(());
    }
    let check = |flag: &'static str, present: bool| -> Result<()> {
        if present {
            return Err(Error::ReproducibilityFlagUnsupported {
                flag,
                format: fmt.to_string(),
            });
        }
        Ok(())
    };
    check("--mtime", mtime.is_some())?;
    check("--owner", owner.is_some())?;
    check("--group", group.is_some())?;
    check("--mode", mode.is_some())?;
    check("--newer-than", newer_than.is_some())?;
    check("--older-than", older_than.is_some())?;
    Ok(())
}

/// Format a byte count for display.  When `human` is true, uses IEC-style
/// units (KiB, MiB, …); otherwise returns the raw number followed by "bytes".
fn format_size(bytes: u64, human: bool) -> String {
    if !human {
        return format!("{bytes} bytes");
    }
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    for &unit in UNITS {
        if value < 1024.0 {
            return if unit == "B" {
                format!("{bytes} B")
            } else {
                format!("{value:.1} {unit}")
            };
        }
        value /= 1024.0;
    }
    format!("{value:.1} PiB")
}

/// Maximum bytes peeked from stdin for magic-byte format detection.
///
/// Plain tar's `ustar` magic sits at offset 257, so we need at least 262 bytes
/// to recognise it; 512 (one tar block) is a safe round figure that also covers
/// the offset-0 magics (gzip, zstd, xz, bzip2, zip, 7z).
const STDIN_MAGIC_PREFIX: usize = 512;

/// The reader returned for a stdin archive: the peeked prefix chained ahead of
/// the unread remainder of the stream, so the format detector and the decoder
/// both see the whole archive.
type StdinReader = std::io::Chain<std::io::Cursor<Vec<u8>>, std::io::StdinLock<'static>>;

/// Resolve a stdin archive into its format and a replayable reader.
///
/// Peeks a prefix of stdin, determines the format (explicit `--format` wins,
/// else magic-byte auto-detection), then chains the prefix back onto the rest
/// of the stream. zip and 7z need seekable input and are rejected here, as is
/// a terminal/empty stdin (nothing was piped in).
fn resolve_stdin_source(format: Option<Format>) -> Result<(Format, StdinReader)> {
    // A terminal stdin means nothing was piped in; reading would block forever.
    if std::io::stdin().is_terminal() {
        return Err(Error::NoInput);
    }

    let mut stdin = std::io::stdin().lock();
    let prefix = filter::read_prefix(&mut stdin, STDIN_MAGIC_PREFIX)?;
    if prefix.is_empty() {
        return Err(Error::NoInput);
    }

    let fmt = match format {
        Some(f) => f,
        None => Format::from_magic_bytes(&prefix).ok_or(Error::CannotInferFormatStdin)?,
    };

    // zip and 7z need seekable input to read their central directory / header.
    if requires_seek(&fmt) {
        return Err(Error::StdinNotSupported(fmt.to_string()));
    }

    // Re-attach the peeked prefix ahead of the unread remainder of stdin.
    let reader = std::io::Cursor::new(prefix).chain(stdin);
    Ok((fmt, reader))
}

/// Read archive metadata from stdin.
fn info_from_stdin(
    format: Option<Format>,
    password: &Option<String>,
) -> Result<rz_archive::ArchiveInfo> {
    let (fmt, reader) = resolve_stdin_source(format)?;
    reject_encryption_for_non_supported(&fmt, password)?;
    let info = match fmt {
        Format::Tar => tar::info_from_reader(reader)?,
        Format::TarGz => tar_gz::info_from_reader(reader)?,
        Format::TarZst => tar_zst::info_from_reader(std::io::BufReader::new(reader))?,
        Format::TarXz => tar_xz::info_from_reader(reader)?,
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => tar_bz2::info_from_reader(reader)?,
        _ => return Err(Error::StdinNotSupported(fmt.to_string())),
    };
    Ok(info)
}

/// List archive entries from stdin.
fn list_from_stdin(
    format: Option<Format>,
    password: &Option<String>,
) -> Result<Vec<rz_archive::Entry>> {
    let (fmt, reader) = resolve_stdin_source(format)?;
    reject_encryption_for_non_supported(&fmt, password)?;
    let entries = match fmt {
        Format::Tar => tar::list_from_reader(reader)?,
        Format::TarGz => tar_gz::list_from_reader(reader)?,
        Format::TarZst => tar_zst::list_from_reader(std::io::BufReader::new(reader))?,
        Format::TarXz => tar_xz::list_from_reader(reader)?,
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => tar_bz2::list_from_reader(reader)?,
        _ => return Err(Error::StdinNotSupported(fmt.to_string())),
    };
    Ok(entries)
}

/// Verify archive integrity from stdin.
fn test_from_stdin(
    format: Option<Format>,
    password: &Option<String>,
    progress: &dyn ProgressReport,
) -> Result<()> {
    let (fmt, reader) = resolve_stdin_source(format)?;
    reject_encryption_for_non_supported(&fmt, password)?;
    match fmt {
        Format::Tar => tar::test_from_reader(reader, progress)?,
        Format::TarGz => tar_gz::test_from_reader(reader, progress)?,
        Format::TarZst => tar_zst::test_from_reader(std::io::BufReader::new(reader), progress)?,
        Format::TarXz => tar_xz::test_from_reader(reader, progress)?,
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => tar_bz2::test_from_reader(reader, progress)?,
        _ => return Err(Error::StdinNotSupported(fmt.to_string())),
    }
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Compress {
            mut input,
            output,
            format,
            level,
            store,
            exclude,
            exclude_from,
            files_from,
            exclude_vcs,
            exclude_backups,
            follow_symlinks,
            exclude_vcs_ignores,
            no_recursion,
            totals,
            dry_run,
            mtime,
            owner,
            group,
            mode,
            newer_than,
            older_than,
            ignore_failed_read,
            password_args,
        } => {
            let password = resolve_password(&password_args)?;
            let level = if store { Some(0) } else { level };

            // Merge --files-from paths into input list.
            if let Some(ref list_file) = files_from {
                let extra = filter::read_paths_from_file(list_file)?;
                input.extend(extra);
            }

            // `input` can only be empty here when it came entirely from
            // --files-from (clap requires it otherwise) and the list held no
            // usable lines.  Bail before the dry-run branch silently prints
            // nothing and before `fmt.default_output(&input[0])` indexes an
            // empty vec.
            if input.is_empty() {
                return Err(Error::NoReadableInputs);
            }

            // Build combined exclude set.
            let mut extra_patterns = exclude;
            if exclude_vcs {
                for pat in &[".git", ".hg", ".svn", ".bzr", "_darcs", ".pijul", "CVS"] {
                    extra_patterns.push((*pat).to_owned());
                }
            }
            if exclude_backups {
                for pat in &["*~", "*.bak", "#*#", ".#*"] {
                    extra_patterns.push((*pat).to_owned());
                }
            }
            let excludes = filter::build_excludes(extra_patterns, &exclude_from)?;

            // Dry-run: list what would be compressed and exit.
            if dry_run {
                let dry_opts = CompressOpts {
                    level,
                    excludes,
                    follow_symlinks,
                    exclude_vcs_ignores,
                    no_recursion,
                    progress: &NoProgress,
                    fixed_mtime: mtime,
                    fixed_uid: owner,
                    fixed_gid: group,
                    fixed_mode: mode,
                    newer_than,
                    older_than,
                    ignore_failed_read,
                    password: None,
                };
                let paths = filter::collect_compress_paths(&input, &dry_opts)?;
                let mut stdout = std::io::stdout().lock();
                for p in &paths {
                    let _ = writeln!(stdout, "{p}");
                }
                return Ok(());
            }

            let to_stdout = output.as_ref().is_some_and(|o| is_stdio(o.as_str()));

            let fmt = if to_stdout {
                format.ok_or(Error::CannotInferOutput)?
            } else {
                resolve_compress_format(format, output.as_deref())?
            };

            if to_stdout && requires_seek(&fmt) {
                return Err(Error::StdoutNotSupported(fmt.to_string()));
            }

            // Reproducibility flags are implemented only for tar-family formats;
            // zip and 7z either lack fields for the metadata (zip has no UID/GID)
            // or the writer doesn't expose per-entry overrides (sevenz-rust2).
            // Reject rather than silently no-op so users don't get misleading
            // results when chasing bit-for-bit reproducibility.
            reject_reproducibility_for_non_tar(
                &fmt, mtime, owner, group, mode, newer_than, older_than,
            )?;

            // Encryption is only supported for zip and 7z; reject early for
            // tar-family so the user gets a clear message before any I/O.
            reject_encryption_for_non_supported(&fmt, &password)?;

            let base_progress: Box<dyn ProgressReport> = if cli.progress && !to_stdout {
                Box::new(BarProgress::spinner())
            } else if totals {
                Box::new(BarProgress::hidden())
            } else {
                Box::new(NoProgress)
            };
            let verbose_progress;
            let progress: &dyn ProgressReport = if cli.verbose {
                verbose_progress = VerboseReport::new(&*base_progress);
                &verbose_progress
            } else {
                &*base_progress
            };
            let opts = CompressOpts {
                level,
                excludes,
                follow_symlinks,
                exclude_vcs_ignores,
                no_recursion,
                progress,
                fixed_mtime: mtime,
                fixed_uid: owner,
                fixed_gid: group,
                fixed_mode: mode,
                newer_than,
                older_than,
                ignore_failed_read,
                password,
            };

            if to_stdout {
                let stdout = std::io::stdout().lock();
                match fmt {
                    Format::Tar => tar::compress_to_writer(&input, stdout, &opts)?,
                    Format::TarGz => tar_gz::compress_to_writer(&input, stdout, &opts)?,
                    Format::TarZst => tar_zst::compress_to_writer(&input, stdout, &opts)?,
                    Format::TarXz => tar_xz::compress_to_writer(&input, stdout, &opts)?,
                    #[cfg(feature = "bzip2")]
                    Format::TarBz2 => tar_bz2::compress_to_writer(&input, stdout, &opts)?,
                    _ => return Err(Error::StdoutNotSupported(fmt.to_string())),
                }
                // The lock above was moved into the writer; re-acquire it to force
                // out anything still sitting in Stdout's own buffer and surface a
                // late write failure (e.g. a full disk) instead of exiting 0.
                std::io::stdout().lock().flush()?;
            } else {
                let output = match output {
                    Some(o) => o,
                    None => fmt.default_output(&input[0]),
                };
                match fmt {
                    Format::Zip => zip::compress(&input, &output, &opts)?,
                    Format::Tar => tar::compress(&input, &output, &opts)?,
                    Format::TarGz => tar_gz::compress(&input, &output, &opts)?,
                    Format::TarZst => tar_zst::compress(&input, &output, &opts)?,
                    Format::TarXz => tar_xz::compress(&input, &output, &opts)?,
                    #[cfg(feature = "bzip2")]
                    Format::TarBz2 => tar_bz2::compress(&input, &output, &opts)?,
                    Format::SevenZ => seven_z::compress(&input, &output, &opts)?,
                    #[allow(unreachable_patterns)]
                    other => return Err(Error::UnsupportedFormat(other.to_string())),
                }
            }
            progress.finish();
            if totals {
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(
                    stderr,
                    "Total bytes: {}",
                    format_size(progress.position(), false)
                );
            }
        }

        Command::Decompress {
            input,
            output,
            format,
            force,
            no_overwrite,
            keep_newer,
            no_directory,
            to_stdout,
            strip_components,
            exclude,
            exclude_from,
            include,
            backup,
            suffix,
            preserve_permissions,
            same_owner,
            newer_than,
            older_than,
            totals,
            dry_run,
            rename,
            prefix,
            paths,
            one_top_level,
            password_args,
        } => {
            let password = resolve_password(&password_args)?;
            let from_stdin = input.as_ref().is_none_or(|p| is_stdio(p.as_str()));

            // For stdin, peek + detect the format now; the returned reader
            // carries the rest of the stream through to extraction. The
            // requires_seek (zip/7z) and terminal/empty-stdin rejections happen
            // inside resolve_stdin_source.
            let (fmt, mut stdin_reader) = if from_stdin {
                let (fmt, reader) = resolve_stdin_source(format)?;
                (fmt, Some(reader))
            } else {
                let fmt = match input.as_deref() {
                    Some(p) => resolve_input_format(format, p)?,
                    None => return Err(Error::NoInput),
                };
                (fmt, None)
            };
            // From here on treat input as a concrete path; it is empty and
            // unused on the stdin path (every use is gated by `from_stdin`).
            let input = input.unwrap_or_default();

            // `--one-top-level` derives a sub-directory from the archive
            // filename (`foo.tar.gz` → `foo/`).  Stdin has no filename, so
            // we can't derive anything — bail with a clear error rather
            // than silently treating "-" as the stem.  Tar-family extraction
            // expects its output directory to already exist, so we create
            // it ourselves here.
            let output = if one_top_level {
                if from_stdin {
                    return Err(Error::OneTopLevelStdin);
                }
                let derived = fmt.derive_output_dir(&input);
                fs_err::create_dir_all(&derived)?;
                Some(derived)
            } else {
                output
            };

            let excludes = filter::build_excludes(exclude, &exclude_from)?;
            let includes = {
                let mut all_includes = include;
                all_includes.extend(paths);
                filter::build_glob_set(&all_includes)?
            };

            // Dry-run: list what would be extracted and exit.
            if dry_run {
                let entries = if from_stdin {
                    // Consume the peeked reader to list; dry-run never extracts,
                    // so spending the stream here is fine.
                    let reader = stdin_reader.take().ok_or(Error::NoInput)?;
                    match fmt {
                        Format::Tar => tar::list_from_reader(reader)?,
                        Format::TarGz => tar_gz::list_from_reader(reader)?,
                        Format::TarZst => {
                            tar_zst::list_from_reader(std::io::BufReader::new(reader))?
                        }
                        Format::TarXz => tar_xz::list_from_reader(reader)?,
                        #[cfg(feature = "bzip2")]
                        Format::TarBz2 => tar_bz2::list_from_reader(reader)?,
                        _ => return Err(Error::StdinNotSupported(fmt.to_string())),
                    }
                } else {
                    match fmt {
                        Format::Zip => zip::list(&input)?,
                        Format::Tar => tar::list(&input)?,
                        Format::TarGz => tar_gz::list(&input)?,
                        Format::TarZst => tar_zst::list(&input)?,
                        Format::TarXz => tar_xz::list(&input)?,
                        #[cfg(feature = "bzip2")]
                        Format::TarBz2 => tar_bz2::list(&input)?,
                        Format::SevenZ => seven_z::list(&input)?,
                        #[allow(unreachable_patterns)]
                        other => return Err(Error::UnsupportedFormat(other.to_string())),
                    }
                };
                let mut stdout = std::io::stdout().lock();
                for entry in &entries {
                    if !filter::should_extract(entry.path.as_str(), &includes, &excludes) {
                        continue;
                    }
                    if let Some(stripped) = filter::strip_components(&entry.path, strip_components)
                    {
                        let _ = writeln!(stdout, "{stripped}");
                    }
                }
                return Ok(());
            }

            let base_progress: Box<dyn ProgressReport> = if cli.progress && !from_stdin {
                let file_size = fs_err::metadata(&input)?.len();
                Box::new(BarProgress::bytes(file_size))
            } else if cli.progress {
                Box::new(BarProgress::spinner())
            } else if totals {
                Box::new(BarProgress::hidden())
            } else {
                Box::new(NoProgress)
            };
            let verbose_progress;
            let progress: &dyn ProgressReport = if cli.verbose {
                verbose_progress = VerboseReport::new(&*base_progress);
                &verbose_progress
            } else {
                &*base_progress
            };
            let backup_suffix = if let Some(s) = suffix {
                Some(s)
            } else if backup {
                Some(".bak".to_owned())
            } else {
                None
            };
            // --same-owner only applies to tar-family extraction (zip/7z
            // don't carry portable uid/gid).  Reject up front so users don't
            // assume ownership is being restored silently.
            let is_tar_family = matches!(
                fmt,
                Format::Tar | Format::TarGz | Format::TarZst | Format::TarXz | Format::TarBz2
            );
            if same_owner && !is_tar_family {
                return Err(Error::ReproducibilityFlagUnsupported {
                    flag: "--same-owner",
                    format: fmt.to_string(),
                });
            }
            // Time-based filters read the entry mtime from the tar header;
            // zip and 7z entries don't expose reliable mtime through the
            // current crates (sevenz-rust2 in particular).  Reject rather
            // than silently returning no matches.
            if !is_tar_family && (newer_than.is_some() || older_than.is_some()) {
                let flag = if newer_than.is_some() {
                    "--newer-than"
                } else {
                    "--older-than"
                };
                return Err(Error::ReproducibilityFlagUnsupported {
                    flag,
                    format: fmt.to_string(),
                });
            }

            reject_encryption_for_non_supported(&fmt, &password)?;

            let opts = DecompressOpts {
                force,
                no_overwrite,
                keep_newer,
                no_directory,
                strip_components,
                includes,
                excludes,
                backup_suffix,
                preserve_permissions,
                same_owner,
                newer_than,
                older_than,
                renames: rename,
                prefix,
                progress,
                password,
            };

            if to_stdout {
                let mut stdout = std::io::stdout().lock();
                if from_stdin {
                    let reader = stdin_reader.take().ok_or(Error::NoInput)?;
                    match fmt {
                        Format::Tar => {
                            tar::decompress_reader_to_writer(reader, &mut stdout, &opts)?
                        }
                        Format::TarGz => {
                            tar_gz::decompress_reader_to_writer(reader, &mut stdout, &opts)?
                        }
                        Format::TarZst => tar_zst::decompress_reader_to_writer(
                            std::io::BufReader::new(reader),
                            &mut stdout,
                            &opts,
                        )?,
                        Format::TarXz => {
                            tar_xz::decompress_reader_to_writer(reader, &mut stdout, &opts)?
                        }
                        #[cfg(feature = "bzip2")]
                        Format::TarBz2 => {
                            tar_bz2::decompress_reader_to_writer(reader, &mut stdout, &opts)?
                        }
                        _ => return Err(Error::StdinNotSupported(fmt.to_string())),
                    }
                } else {
                    match fmt {
                        Format::Zip => zip::decompress_to_writer(&input, &mut stdout, &opts)?,
                        Format::Tar => tar::decompress_to_writer(&input, &mut stdout, &opts)?,
                        Format::TarGz => tar_gz::decompress_to_writer(&input, &mut stdout, &opts)?,
                        Format::TarZst => {
                            tar_zst::decompress_to_writer(&input, &mut stdout, &opts)?
                        }
                        Format::TarXz => tar_xz::decompress_to_writer(&input, &mut stdout, &opts)?,
                        #[cfg(feature = "bzip2")]
                        Format::TarBz2 => {
                            tar_bz2::decompress_to_writer(&input, &mut stdout, &opts)?
                        }
                        Format::SevenZ => {
                            seven_z::decompress_to_writer(&input, &mut stdout, &opts)?
                        }
                        #[allow(unreachable_patterns)]
                        other => return Err(Error::UnsupportedFormat(other.to_string())),
                    }
                }
            } else if from_stdin {
                let output = output.unwrap_or_else(|| ".".into());
                let reader = stdin_reader.take().ok_or(Error::NoInput)?;
                match fmt {
                    Format::Tar => tar::decompress_from_reader(reader, &output, &opts)?,
                    Format::TarGz => tar_gz::decompress_from_reader(reader, &output, &opts)?,
                    Format::TarZst => tar_zst::decompress_from_reader(
                        std::io::BufReader::new(reader),
                        &output,
                        &opts,
                    )?,
                    Format::TarXz => tar_xz::decompress_from_reader(reader, &output, &opts)?,
                    #[cfg(feature = "bzip2")]
                    Format::TarBz2 => tar_bz2::decompress_from_reader(reader, &output, &opts)?,
                    _ => return Err(Error::StdinNotSupported(fmt.to_string())),
                }
            } else {
                let output = output.unwrap_or_else(|| ".".into());
                match fmt {
                    Format::Zip => zip::decompress(&input, &output, &opts)?,
                    Format::Tar => tar::decompress(&input, &output, &opts)?,
                    Format::TarGz => tar_gz::decompress(&input, &output, &opts)?,
                    Format::TarZst => tar_zst::decompress(&input, &output, &opts)?,
                    Format::TarXz => tar_xz::decompress(&input, &output, &opts)?,
                    #[cfg(feature = "bzip2")]
                    Format::TarBz2 => tar_bz2::decompress(&input, &output, &opts)?,
                    Format::SevenZ => seven_z::decompress(&input, &output, &opts)?,
                    #[allow(unreachable_patterns)]
                    other => return Err(Error::UnsupportedFormat(other.to_string())),
                }
            }
            progress.finish();
            if totals {
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(
                    stderr,
                    "Total bytes: {}",
                    format_size(progress.position(), false)
                );
            }
        }

        Command::List {
            input,
            format,
            long,
            exclude,
            exclude_from,
            sort,
            human_readable,
            json,
            password_args,
        } => {
            let password = resolve_password(&password_args)?;
            let from_stdin = input.as_ref().is_none_or(|p| is_stdio(p.as_str()));

            let mut entries = if from_stdin {
                list_from_stdin(format, &password)?
            } else {
                let input = input.unwrap_or_default();
                let fmt = resolve_input_format(format, &input)?;
                reject_encryption_for_non_supported(&fmt, &password)?;
                match fmt {
                    Format::Zip => zip::list(&input)?,
                    Format::Tar => tar::list(&input)?,
                    Format::TarGz => tar_gz::list(&input)?,
                    Format::TarZst => tar_zst::list(&input)?,
                    Format::TarXz => tar_xz::list(&input)?,
                    #[cfg(feature = "bzip2")]
                    Format::TarBz2 => tar_bz2::list(&input)?,
                    Format::SevenZ => seven_z::list(&input)?,
                    #[allow(unreachable_patterns)]
                    other => return Err(Error::UnsupportedFormat(other.to_string())),
                }
            };

            let excludes = filter::build_excludes(exclude, &exclude_from)?;

            if let Some(ref field) = sort {
                match field {
                    SortField::Name => entries.sort_by(|a, b| a.path.cmp(&b.path)),
                    SortField::Size => entries.sort_by_key(|e| e.size),
                    SortField::Date => entries.sort_by_key(|e| e.mtime),
                }
            }

            let includes = globset::GlobSet::empty();
            let filtered: Vec<_> = entries
                .into_iter()
                .filter(|e| filter::should_extract(e.path.as_str(), &includes, &excludes))
                .collect();

            let mut stdout = std::io::stdout().lock();
            if json {
                let _ = serde_json::to_writer_pretty(&mut stdout, &filtered);
                let _ = writeln!(stdout);
            } else {
                for entry in &filtered {
                    if long {
                        let kind = if entry.is_dir { "d" } else { "-" };
                        let size_str = format_size(entry.size, human_readable);
                        let _ = writeln!(
                            stdout,
                            "{kind}{:06o}  {:>10}  {}",
                            entry.mode, size_str, entry.path,
                        );
                    } else {
                        let _ = writeln!(stdout, "{}", entry.path);
                    }
                }
            }
        }

        Command::Test {
            input,
            format,
            password_args,
        } => {
            let password = resolve_password(&password_args)?;
            let from_stdin = input.as_ref().is_none_or(|p| is_stdio(p.as_str()));

            // A file gives us a byte total for a real progress bar; stdin can't
            // be sized ahead of time, so it falls back to a spinner.
            let base_progress: Box<dyn ProgressReport> = match input.as_ref() {
                Some(p) if cli.progress && !from_stdin => {
                    Box::new(BarProgress::bytes(fs_err::metadata(p)?.len()))
                }
                _ if cli.progress => Box::new(BarProgress::spinner()),
                _ => Box::new(NoProgress),
            };
            let verbose_progress;
            let progress: &dyn ProgressReport = if cli.verbose {
                verbose_progress = VerboseReport::new(&*base_progress);
                &verbose_progress
            } else {
                &*base_progress
            };
            if from_stdin {
                test_from_stdin(format, &password, progress)?;
            } else {
                let input = input.unwrap_or_default();
                let fmt = resolve_input_format(format, &input)?;
                reject_encryption_for_non_supported(&fmt, &password)?;
                match fmt {
                    Format::Zip => zip::test(&input, password.as_deref(), progress)?,
                    Format::Tar => tar::test(&input, progress)?,
                    Format::TarGz => tar_gz::test(&input, progress)?,
                    Format::TarZst => tar_zst::test(&input, progress)?,
                    Format::TarXz => tar_xz::test(&input, progress)?,
                    #[cfg(feature = "bzip2")]
                    Format::TarBz2 => tar_bz2::test(&input, progress)?,
                    Format::SevenZ => seven_z::test(&input, password.as_deref(), progress)?,
                    #[allow(unreachable_patterns)]
                    other => return Err(Error::UnsupportedFormat(other.to_string())),
                }
            }
            progress.finish();
            if !cli.quiet {
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "ok");
            }
        }

        Command::Info {
            input,
            format,
            human_readable,
            json,
            password_args,
        } => {
            let password = resolve_password(&password_args)?;
            // Stdin when no path is given or it's the `-` sentinel.
            let from_stdin = input.as_ref().is_none_or(|p| is_stdio(p.as_str()));

            let info = if from_stdin {
                info_from_stdin(format, &password)?
            } else {
                // Safe: `from_stdin` is false only when `input` is `Some` and
                // not `-`.
                let input = input.unwrap_or_default();
                let fmt = resolve_input_format(format, &input)?;
                reject_encryption_for_non_supported(&fmt, &password)?;
                match fmt {
                    Format::Zip => zip::info(&input)?,
                    Format::Tar => tar::info(&input)?,
                    Format::TarGz => tar_gz::info(&input)?,
                    Format::TarZst => tar_zst::info(&input)?,
                    Format::TarXz => tar_xz::info(&input)?,
                    #[cfg(feature = "bzip2")]
                    Format::TarBz2 => tar_bz2::info(&input)?,
                    Format::SevenZ => seven_z::info(&input)?,
                    #[allow(unreachable_patterns)]
                    other => return Err(Error::UnsupportedFormat(other.to_string())),
                }
            };

            let mut stdout = std::io::stdout().lock();
            if json {
                let _ = serde_json::to_writer_pretty(&mut stdout, &info);
                let _ = writeln!(stdout);
            } else {
                let _ = writeln!(stdout, "Format:       {}", info.format);
                let _ = writeln!(stdout, "Entries:      {}", info.entry_count);
                let _ = writeln!(
                    stdout,
                    "Compressed:   {}",
                    format_size(info.compressed_size, human_readable)
                );
                let _ = writeln!(
                    stdout,
                    "Uncompressed: {}",
                    format_size(info.total_uncompressed, human_readable)
                );
            }
        }

        Command::Formats { json } => {
            print_formats(json)?;
        }

        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "rz", &mut std::io::stdout().lock());
        }

        Command::Man => {
            let cmd = Cli::command();
            let man = clap_mangen::Man::new(cmd);
            let mut stdout = std::io::stdout().lock();
            man.render(&mut stdout).map_err(Error::Io)?;
        }

        Command::Append {
            archive,
            input,
            format,
            level,
            exclude,
            exclude_from,
            follow_symlinks,
        } => {
            run_append(
                cli.progress,
                cli.verbose,
                archive,
                input,
                format,
                level,
                exclude,
                exclude_from,
                follow_symlinks,
                AppendMode::Append,
            )?;
        }

        Command::Update {
            archive,
            input,
            format,
            level,
            exclude,
            exclude_from,
            follow_symlinks,
        } => {
            run_append(
                cli.progress,
                cli.verbose,
                archive,
                input,
                format,
                level,
                exclude,
                exclude_from,
                follow_symlinks,
                AppendMode::Update,
            )?;
        }

        Command::Remove {
            archive,
            patterns,
            format,
            level,
        } => {
            let fmt = resolve_input_format(format, &archive)?;
            modify::remove(&archive, fmt, &patterns, level)?;
        }

        Command::Convert {
            input,
            output,
            from,
            to,
            level,
            force,
        } => {
            run_convert(input, output, from, to, level, force)?;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_append(
    show_progress: bool,
    verbose: bool,
    archive: camino::Utf8PathBuf,
    input: Vec<camino::Utf8PathBuf>,
    format: Option<Format>,
    level: Option<u32>,
    exclude: Vec<String>,
    exclude_from: Vec<camino::Utf8PathBuf>,
    follow_symlinks: bool,
    mode: AppendMode,
) -> Result<()> {
    let fmt = resolve_input_format(format, &archive)?;
    let excludes = filter::build_excludes(exclude, &exclude_from)?;
    let base_progress: Box<dyn ProgressReport> = if show_progress {
        Box::new(BarProgress::spinner())
    } else {
        Box::new(NoProgress)
    };
    let verbose_progress;
    let progress: &dyn ProgressReport = if verbose {
        verbose_progress = VerboseReport::new(&*base_progress);
        &verbose_progress
    } else {
        &*base_progress
    };
    let opts = CompressOpts {
        level,
        excludes,
        follow_symlinks,
        exclude_vcs_ignores: false,
        no_recursion: false,
        progress,
        fixed_mtime: None,
        fixed_uid: None,
        fixed_gid: None,
        fixed_mode: None,
        newer_than: None,
        older_than: None,
        ignore_failed_read: false,
        password: None,
    };
    modify::append(&archive, fmt, &input, mode, &opts)?;
    progress.finish();
    Ok(())
}

/// Resolve the output format for `rz convert`.
///
/// Priority: explicit `--to` → extension of `--output` → error.
fn resolve_convert_output_format(
    to_format: Option<Format>,
    output: Option<&Utf8Path>,
) -> Result<Format> {
    if let Some(f) = to_format {
        return Ok(f);
    }
    if let Some(out) = output {
        if let Some(f) = Format::from_path(out) {
            return Ok(f);
        }
        return Err(Error::CannotInferFormat(out.to_owned()));
    }
    Err(Error::ConvertCannotInferOutputFormat)
}

/// Derive the output path when `--output` was omitted but `--to` was given.
///
/// Strips the input's extension(s) for `fmt_in` and appends `fmt_out`'s
/// canonical extension.  The directory component of `input` is preserved so
/// the output lands alongside the input.
///
/// Example: `/path/foo.tar.gz` + `--to tar-zst` → `/path/foo.tar.zst`
fn derive_convert_output(input: &Utf8Path, fmt_out: Format, fmt_in: Format) -> Utf8PathBuf {
    // We need to work on the file name, then re-join with the parent.
    let name = input.file_name().unwrap_or("archive");
    let stem = {
        let mut s = name;
        for ext in fmt_in.recognized_extensions() {
            if s.len() >= ext.len() && s[s.len() - ext.len()..].eq_ignore_ascii_case(ext) {
                s = &s[..s.len() - ext.len()];
                break;
            }
        }
        if s.is_empty() { "archive" } else { s }
    };
    let new_name = format!("{stem}{}", fmt_out.extension());
    match input.parent() {
        Some(parent) if !parent.as_str().is_empty() => parent.join(new_name),
        _ => Utf8PathBuf::from(new_name),
    }
}

/// Return `true` when two paths refer to the same filesystem object.
///
/// Canonicalization is attempted on both sides; if either fails (e.g. the
/// output doesn't exist yet) the raw `Utf8Path` strings are compared instead.
fn paths_canonically_equal(a: &Utf8Path, b: &Utf8Path) -> bool {
    let canon_a = a.canonicalize().ok();
    let canon_b = b.canonicalize().ok();
    match (canon_a, canon_b) {
        (Some(ca), Some(cb)) => ca == cb,
        _ => a == b,
    }
}

/// Dispatch decompress to the correct format module.
fn dispatch_decompress(
    fmt: Format,
    input: &Utf8Path,
    output_dir: &Utf8Path,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    match fmt {
        Format::Zip => zip::decompress(input, output_dir, opts)?,
        Format::Tar => tar::decompress(input, output_dir, opts)?,
        Format::TarGz => tar_gz::decompress(input, output_dir, opts)?,
        Format::TarZst => tar_zst::decompress(input, output_dir, opts)?,
        Format::TarXz => tar_xz::decompress(input, output_dir, opts)?,
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => tar_bz2::decompress(input, output_dir, opts)?,
        Format::SevenZ => seven_z::decompress(input, output_dir, opts)?,
        #[allow(unreachable_patterns)]
        other => return Err(Error::UnsupportedFormat(other.to_string())),
    }
    Ok(())
}

/// Dispatch compress to the correct format module.
fn dispatch_compress(
    fmt: Format,
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    match fmt {
        Format::Zip => zip::compress(inputs, output, opts)?,
        Format::Tar => tar::compress(inputs, output, opts)?,
        Format::TarGz => tar_gz::compress(inputs, output, opts)?,
        Format::TarZst => tar_zst::compress(inputs, output, opts)?,
        Format::TarXz => tar_xz::compress(inputs, output, opts)?,
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => tar_bz2::compress(inputs, output, opts)?,
        Format::SevenZ => seven_z::compress(inputs, output, opts)?,
        #[allow(unreachable_patterns)]
        other => return Err(Error::UnsupportedFormat(other.to_string())),
    }
    Ok(())
}

fn run_convert(
    input: Utf8PathBuf,
    output: Option<Utf8PathBuf>,
    from_format: Option<Format>,
    to_format: Option<Format>,
    level: Option<u32>,
    force: bool,
) -> Result<()> {
    let fmt_in = resolve_input_format(from_format, &input)?;
    let fmt_out = resolve_convert_output_format(to_format, output.as_deref())?;

    let output_path = match output {
        Some(p) => p,
        None => derive_convert_output(&input, fmt_out, fmt_in),
    };

    if !force && fs_err::metadata(&output_path).is_ok() {
        return Err(Error::FileExists(output_path));
    }

    if paths_canonically_equal(&input, &output_path) {
        return Err(Error::ConvertSamePath(output_path));
    }

    // Extract input into a temporary directory, then re-compress from there.
    let tmp = tempfile::tempdir()?;
    let tmp_dir = Utf8Path::from_path(tmp.path())
        .ok_or_else(|| Error::InvalidUtf8Path(tmp.path().display().to_string()))?
        .to_owned();

    let dec_opts = DecompressOpts::new(
        true,
        0,
        globset::GlobSet::empty(),
        globset::GlobSet::empty(),
    );
    dispatch_decompress(fmt_in, &input, &tmp_dir, &dec_opts)?;

    // Compress from the children of tmp_dir so the archive entries are named
    // after the original archive's top-level entries, not the tempdir itself.
    let mut children: Vec<Utf8PathBuf> = Vec::new();
    for entry in fs_err::read_dir(&tmp_dir)? {
        let entry = entry?;
        let p = entry.path();
        let utf8 = Utf8PathBuf::try_from(p)
            .map_err(|e| Error::InvalidUtf8Path(e.into_path_buf().display().to_string()))?;
        children.push(utf8);
    }

    let comp_opts = CompressOpts::new(level, globset::GlobSet::empty());
    dispatch_compress(fmt_out, &children, &output_path, &comp_opts)?;

    Ok(())
}

fn print_formats(json: bool) -> Result<()> {
    use clap::ValueEnum;
    use serde::Serialize;

    #[derive(Serialize)]
    #[serde(rename_all = "lowercase")]
    enum OutputStatus {
        Enabled,
        Disabled,
    }

    #[derive(Serialize)]
    struct OutputFormat {
        format: String,
        extension: String,
        backend: Option<String>,
        status: OutputStatus,
    }

    // Built from the `Format` variants so the listed ids and extensions are
    // the exact strings `--format` accepts and `from_path` recognises — a
    // hand-written table here once drifted (`tar-cz`) and fed users an id the
    // parser rejects.
    let formats: Vec<OutputFormat> = Format::value_variants()
        .iter()
        .map(|fmt| {
            let (backend, status) = match fmt {
                Format::Zip => (Some("zip"), OutputStatus::Enabled),
                Format::Tar => (None, OutputStatus::Enabled),
                Format::TarGz => (Some("flate2"), OutputStatus::Enabled),
                Format::TarZst => (Some("ruzstd"), OutputStatus::Enabled),
                Format::TarXz => (
                    Some(if cfg!(feature = "xz2") {
                        "xz2 (C)"
                    } else {
                        "lzma-rust2"
                    }),
                    OutputStatus::Enabled,
                ),
                Format::TarBz2 => (
                    Some("bzip2 (C)"),
                    if cfg!(feature = "bzip2") {
                        OutputStatus::Enabled
                    } else {
                        OutputStatus::Disabled
                    },
                ),
                Format::SevenZ => (Some("sevenz-rust2"), OutputStatus::Enabled),
            };
            OutputFormat {
                format: fmt.to_string(),
                extension: fmt.extension().to_owned(),
                backend: backend.map(str::to_owned),
                status,
            }
        })
        .collect();

    if json {
        let mut stdout = std::io::stdout().lock();
        let json = serde_json::to_string(&formats).map_err(std::io::Error::other)?;
        let _ = writeln!(stdout, "{}", json);
    } else {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(
            stdout,
            "{:<12} {:<12} {:<16} STATUS",
            "FORMAT", "EXTENSION", "BACKEND"
        );
        let _ = writeln!(stdout, "{}", "-".repeat(52));

        for OutputFormat {
            format,
            extension,
            backend,
            status,
        } in formats
        {
            let status = match status {
                OutputStatus::Enabled => "enabled",
                OutputStatus::Disabled => "disabled",
            };

            let backend = match backend {
                Some(backend) => backend,
                None => "-".into(),
            };

            let _ = writeln!(
                stdout,
                "{format:<12} {extension:<12} {backend:<16} {status}"
            );
        }
    }
    Ok(())
}
