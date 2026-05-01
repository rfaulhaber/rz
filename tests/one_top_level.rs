//! End-to-end coverage for `decompress --one-top-level`.
//!
//! The directory-derivation logic itself is unit-tested in `format.rs`; this
//! suite shells out to the compiled `rz` binary to verify the wiring in
//! `main.rs` — flag conflicts, the create-directory side effect, and the
//! stdin rejection path.

mod helpers;

use std::process::Command;

use helpers::{TAR_GZ, TestResult, ZIP, build_file_tree, default_compress_opts, temp_utf8_dir};

/// Path to the `rz` binary that Cargo just built for this test crate.
fn rz_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz")
}

#[test]
fn tar_gz_extracts_into_derived_directory() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    // Name the archive deliberately so `derive_output_dir` produces "payload".
    let archive = tmp.join("payload.tar.gz");
    (TAR_GZ.compress)(&[tree], &archive, &default_compress_opts(None))?;

    let status = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["decompress", archive.as_str(), "--one-top-level"])
        .status()?;
    assert!(status.success(), "rz exited with {status}");

    let derived = tmp.join("payload");
    assert!(derived.is_dir(), "expected derived directory {derived}");
    // The original "tree" wrapper from the archive ends up inside "payload/"
    // because we don't re-strip on extract.
    assert!(derived.join("tree/hello.txt").exists());
    assert!(derived.join("tree/subdir/nested.txt").exists());
    Ok(())
}

#[test]
fn zip_extracts_into_derived_directory() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let archive = tmp.join("bundle.zip");
    (ZIP.compress)(&[tree], &archive, &default_compress_opts(None))?;

    let status = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["decompress", archive.as_str(), "--one-top-level"])
        .status()?;
    assert!(status.success(), "rz exited with {status}");

    let derived = tmp.join("bundle");
    assert!(derived.is_dir(), "expected derived directory {derived}");
    assert!(derived.join("tree/hello.txt").exists());
    Ok(())
}

#[test]
fn one_top_level_rejects_stdin_input() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    // Build a real tar.gz so stdin has valid bytes to feed in.
    let tree = tmp.join("tree");
    build_file_tree(&tree)?;
    let archive = tmp.join("payload.tar.gz");
    (TAR_GZ.compress)(&[tree], &archive, &default_compress_opts(None))?;

    let archive_bytes = fs_err::read(&archive)?;
    let mut child = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["decompress", "-", "--format", "tar-gz", "--one-top-level"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().ok_or("no stdin")?;
        stdin.write_all(&archive_bytes)?;
    }
    let out = child.wait_with_output()?;
    assert!(!out.status.success(), "expected failure, got {}", out.status);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--one-top-level"),
        "stderr should mention the offending flag: {stderr}",
    );
    Ok(())
}

#[test]
fn one_top_level_conflicts_with_output_flag() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    // Doesn't need to exist — clap rejects argument combinations before any
    // file I/O happens.
    let archive = tmp.join("payload.tar.gz");
    let other = tmp.join("elsewhere");

    let out = Command::new(rz_bin())
        .args([
            "decompress",
            archive.as_str(),
            "--one-top-level",
            "-o",
            other.as_str(),
        ])
        .output()?;
    assert!(!out.status.success(), "expected clap to reject combination");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--one-top-level") && stderr.contains("--output"),
        "stderr should call out the conflict: {stderr}",
    );
    Ok(())
}
