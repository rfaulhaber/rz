use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

use rz::cmd::{Cli, Command, Format};
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

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Compress {
            input,
            output,
            format,
            level,
            exclude,
        } => {
            let to_stdout = output.as_ref().is_some_and(|o| is_stdio(o.as_str()));

            let fmt = if to_stdout {
                // Cannot infer format from "-", need explicit --format
                format.ok_or(Error::CannotInferOutput)?
            } else {
                resolve_compress_format(format, output.as_deref())?
            };

            if to_stdout && requires_seek(&fmt) {
                return Err(Error::StdoutNotSupported(
                    format!("{:?}", fmt),
                ));
            }

            // Disable progress bar when writing to stdout to avoid corrupting output.
            let base_progress: Box<dyn ProgressReport> = if cli.progress && !to_stdout {
                Box::new(BarProgress::spinner())
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
                excludes: filter::build_exclude_set(&exclude)?,
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
        }

        Command::Decompress {
            input,
            output,
            format,
            force,
            to_stdout,
            strip_components,
            exclude,
            include,
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

            let base_progress: Box<dyn ProgressReport> = if cli.progress && !from_stdin {
                let file_size = fs_err::metadata(&input)?.len();
                Box::new(BarProgress::bytes(file_size))
            } else if cli.progress {
                Box::new(BarProgress::spinner())
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
                strip_components,
                includes: {
                    let mut all_includes = include;
                    all_includes.extend(paths);
                    filter::build_include_set(&all_includes)?
                },
                excludes: filter::build_exclude_set(&exclude)?,
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
        }

        Command::List {
            input,
            format,
            long,
            exclude,
        } => {
            let fmt = resolve_input_format(format, &input)?;
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
                other => return Err(Error::UnsupportedFormat(
                    format!("{:?}", other),
                )),
            };

            let excludes = filter::build_exclude_set(&exclude)?;
            let mut stdout = std::io::stdout().lock();
            for entry in &entries {
                if !excludes.is_empty()
                    && excludes.is_match(entry.path.as_str().trim_end_matches('/'))
                {
                    continue;
                }
                if long {
                    let kind = if entry.is_dir { "d" } else { "-" };
                    let _ = writeln!(
                        stdout,
                        "{kind}{:06o}  {:>10}  {}",
                        entry.mode, entry.size, entry.path,
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
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "ok");
        }

        Command::Info { input, format } => {
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
            let _ = writeln!(stdout, "Compressed:   {} bytes", info.compressed_size);
            let _ = writeln!(stdout, "Uncompressed: {} bytes", info.total_uncompressed);
        }
    }

    Ok(())
}
