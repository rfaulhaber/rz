use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

use rz::cmd::{Cli, Command, Format};
use rz::error::{Error, Result};
use rz::format::{resolve_compress_format, resolve_input_format};
use rz::{seven_z, tar_gz};

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
                Format::TarGz => tar_gz::compress(&input, &output, level)?,
                Format::SevenZ => seven_z::compress(&input, &output, level)?,
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
                Format::TarGz => tar_gz::decompress(&input, &output, force)?,
                Format::SevenZ => seven_z::decompress(&input, &output, force)?,
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
                Format::TarGz => tar_gz::list(&input)?,
                Format::SevenZ => seven_z::list(&input)?,
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
                Format::TarGz => tar_gz::info(&input)?,
                Format::SevenZ => seven_z::info(&input)?,
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
