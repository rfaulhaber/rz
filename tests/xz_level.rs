//! `--level` range validation for the xz backend.
//!
//! With the `xz2` feature, liblzma rejects presets above 9 and the crate
//! unwraps that into a panic (exit 101); the pure-Rust backend silently
//! clamped instead.  Both backends must now reject the flag cleanly, so the
//! observable behaviour is identical whichever backend is compiled in.

use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rz_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz")
}

fn temp_dir() -> Result<(tempfile::TempDir, camino::Utf8PathBuf), Box<dyn std::error::Error>> {
    let guard = tempfile::tempdir()?;
    let path = camino::Utf8PathBuf::try_from(guard.path().to_path_buf())
        .map_err(|e| format!("non-UTF-8 tempdir: {e}"))?;
    Ok((guard, path))
}

#[test]
fn xz_level_above_nine_errors_cleanly() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let payload = tmp.join("a.txt");
    fs_err::write(&payload, "hello")?;
    let out = Command::new(rz_bin())
        .args([
            "compress",
            payload.as_str(),
            "-f",
            "tar-xz",
            "--level",
            "10",
            "-o",
        ])
        .arg(tmp.join("out.tar.xz").as_str())
        .output()?;
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected clean rejection, got {:?} (a 101 means the liblzma unwrap panicked)",
        out.status.code(),
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("xz compression level"),
    );
    Ok(())
}

#[test]
fn xz_level_nine_still_works() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let payload = tmp.join("a.txt");
    fs_err::write(&payload, "hello")?;
    let archive = tmp.join("out.tar.xz");
    let out = Command::new(rz_bin())
        .args([
            "compress",
            payload.as_str(),
            "--level",
            "9",
            "-o",
            archive.as_str(),
        ])
        .output()?;
    assert!(
        out.status.success(),
        "level 9 must remain valid: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let list = Command::new(rz_bin()).arg("list").arg(archive.as_str()).output()?;
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains("a.txt"));
    Ok(())
}

#[test]
fn xz_append_level_above_nine_errors_cleanly() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let payload = tmp.join("a.txt");
    fs_err::write(&payload, "hello")?;
    let archive = tmp.join("out.tar.xz");
    let ok = Command::new(rz_bin())
        .args(["compress", payload.as_str(), "-o", archive.as_str()])
        .output()?;
    assert!(ok.status.success());

    let extra = tmp.join("b.txt");
    fs_err::write(&extra, "more")?;
    let out = Command::new(rz_bin())
        .args([
            "append",
            archive.as_str(),
            extra.as_str(),
            "--level",
            "11",
        ])
        .output()?;
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("xz compression level"),
    );
    Ok(())
}
