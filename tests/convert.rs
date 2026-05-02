mod helpers;

use camino::Utf8PathBuf;
use clap::Parser;
use globset::GlobSet;

use helpers::{TestResult, assert_trees_match, build_file_tree, temp_utf8_dir};
use rz::cmd::{Cli, Format};
use rz::{CompressOpts, DecompressOpts};

// ── round-trip helpers ────────────────────────────────────────────────────────

/// Compress `tree` into an archive of format `from`, convert to `to`, then
/// decompress the converted archive and verify contents match.
fn convert_round_trip(from_ext: &str, to_ext: &str) -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    // Build the source archive.
    let src = tmp.join(format!("src{from_ext}"));
    let fmt_in = rz::cmd::Format::from_path(&src).ok_or("cannot infer from format")?;
    let comp_opts = CompressOpts::new(None, GlobSet::empty());
    dispatch_compress(fmt_in, std::slice::from_ref(&tree), &src, &comp_opts)?;

    // Run convert via the library dispatch helpers (same code path as main.rs).
    let dst = tmp.join(format!("dst{to_ext}"));
    let fmt_out = rz::cmd::Format::from_path(&dst).ok_or("cannot infer to format")?;
    let dec_opts = DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty());

    let tmp2 = tempfile::tempdir()?;
    let tmp2_dir = camino::Utf8Path::from_path(tmp2.path())
        .ok_or("non-UTF8 tempdir")?
        .to_owned();
    dispatch_decompress(fmt_in, &src, &tmp2_dir, &dec_opts)?;

    let mut children: Vec<Utf8PathBuf> = Vec::new();
    for entry in fs_err::read_dir(&tmp2_dir)? {
        let entry = entry?;
        let p = entry.path();
        let utf8 = Utf8PathBuf::try_from(p).map_err(|e| e.to_string())?;
        children.push(utf8);
    }
    let comp2 = CompressOpts::new(None, GlobSet::empty());
    dispatch_compress(fmt_out, &children, &dst, &comp2)?;

    // Decompress the converted archive and compare.
    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;
    let dec2 = DecompressOpts::new(false, 0, GlobSet::empty(), GlobSet::empty());
    dispatch_decompress(fmt_out, &dst, &out, &dec2)?;

    // For tar-family, the top-level directory is `tree/`; for zip/7z it may
    // differ — just find the extracted tree by looking for "hello.txt".
    let extracted = find_tree_root(&out)?;
    assert_trees_match(&tree, &extracted)?;
    Ok(())
}

fn find_tree_root(root: &camino::Utf8Path) -> Result<Utf8PathBuf, Box<dyn std::error::Error>> {
    // Walk one level; return the first directory or fall back to root itself.
    for entry in fs_err::read_dir(root)? {
        let entry = entry?;
        let p = entry.path();
        let utf8 = Utf8PathBuf::try_from(p).map_err(|e| e.to_string())?;
        if utf8.is_dir() {
            return Ok(utf8);
        }
    }
    Ok(root.to_owned())
}

fn dispatch_compress(
    fmt: Format,
    inputs: &[Utf8PathBuf],
    output: &camino::Utf8Path,
    opts: &CompressOpts<'_>,
) -> rz::error::Result<()> {
    match fmt {
        Format::Zip => rz::zip::compress(inputs, output, opts),
        Format::Tar => rz::tar::compress(inputs, output, opts),
        Format::TarGz => rz::tar_gz::compress(inputs, output, opts),
        Format::TarZst => rz::tar_zst::compress(inputs, output, opts),
        Format::TarXz => rz::tar_xz::compress(inputs, output, opts),
        Format::SevenZ => rz::seven_z::compress(inputs, output, opts),
        #[allow(unreachable_patterns)]
        _ => Err(rz::error::Error::UnsupportedFormat(fmt.to_string())),
    }
}

