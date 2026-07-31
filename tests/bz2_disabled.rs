//! Without the `bzip2` feature, tar-bz2 is still a valid clap value and is
//! still magic-byte detected — but every dispatch arm is compiled out.  The
//! catch-alls used to blame stdin/stdout seekability ("does not support
//! reading from stdin"), which is false: tar-bz2 streams fine when compiled
//! in.  Every path must instead say the feature is disabled.
#![cfg(not(feature = "bzip2"))]

use std::io::Write;
use std::process::{Command, Stdio};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rz_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz")
}

const DISABLED_MSG: &str = "not compiled into this build";

/// A few bytes carrying the bzip2 magic — enough for format detection, which
/// is all that should run before the feature check fires.
const BZ2_MAGIC: &[u8] = b"BZh91AY&SY\x00\x00\x00\x00";

fn assert_reports_disabled(out: &std::process::Output, ctx: &str) {
    assert_eq!(out.status.code(), Some(1), "{ctx}: expected exit 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(DISABLED_MSG),
        "{ctx}: stderr should report the disabled feature, got: {stderr}",
    );
}

#[test]
fn stdin_list_reports_disabled_feature_not_seekability() -> TestResult {
    let mut child = Command::new(rz_bin())
        .arg("list")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .ok_or("no piped stdin")?
        .write_all(BZ2_MAGIC)?;
    let out = child.wait_with_output()?;
    assert_reports_disabled(&out, "stdin list");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("stdin"),
        "must not blame stdin seekability",
    );
    Ok(())
}

#[test]
fn file_list_reports_disabled_feature() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let path = tmp.path().join("a.tar.bz2");
    fs_err::write(&path, BZ2_MAGIC)?;
    let out = Command::new(rz_bin())
        .arg("list")
        .arg(&path)
        .output()?;
    assert_reports_disabled(&out, "file list");
    Ok(())
}

#[test]
fn compress_reports_disabled_feature() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let payload = tmp.path().join("x.txt");
    fs_err::write(&payload, "hi")?;
    let out_path = tmp.path().join("out.tar.bz2");
    let out = Command::new(rz_bin())
        .arg("compress")
        .arg(&payload)
        .arg("-o")
        .arg(&out_path)
        .output()?;
    assert_reports_disabled(&out, "compress");
    assert!(!out_path.exists(), "no stub archive may be left behind");
    Ok(())
}

/// The preview must fail the same way the real run would — `compress -n`
/// used to return before format resolution and exit 0.
#[test]
fn compress_dry_run_reports_disabled_feature() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let payload = tmp.path().join("x.txt");
    fs_err::write(&payload, "hi")?;
    let out_path = tmp.path().join("a.tar.bz2");
    let out = Command::new(rz_bin())
        .arg("compress")
        .arg("-n")
        .arg(&payload)
        .arg("-o")
        .arg(&out_path)
        .output()?;
    assert_reports_disabled(&out, "compress -n");
    Ok(())
}

#[test]
fn append_reports_disabled_feature() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let archive = tmp.path().join("a.tar.bz2");
    fs_err::write(&archive, BZ2_MAGIC)?;
    let payload = tmp.path().join("x.txt");
    fs_err::write(&payload, "hi")?;
    let out = Command::new(rz_bin())
        .arg("append")
        .arg(&archive)
        .arg(&payload)
        .output()?;
    assert_reports_disabled(&out, "append");
    Ok(())
}
