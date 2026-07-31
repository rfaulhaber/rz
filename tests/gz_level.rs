//! `--level` range validation for the gzip backend — the same defect class
//! the xz fix covered: flate2's `Compression::new` asserts `level <= 9`, so
//! an unvalidated `--level 10` panicked (exit 101) and left a stub archive
//! (plus a leaked temp file on the modify paths).

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
fn gz_level_above_nine_errors_cleanly() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let payload = tmp.join("a.txt");
    fs_err::write(&payload, "hello")?;
    let archive = tmp.join("out.tar.gz");
    let out = Command::new(rz_bin())
        .args([
            "compress",
            payload.as_str(),
            "--level",
            "10",
            "-o",
            archive.as_str(),
        ])
        .output()?;
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected clean rejection, got {:?} (a 101 means the flate2 assert fired)",
        out.status.code(),
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("gzip compression level"));
    assert!(
        fs_err::metadata(&archive).is_err(),
        "a rejected run must not leave a stub archive",
    );
    Ok(())
}

#[test]
fn gz_levels_zero_and_nine_still_work() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let payload = tmp.join("a.txt");
    fs_err::write(&payload, "hello")?;
    for level in ["0", "9"] {
        let archive = tmp.join(format!("out-{level}.tar.gz"));
        let out = Command::new(rz_bin())
            .args([
                "compress",
                payload.as_str(),
                "--level",
                level,
                "-o",
                archive.as_str(),
            ])
            .output()?;
        assert!(
            out.status.success(),
            "level {level} must remain valid: {}",
            String::from_utf8_lossy(&out.stderr),
        );
        let list = Command::new(rz_bin()).arg("list").arg(archive.as_str()).output()?;
        assert!(list.status.success());
        assert!(String::from_utf8_lossy(&list.stdout).contains("a.txt"));
    }
    Ok(())
}

#[test]
fn gz_append_level_above_nine_errors_cleanly_and_leaks_no_temp() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let payload = tmp.join("a.txt");
    fs_err::write(&payload, "hello")?;
    let archive = tmp.join("out.tar.gz");
    let ok = Command::new(rz_bin())
        .args(["compress", payload.as_str(), "-o", archive.as_str()])
        .output()?;
    assert!(ok.status.success());
    let before = fs_err::read(&archive)?;

    let extra = tmp.join("b.txt");
    fs_err::write(&extra, "more")?;
    let out = Command::new(rz_bin())
        .args(["append", archive.as_str(), extra.as_str(), "--level", "11"])
        .output()?;
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("gzip compression level"));

    assert_eq!(fs_err::read(&archive)?, before, "the archive must be untouched");
    let leftovers: Vec<_> = fs_err::read_dir(&tmp)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("rzappend"))
        .collect();
    assert!(leftovers.is_empty(), "temp files leaked: {leftovers:?}");
    Ok(())
}
