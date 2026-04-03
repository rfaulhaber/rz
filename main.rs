use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

use rz::cmd::{Cli, Command, Format, SortField};
use rz::error::{Error, Result};
use rz::filter;
use rz::format::{resolve_compress_format, resolve_input_format};
use rz::progress::{BarProgress, NoProgress, ProgressReport, VerboseReport};
use rz::{seven_z, tar, tar_gz, tar_xz, tar_zst, zip, CompressOpts, DecompressOpts};
#[cfg(feature = "bzip2")]
use rz::tar_bz2;

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
            exclude,
            exclude_from,
            files_from,
            exclude_vcs,
            exclude_backups,
            follow_symlinks,
            totals,
            dry_run,
        } => {
            // Merge --files-from paths into input list.
            if let Some(ref list_file) = files_from {
                let extra = filter::read_paths_from_file(list_file)?;
                input.extend(extra);
            }

            // Build combined exclude set.
            let mut all_excludes = exclude;
            for path in &exclude_from {
                all_excludes.extend(filter::read_patterns_from_file(path)?);
            }
            if exclude_vcs {
                for pat in &[".git", ".hg", ".svn", ".bzr", "_darcs", ".pijul", "CVS"] {
                    all_excludes.push((*pat).to_owned());
                }
            }
            if exclude_backups {
                for pat in &["*~", "*.bak", "#*#", ".#*"] {
                    all_excludes.push((*pat).to_owned());
                }
            }
            let excludes = filter::build_exclude_set(&all_excludes)?;

            // Dry-run: list what would be compressed and exit.
            if dry_run {
                let paths = filter::collect_compress_paths(&input, &excludes)?;
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
                return Err(Error::StdoutNotSupported(
                    format!("{:?}", fmt),
                ));
            }

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
                progress,
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
                    _ => return Err(Error::StdoutNotSupported(format!("{:?}", fmt))),
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
                    other => return Err(Error::UnsupportedFormat(
                        format!("{:?}", other),
                    )),
                }
            }
            progress.finish();
            if totals {
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "Total bytes: {}", format_size(progress.position(), false));
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
                return Err(Error::StdinNotSupported(
                    format!("{:?}", fmt),
                ));
            }

            // Build combined exclude set.
            let mut all_excludes = exclude;
            for path in &exclude_from {
                all_excludes.extend(filter::read_patterns_from_file(path)?);
            }
            let excludes = filter::build_exclude_set(&all_excludes)?;
            let includes = {
                let mut all_includes = include;
                all_includes.extend(paths);
                filter::build_include_set(&all_includes)?
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
                    other => return Err(Error::UnsupportedFormat(format!("{:?}", other))),
                };
                let mut stdout = std::io::stdout().lock();
                for entry in &entries {
                    if !filter::should_extract(entry.path.as_str(), &includes, &excludes) {
                        continue;
                    }
                    if let Some(stripped) = filter::strip_components(&entry.path, strip_components) {
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
            let opts = DecompressOpts {
                force,
                no_overwrite,
                keep_newer,
                no_directory,
                strip_components,
                includes,
                excludes,
                progress,
            };

            if to_stdout {
                let mut stdout = std::io::stdout().lock();
                if from_stdin {
                    let stdin = std::io::stdin().lock();
                    match fmt {
                        Format::Tar => tar::decompress_reader_to_writer(stdin, &mut stdout, &opts)?,
                        Format::TarGz => tar_gz::decompress_reader_to_writer(stdin, &mut stdout, &opts)?,
                        Format::TarZst => tar_zst::decompress_reader_to_writer(std::io::BufReader::new(stdin), &mut stdout, &opts)?,
                        Format::TarXz => tar_xz::decompress_reader_to_writer(stdin, &mut stdout, &opts)?,
                        #[cfg(feature = "bzip2")]
                        Format::TarBz2 => tar_bz2::decompress_reader_to_writer(stdin, &mut stdout, &opts)?,
                        _ => return Err(Error::StdinNotSupported(format!("{:?}", fmt))),
                    }
                } else {
                    match fmt {
                        Format::Zip => zip::decompress_to_writer(&input, &mut stdout, &opts)?,
                        Format::Tar => tar::decompress_to_writer(&input, &mut stdout, &opts)?,
                        Format::TarGz => tar_gz::decompress_to_writer(&input, &mut stdout, &opts)?,
                        Format::TarZst => tar_zst::decompress_to_writer(&input, &mut stdout, &opts)?,
                        Format::TarXz => tar_xz::decompress_to_writer(&input, &mut stdout, &opts)?,
                        #[cfg(feature = "bzip2")]
                        Format::TarBz2 => tar_bz2::decompress_to_writer(&input, &mut stdout, &opts)?,
                        Format::SevenZ => seven_z::decompress_to_writer(&input, &mut stdout, &opts)?,
                        #[allow(unreachable_patterns)]
                        other => return Err(Error::UnsupportedFormat(format!("{:?}", other))),
                    }
                }
            } else if from_stdin {
                let output = output.unwrap_or_else(|| ".".into());
                let stdin = std::io::stdin().lock();
                match fmt {
                    Format::Tar => tar::decompress_from_reader(stdin, &output, &opts)?,
                    Format::TarGz => tar_gz::decompress_from_reader(stdin, &output, &opts)?,
                    Format::TarZst => tar_zst::decompress_from_reader(std::io::BufReader::new(stdin), &output, &opts)?,
                    Format::TarXz => tar_xz::decompress_from_reader(stdin, &output, &opts)?,
                    #[cfg(feature = "bzip2")]
                    Format::TarBz2 => tar_bz2::decompress_from_reader(stdin, &output, &opts)?,
                    _ => return Err(Error::StdinNotSupported(format!("{:?}", fmt))),
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
                    other => return Err(Error::UnsupportedFormat(
                        format!("{:?}", other),
                    )),
                }
            }
            progress.finish();
            if totals {
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "Total bytes: {}", format_size(progress.position(), false));
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
                other => return Err(Error::UnsupportedFormat(
                    format!("{:?}", other),
                )),
            };

            // Build combined exclude set.
            let mut all_excludes = exclude;
            for path in &exclude_from {
                all_excludes.extend(filter::read_patterns_from_file(path)?);
            }
            let excludes = filter::build_exclude_set(&all_excludes)?;

            if let Some(ref field) = sort {
                match field {
                    SortField::Name => entries.sort_by(|a, b| a.path.cmp(&b.path)),
                    SortField::Size => entries.sort_by(|a, b| a.size.cmp(&b.size)),
                    SortField::Date => entries.sort_by(|a, b| a.mtime.cmp(&b.mtime)),
                }
            }

            let mut stdout = std::io::stdout().lock();
            for entry in &entries {
                if !excludes.is_empty()
                    && excludes.is_match(entry.path.as_str().trim_end_matches('/'))
                {
                    continue;
                }
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
                other => return Err(Error::UnsupportedFormat(
                    format!("{:?}", other),
                )),
            }
            progress.finish();
            if !cli.quiet {
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "ok");
            }
        }

        Command::Info { input, format, human_readable } => {
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
                other => return Err(Error::UnsupportedFormat(
                    format!("{:?}", other),
                )),
            };

            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "Format:       {}", info.format);
            let _ = writeln!(stdout, "Entries:      {}", info.entry_count);
            let _ = writeln!(stdout, "Compressed:   {}", format_size(info.compressed_size, human_readable));
            let _ = writeln!(stdout, "Uncompressed: {}", format_size(info.total_uncompressed, human_readable));
        }
    }

    Ok(())
}
