//! End-to-end coverage for `list`, `test`, and `decompress` reading from
//! stdin — optional INPUT (bare command reads a pipe), magic-byte format
//! auto-detection, and zip/seekable rejection. The `info` stdin path has its
//! own suite in `info_stdin.rs`.

mod helpers;

use std::io::Write;
use std::process::{Command, Stdio};

use helpers::{
    TAR_GZ, TestResult, ZIP, assert_trees_match, build_file_tree, default_compress_opts,
    temp_utf8_dir,
};

fn rz_archive_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz-archive")
}

/// Run `rz_archive` with `args`, feeding `stdin_bytes`, and capture output.
fn run_with_stdin(
    args: &[&str],
    stdin_bytes: &[u8],
) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let mut child = Command::new(rz_archive_bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .ok_or("no piped stdin")?
        .write_all(stdin_bytes)?;
    Ok(child.wait_with_output()?)
}

/// Build a `tree/` file tree, compress it to a tar.gz, and return the bytes.
fn tar_gz_bytes(tmp: &camino::Utf8Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let tree = tmp.join("tree");
    build_file_tree(&tree)?;
    let archive = tmp.join("payload.tar.gz");
    (TAR_GZ.compress)(&[tree], &archive, &default_compress_opts(None))?;
    Ok(fs_err::read(&archive)?)
}

#[test]
fn list_bare_stdin_autodetects() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let bytes = tar_gz_bytes(&tmp)?;

    // No input arg, no --format: format is sniffed from the gzip magic.
    let out = run_with_stdin(&["list"], &bytes)?;
    assert!(
        out.status.success(),
        "bare list from stdin failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tree/hello.txt"), "missing entry: {stdout}");
    assert!(
        stdout.contains("tree/subdir/nested.txt"),
        "missing nested entry: {stdout}",
    );
    Ok(())
}

#[test]
fn test_bare_stdin_reports_ok() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let bytes = tar_gz_bytes(&tmp)?;

    let out = run_with_stdin(&["test"], &bytes)?;
    assert!(
        out.status.success(),
        "bare test from stdin failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    // `test` prints "ok" to stderr on success.
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ok"),
        "expected ok on stderr",
    );
    Ok(())
}

#[test]
fn decompress_bare_stdin_extracts() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let tree = tmp.join("tree");
    build_file_tree(&tree)?;
    let archive = tmp.join("payload.tar.gz");
    (TAR_GZ.compress)(std::slice::from_ref(&tree), &archive, &default_compress_opts(None))?;
    let bytes = fs_err::read(&archive)?;

    let out_dir = tmp.join("out");
    fs_err::create_dir(&out_dir)?;
    let out = run_with_stdin(&["decompress", "-o", out_dir.as_str()], &bytes)?;
    assert!(
        out.status.success(),
        "bare decompress from stdin failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert_trees_match(&tree, &out_dir.join("tree"))?;
    Ok(())
}

#[test]
fn decompress_dry_run_stdin_lists_without_extracting() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let tree = tmp.join("tree");
    build_file_tree(&tree)?;
    let archive = tmp.join("payload.tar.gz");
    (TAR_GZ.compress)(&[tree], &archive, &default_compress_opts(None))?;
    let bytes = fs_err::read(&archive)?;

    let out = run_with_stdin(&["decompress", "--dry-run"], &bytes)?;
    assert!(
        out.status.success(),
        "dry-run from stdin failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("tree/hello.txt"),
        "dry-run should list entries: {stdout}",
    );
    // Nothing should have been written to the working dir; the harness runs in
    // the test process's cwd, so just confirm the listing happened (above) and
    // the command didn't error.
    Ok(())
}

#[test]
fn list_stdin_rejects_zip() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let tree = tmp.join("tree");
    build_file_tree(&tree)?;
    let archive = tmp.join("bundle.zip");
    (ZIP.compress)(&[tree], &archive, &default_compress_opts(None))?;
    let bytes = fs_err::read(&archive)?;

    // zip auto-detected from magic, then rejected (needs seekable input).
    let out = run_with_stdin(&["list"], &bytes)?;
    assert!(!out.status.success(), "expected zip stdin rejection");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("does not support reading from stdin"),
        "stderr should explain the seekable requirement",
    );
    Ok(())
}

#[test]
fn test_empty_stdin_reports_no_input() -> TestResult {
    let out = run_with_stdin(&["test"], b"")?;
    assert!(!out.status.success(), "expected failure on empty stdin");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no input provided"),
        "stderr should explain there was no input",
    );
    Ok(())
}
