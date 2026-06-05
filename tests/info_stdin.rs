//! End-to-end coverage for `info` reading from stdin (`info -`).
//!
//! The per-format counting/streaming logic lives in each module's
//! `info_from_reader`; this suite shells out to the compiled `rz_archive`
//! binary to verify the `main.rs` wiring: format-required-for-stdin,
//! seekable-format rejection, and that piped input yields the same metadata
//! as reading the file directly.

mod helpers;

use std::io::Write;
use std::process::{Command, Stdio};

use helpers::{TAR_GZ, TestResult, ZIP, build_file_tree, default_compress_opts, temp_utf8_dir};

/// Path to the `rz_archive` binary that Cargo just built for this test crate.
fn rz_archive_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz-archive")
}

/// Run `rz_archive` with `args`, feeding `stdin_bytes` on stdin, and capture
/// the result.
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

#[test]
fn tar_gz_info_from_stdin_matches_file() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;
    let archive = tmp.join("payload.tar.gz");
    (TAR_GZ.compress)(&[tree], &archive, &default_compress_opts(None))?;

    // File-based info — the reference output.
    let from_file = Command::new(rz_archive_bin())
        .args(["info", archive.as_str(), "--json"])
        .output()?;
    assert!(from_file.status.success(), "file info failed");

    // Stdin-based info — should produce byte-identical JSON, since stdin's
    // tallied compressed size equals the file's on-disk size.
    let archive_bytes = fs_err::read(&archive)?;
    let from_stdin = run_with_stdin(&["info", "-", "--format", "tar-gz", "--json"], &archive_bytes)?;
    assert!(
        from_stdin.status.success(),
        "stdin info failed: {}",
        String::from_utf8_lossy(&from_stdin.stderr),
    );

    assert_eq!(
        String::from_utf8_lossy(&from_stdin.stdout),
        String::from_utf8_lossy(&from_file.stdout),
        "stdin info should match file info",
    );
    Ok(())
}

#[test]
fn info_from_stdin_requires_format() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;
    let archive = tmp.join("payload.tar.gz");
    (TAR_GZ.compress)(&[tree], &archive, &default_compress_opts(None))?;
    let archive_bytes = fs_err::read(&archive)?;

    // No --format: there's no filename to infer from, so this must fail before
    // any reading.
    let out = run_with_stdin(&["info", "-"], &archive_bytes)?;
    assert!(!out.status.success(), "expected failure without --format");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot infer format from stdin"),
        "stderr should explain the missing format: {stderr}",
    );
    Ok(())
}

#[test]
fn info_from_stdin_rejects_zip() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;
    let archive = tmp.join("bundle.zip");
    (ZIP.compress)(&[tree], &archive, &default_compress_opts(None))?;
    let archive_bytes = fs_err::read(&archive)?;

    // zip needs a seekable central directory; a pipe can't provide it.
    let out = run_with_stdin(&["info", "-", "--format", "zip"], &archive_bytes)?;
    assert!(!out.status.success(), "expected zip stdin rejection");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does not support reading from stdin"),
        "stderr should explain the seekable requirement: {stderr}",
    );
    Ok(())
}