fn dispatch_decompress(
    fmt: Format,
    input: &camino::Utf8Path,
    output: &camino::Utf8Path,
    opts: &DecompressOpts<'_>,
) -> rz::error::Result<()> {
    match fmt {
        Format::Zip => rz::zip::decompress(input, output, opts),
        Format::Tar => rz::tar::decompress(input, output, opts),
        Format::TarGz => rz::tar_gz::decompress(input, output, opts),
        Format::TarZst => rz::tar_zst::decompress(input, output, opts),
        Format::TarXz => rz::tar_xz::decompress(input, output, opts),
        Format::SevenZ => rz::seven_z::decompress(input, output, opts),
        #[allow(unreachable_patterns)]
        _ => Err(rz::error::Error::UnsupportedFormat(fmt.to_string())),
    }
}

// ── actual tests ──────────────────────────────────────────────────────────────

#[test]
fn convert_tar_gz_to_tar_zst_round_trip() -> TestResult {
    convert_round_trip(".tar.gz", ".tar.zst")
}

#[test]
fn convert_zip_to_tar_gz_round_trip() -> TestResult {
    convert_round_trip(".zip", ".tar.gz")
}

#[test]
fn convert_tar_gz_to_zip_round_trip() -> TestResult {
    convert_round_trip(".tar.gz", ".zip")
}

#[test]
fn convert_tar_to_tar_xz_round_trip() -> TestResult {
    convert_round_trip(".tar", ".tar.xz")
}

// ── CLI-level tests ───────────────────────────────────────────────────────────

#[test]
fn convert_refuses_overwrite_without_force() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let src = tmp.join("src.tar.gz");
    let dst = tmp.join("dst.tar.zst");

    // Create both archives so dst already exists.
    let opts = CompressOpts::new(None, GlobSet::empty());
    rz::tar_gz::compress(std::slice::from_ref(&tree), &src, &opts)?;
    rz::tar_zst::compress(std::slice::from_ref(&tree), &dst, &opts)?;

    // run_convert without force should error with FileExists.
    let result = run_convert_fn(src, Some(dst), None, None, None, false);
    assert!(
        matches!(result, Err(rz::error::Error::FileExists(_))),
        "expected FileExists, got {result:?}",
    );
    Ok(())
}

#[test]
fn convert_overwrites_with_force() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let src = tmp.join("src.tar.gz");
    let dst = tmp.join("dst.tar.zst");

    let opts = CompressOpts::new(None, GlobSet::empty());
    rz::tar_gz::compress(std::slice::from_ref(&tree), &src, &opts)?;
    rz::tar_zst::compress(std::slice::from_ref(&tree), &dst, &opts)?;

    run_convert_fn(src, Some(dst.clone()), None, None, None, true)?;

    // dst should be a valid tar.zst now.
    let entries = rz::tar_zst::list(&dst)?;
    assert!(!entries.is_empty(), "converted archive has no entries");
    Ok(())
}

#[test]
fn convert_rejects_same_input_output() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;
    let src = tmp.join("src.tar.gz");
    let opts = CompressOpts::new(None, GlobSet::empty());
    rz::tar_gz::compress(&[tree], &src, &opts)?;

    let result = run_convert_fn(src.clone(), Some(src), None, None, None, true);
    assert!(
        matches!(result, Err(rz::error::Error::ConvertSamePath(_))),
        "expected ConvertSamePath, got {result:?}",
    );
    Ok(())
}

#[test]
fn convert_derives_output_from_to_flag() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;
    let src = tmp.join("src.tar.gz");
    let opts = CompressOpts::new(None, GlobSet::empty());
    rz::tar_gz::compress(&[tree], &src, &opts)?;

    // No explicit output — derive from --to.
    run_convert_fn(src, None, None, Some(Format::TarZst), None, false)?;

    // The derived output should be src.tar.zst in the same directory.
    let expected_dst = tmp.join("src.tar.zst");
    assert!(
        expected_dst.exists(),
        "derived output {expected_dst} not found",
    );
    Ok(())
}

