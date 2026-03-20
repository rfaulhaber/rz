use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

use rz::cmd::{Cli, Command, Format};
use rz::error::{Error, Result};
use rz::filter;
use rz::format::{resolve_compress_format, resolve_input_format};
use rz::progress::{BarProgress, NoProgress, ProgressReport};
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

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Compress {
            input,
            output,
            format,
            level,
            exclude,
        } => {
            let fmt = resolve_compress_format(
                format,
                output.as_deref(),
            )?;
            let output = match output {
                Some(o) => o,
                None => fmt.default_output(&input[0]),
            };
            let progress: Box<dyn ProgressReport> = if cli.progress {
                Box::new(BarProgress::spinner())
            } else {
                Box::new(NoProgress)
            };
            let opts = CompressOpts {
                level,
                excludes: filter::build_exclude_set(&exclude)?,
                progress: &*progress,
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
            progress.finish();
        }

        Command::Decompress {
            input,
            output,
            format,
            force,
            strip_components,
            exclude,
        } => {
            let fmt = resolve_input_format(format, &input)?;
            let output = output.unwrap_or_else(|| ".".into());
            let progress: Box<dyn ProgressReport> = if cli.progress {
                let file_size = fs_err::metadata(&input)?.len();
                Box::new(BarProgress::bytes(file_size))
            } else {
                Box::new(NoProgress)
            };
            let opts = DecompressOpts {
                force,
                strip_components,
                excludes: filter::build_exclude_set(&exclude)?,
                progress: &*progress,
            };
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
