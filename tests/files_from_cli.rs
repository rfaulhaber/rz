//! `compress --files-from` through the real binary.
//!
//! An empty or comment-only list used to reach `fmt.default_output(&input[0])`
//! with no inputs and panic with an index-out-of-bounds (exit 101); the
//! dry-run branch silently printed nothing and exited 0.  Both must instead
//! report `NoReadableInputs`.

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

/// Run compress with the given list-file content and return the output.
fn compress_with_list(
    list_content: &str,
    extra_args: &[&str],
) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let (_guard, tmp) = temp_dir()?;
    let list = tmp.join("list.txt");
    fs_err::write(&list, list_content)?;
    let out = Command::new(rz_bin())
        .args(["compress", "-T", list.as_str(), "-f", "tar"])
        .args(extra_args)
        .arg("-o")
        .arg(tmp.join("out.tar").as_str())
        .output()?;
    Ok(out)
}

#[test]
fn empty_files_from_errors_instead_of_panicking() -> TestResult {
    let out = compress_with_list("", &[])?;
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected clean error, got {:?} (stderr: {})",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no readable inputs"),
        "unexpected stderr: {stderr}",
    );
    Ok(())
}

#[test]
fn comment_only_files_from_errors() -> TestResult {
    let out = compress_with_list("# nothing here\n\n# still nothing\n", &[])?;
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no readable inputs"),
    );
    Ok(())
}

#[test]
fn empty_files_from_dry_run_errors_instead_of_printing_nothing() -> TestResult {
    let out = compress_with_list("", &["--dry-run"])?;
    assert_eq!(
        out.status.code(),
        Some(1),
        "dry-run with an empty list must error, not exit 0",
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no readable inputs"),
    );
    Ok(())
}

#[test]
fn files_from_with_real_paths_still_compresses() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let payload = tmp.join("a.txt");
    fs_err::write(&payload, "hello")?;
    let list = tmp.join("list.txt");
    fs_err::write(&list, format!("# payload\n{payload}\n"))?;
    let archive = tmp.join("out.tar");
    let out = Command::new(rz_bin())
        .args(["compress", "-T", list.as_str(), "-o", archive.as_str()])
        .output()?;
    assert!(
        out.status.success(),
        "compress via files-from failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(archive.as_std_path().exists());
    Ok(())
}