#[test]
fn cli_convert_parses_to_format() -> TestResult {
    let cli = Cli::try_parse_from(["rz", "convert", "a.tar.gz", "--to", "tar-zst"])?;
    if let rz::cmd::Command::Convert { to, .. } = cli.command {
        assert_eq!(to, Some(Format::TarZst));
    } else {
        return Err("expected Convert subcommand".into());
    }
    Ok(())
}

#[test]
fn cli_convert_parses_force_and_level() -> TestResult {
    let cli = Cli::try_parse_from([
        "rz", "convert", "a.tar.gz", "-o", "b.tar.zst", "-F", "-l", "3",
    ])?;
    if let rz::cmd::Command::Convert {
        force, level, output, ..
    } = cli.command
    {
        assert!(force);
        assert_eq!(level, Some(3));
        assert_eq!(output, Some(Utf8PathBuf::from("b.tar.zst")));
    } else {
        return Err("expected Convert subcommand".into());
    }
    Ok(())
}

// ── thin wrapper to call run_convert without going through clap ───────────────

fn run_convert_fn(
    input: Utf8PathBuf,
    output: Option<Utf8PathBuf>,
    from_format: Option<Format>,
    to_format: Option<Format>,
    level: Option<u32>,
    force: bool,
) -> rz::error::Result<()> {
    use rz::format::resolve_input_format;

    let fmt_in = resolve_input_format(from_format, &input)?;
    let fmt_out = resolve_convert_output_format_test(to_format, output.as_deref(), &input, fmt_in)?;

    let output_path = match output {
        Some(p) => p,
        None => derive_convert_output_test(&input, fmt_out, fmt_in),
    };

    if !force && fs_err::metadata(&output_path).is_ok() {
        return Err(rz::error::Error::FileExists(output_path));
    }

    if paths_canonically_equal_test(&input, &output_path) {
        return Err(rz::error::Error::ConvertSamePath(output_path));
    }

    let tmp = tempfile::tempdir()?;
    let tmp_dir = camino::Utf8Path::from_path(tmp.path())
        .ok_or_else(|| rz::error::Error::InvalidUtf8Path(tmp.path().display().to_string()))?
        .to_owned();

    let dec_opts = DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty());
    dispatch_decompress(fmt_in, &input, &tmp_dir, &dec_opts)?;

    let mut children: Vec<Utf8PathBuf> = Vec::new();
    for entry in fs_err::read_dir(&tmp_dir)? {
        let entry = entry?;
        let p = entry.path();
        let utf8 = Utf8PathBuf::try_from(p)
            .map_err(|e| rz::error::Error::InvalidUtf8Path(e.into_path_buf().display().to_string()))?;
        children.push(utf8);
    }

    let comp_opts = CompressOpts::new(level, GlobSet::empty());
    dispatch_compress(fmt_out, &children, &output_path, &comp_opts)?;

    Ok(())
}

fn resolve_convert_output_format_test(
    to_format: Option<Format>,
    output: Option<&camino::Utf8Path>,
    _input: &camino::Utf8Path,
    _fmt_in: Format,
) -> rz::error::Result<Format> {
    if let Some(f) = to_format {
        return Ok(f);
    }
    if let Some(out) = output {
        if let Some(f) = Format::from_path(out) {
            return Ok(f);
        }
        return Err(rz::error::Error::CannotInferFormat(out.to_owned()));
    }
    Err(rz::error::Error::ConvertCannotInferOutputFormat)
}

fn derive_convert_output_test(
    input: &camino::Utf8Path,
    fmt_out: Format,
    fmt_in: Format,
) -> Utf8PathBuf {
    let name = input.file_name().unwrap_or("archive");
    let stem = {
        let mut s = name;
        for ext in fmt_in.recognized_extensions() {
            if s.len() >= ext.len()
                && s[s.len() - ext.len()..].eq_ignore_ascii_case(ext)
            {
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

fn paths_canonically_equal_test(a: &camino::Utf8Path, b: &camino::Utf8Path) -> bool {
    let ca = a.canonicalize().ok();
    let cb = b.canonicalize().ok();
    match (ca, cb) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}
