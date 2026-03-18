use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

use rz::cmd::{Cli, Command, Format};
use rz::error::{Error, Result};
use rz::format::{resolve_compress_format, resolve_input_format};
use rz::{seven_z, tar, tar_gz, tar_xz, tar_zst, zip};
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
            threads: _,
        } => {
            let fmt = resolve_compress_format(
                format,
                output.as_deref(),
            )?;
            let output = match output {
                Some(o) => o,
                None => fmt.default_output(&input[0]),
            };
            match fmt {
                Format::Zip => zip::compress(&input, &output, level)?,
                Format::Tar => tar::compress(&input, &output, level)?,
                Format::TarGz => tar_gz::compress(&input, &output, level)?,
                Format::TarZst => tar_zst::compress(&input, &output, level)?,
                Format::TarXz => tar_xz::compress(&input, &output, level)?,
                #[cfg(feature = "bzip2")]
                Format::TarBz2 => tar_bz2::compress(&input, &output, level)?,
                Format::SevenZ => seven_z::compress(&input, &output, level)?,
                #[allow(unreachable_patterns)]
                other => return Err(Error::UnsupportedFormat(
                    format!("{:?}", other),
                )),
            }
        }

        Command::Decompress {
            input,
            output,
            format,
            force,
        } => {
            let fmt = resolve_input_format(format, &input)?;
            let output = output.unwrap_or_else(|| ".".into());
            match fmt {
                Format::Zip => zip::decompress(&input, &output, force)?,
                Format::Tar => tar::decompress(&input, &output, force)?,
                Format::TarGz => tar_gz::decompress(&input, &output, force)?,
                Format::TarZst => tar_zst::decompress(&input, &output, force)?,
                Format::TarXz => tar_xz::decompress(&input, &output, force)?,
                #[cfg(feature = "bzip2")]
                Format::TarBz2 => tar_bz2::decompress(&input, &output, force)?,
                Format::SevenZ => seven_z::decompress(&input, &output, force)?,
                #[allow(unreachable_patterns)]
                other => return Err(Error::UnsupportedFormat(
                    format!("{:?}", other),
                )),
            }
        }

        Command::List {
            input,
            format,
            long,
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

            let mut stdout = std::io::stdout().lock();
            for entry in &entries {
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
