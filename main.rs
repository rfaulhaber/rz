use std::io::Write;
use std::process::ExitCode;

use clap::{CommandFactory, Parser};

use rz::cmd::{Cli, Command, Format, SortField};
use rz::error::{Error, Result};
use rz::filter;
use rz::format::{resolve_compress_format, resolve_input_format};
use rz::progress::{BarProgress, NoProgress, ProgressReport, VerboseReport};
#[cfg(feature = "bzip2")]
use rz::tar_bz2;
use rz::{CompressOpts, DecompressOpts, seven_z, tar, tar_gz, tar_xz, tar_zst, zip};

fn main() -> ExitCode {
    let cli = Cli::parse();
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

/// Reproducibility overrides (`--mtime`, `--owner`, `--group`) require writing
/// per-entry metadata that zip and 7z don't expose through our underlying
/// writers: zip has no UID/GID field in its central directory, and
/// `sevenz-rust2::ArchiveWriter` has no per-entry metadata hook.  Rather than
/// silently no-op the flags, reject up front with a clear pointer to the
/// tar-family formats that do support reproducibility.
fn reject_reproducibility_for_non_tar(
    fmt: &Format,
    mtime: Option<u64>,
    owner: Option<u64>,
    group: Option<u64>,
) -> Result<()> {
    let is_tar_family = matches!(
        fmt,
        Format::Tar | Format::TarGz | Format::TarZst | Format::TarXz | Format::TarBz2
    );
    if is_tar_family {
        return Ok(());
    }
    let check = |flag: &'static str, value: Option<u64>| -> Result<()> {
        if value.is_some() {
            return Err(Error::ReproducibilityFlagUnsupported {
                flag,
                format: fmt.to_string(),
            });
        }
        Ok(())
    };
    check("--mtime", mtime)?;
    check("--owner", owner)?;
    check("--group", group)?;
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
        } => {
            let level = if store { Some(0) } else { level };

            // Merge --files-from paths into input list.
            if let Some(ref list_file) = files_from {
                let extra = filter::read_paths_from_file(list_file)?;
                input.extend(extra);
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
            reject_reproducibility_for_non_tar(&fmt, mtime, owner, group)?;

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
            totals,
            dry_run,
            paths,
        } => {
            let from_stdin = is_stdio(input.as_str());

            let fmt = if from_stdin {
                format.ok_or(Error::CannotInferFormatStdin)?
            } else {
                resolve_input_format(format, &input)?
            };

            if from_stdin && requires_seek(&fmt) {
                return Err(Error::StdinNotSupported(fmt.to_string()));
            }

            let excludes = filter::build_excludes(exclude, &exclude_from)?;
            let includes = {
                let mut all_includes = include;
                all_includes.extend(paths);
                filter::build_glob_set(&all_includes)?
            };

            // Dry-run: list what would be extracted and exit.
            if dry_run && !from_stdin {
                let entries = match fmt {
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
                progress,
            };

            if to_stdout {
                let mut stdout = std::io::stdout().lock();
                if from_stdin {
                    let stdin = std::io::stdin().lock();
                    match fmt {
                        Format::Tar => tar::decompress_reader_to_writer(stdin, &mut stdout, &opts)?,
                        Format::TarGz => {
                            tar_gz::decompress_reader_to_writer(stdin, &mut stdout, &opts)?
                        }
                        Format::TarZst => tar_zst::decompress_reader_to_writer(
                            std::io::BufReader::new(stdin),
                            &mut stdout,
                            &opts,
                        )?,
                        Format::TarXz => {
                            tar_xz::decompress_reader_to_writer(stdin, &mut stdout, &opts)?
                        }
                        #[cfg(feature = "bzip2")]
                        Format::TarBz2 => {
                            tar_bz2::decompress_reader_to_writer(stdin, &mut stdout, &opts)?
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
                let stdin = std::io::stdin().lock();
                match fmt {
                    Format::Tar => tar::decompress_from_reader(stdin, &output, &opts)?,
                    Format::TarGz => tar_gz::decompress_from_reader(stdin, &output, &opts)?,
                    Format::TarZst => tar_zst::decompress_from_reader(
                        std::io::BufReader::new(stdin),
                        &output,
                        &opts,
                    )?,
                    Format::TarXz => tar_xz::decompress_from_reader(stdin, &output, &opts)?,
                    #[cfg(feature = "bzip2")]
                    Format::TarBz2 => tar_bz2::decompress_from_reader(stdin, &output, &opts)?,
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
        } => {
            let fmt = resolve_input_format(format, &input)?;
            let mut entries = match fmt {
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

        Command::Test { input, format } => {
            let fmt = resolve_input_format(format, &input)?;
            let base_progress: Box<dyn ProgressReport> = if cli.progress {
                let file_size = fs_err::metadata(&input)?.len();
                Box::new(BarProgress::bytes(file_size))
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
            match fmt {
                Format::Zip => zip::test(&input, progress)?,
                Format::Tar => tar::test(&input, progress)?,
                Format::TarGz => tar_gz::test(&input, progress)?,
                Format::TarZst => tar_zst::test(&input, progress)?,
                Format::TarXz => tar_xz::test(&input, progress)?,
                #[cfg(feature = "bzip2")]
                Format::TarBz2 => tar_bz2::test(&input, progress)?,
                Format::SevenZ => seven_z::test(&input, progress)?,
                #[allow(unreachable_patterns)]
                other => return Err(Error::UnsupportedFormat(other.to_string())),
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
        } => {
            let fmt = resolve_input_format(format, &input)?;
            let info = match fmt {
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
    }

    Ok(())
}

fn print_formats(json: bool) -> Result<()> {
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

    let formats = vec![
        OutputFormat {
            format: "tar".into(),
            extension: ".tar".into(),
            backend: None,
            status: OutputStatus::Enabled,
        },
        OutputFormat {
            format: "tar-gz".into(),
            extension: ".tar.gz".into(),
            backend: Some("flate2".into()),
            status: OutputStatus::Enabled,
        },
        OutputFormat {
            format: "tar-zst".into(),
            extension: ".tar.zst".into(),
            backend: Some("ruzstd".into()),
            status: OutputStatus::Enabled,
        },
        OutputFormat {
            format: "tar-cz".into(),
            extension: ".tar.xz".into(),
            backend: Some(if cfg!(feature = "xz2") {
                "xz2 (C)".into()
            } else {
                "lzma-rs".into()
            }),
            status: OutputStatus::Enabled,
        },
        OutputFormat {
            format: "tar-bz2".into(),
            extension: ".tar.bz2".into(),
            backend: Some("bzip2".into()),
            status: if cfg!(feature = "bzip2") {
                OutputStatus::Enabled
            } else {
                OutputStatus::Disabled
            },
        },
        OutputFormat {
            format: "zip".into(),
            extension: ".zip".into(),
            backend: Some("zip".into()),
            status: OutputStatus::Enabled,
        },
        OutputFormat {
            format: "7z".into(),
            extension: ".7z".into(),
            backend: Some("sevenz-rust2".into()),
            status: OutputStatus::Enabled,
        },
    ];

    if json {
        let mut stdout = std::io::stdout().lock();
        let json = serde_json::to_string(&formats)
            .map_err(std::io::Error::other)?;
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
