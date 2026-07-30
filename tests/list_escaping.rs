//! Untrusted entry names must reach the terminal defanged.  `rz list` is the
//! natural "inspect before you extract" step, and a raw ESC byte in a name
//! lets CSI sequences erase or overwrite listing lines, hiding entries.

use std::process::Command;

use camino::Utf8PathBuf;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rz_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz")
}

fn temp_dir() -> Result<(tempfile::TempDir, Utf8PathBuf), Box<dyn std::error::Error>> {
    let guard = tempfile::tempdir()?;
    let path = Utf8PathBuf::try_from(guard.path().to_path_buf())
        .map_err(|e| format!("non-UTF-8 tempdir: {e}"))?;
    Ok((guard, path))
}

const EVIL_NAME: &str = "boring.txt\x1b[1;31m EVERYTHING IS FINE\x1b[0m";

fn hostile_tar(tmp: &Utf8PathBuf) -> Result<Utf8PathBuf, Box<dyn std::error::Error>> {
    let mut builder = tar::Builder::new(Vec::new());
    let content = b"payload";
    let mut h = tar::Header::new_gnu();
    h.set_size(content.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(0);
    builder.append_data(&mut h, EVIL_NAME, content.as_slice())?;
    let archive = tmp.join("evil.tar");
    fs_err::write(&archive, builder.into_inner()?)?;
    Ok(archive)
}

#[test]
fn list_escapes_esc_bytes() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = hostile_tar(&tmp)?;

    for args in [vec!["list"], vec!["list", "-l"]] {
        let out = Command::new(rz_bin())
            .args(&args)
            .arg(archive.as_str())
            .output()?;
        assert!(out.status.success());
        assert!(
            !out.stdout.contains(&0x1b),
            "{args:?}: raw ESC byte reached stdout",
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("\\x1b[1;31m"),
            "{args:?}: expected the escaped form, got: {stdout}",
        );
    }
    Ok(())
}

#[test]
fn dry_run_escapes_esc_bytes() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = hostile_tar(&tmp)?;

    let out = Command::new(rz_bin())
        .args(["decompress", archive.as_str(), "-n"])
        .output()?;
    assert!(out.status.success());
    assert!(!out.stdout.contains(&0x1b), "raw ESC byte reached stdout");
    Ok(())
}

#[test]
fn verbose_extraction_escapes_esc_bytes() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = hostile_tar(&tmp)?;

    let out = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["-v", "decompress", archive.as_str(), "-o", "out", "-F"])
        .output()?;
    assert!(
        out.status.success(),
        "verbose extraction failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(!out.stderr.contains(&0x1b), "raw ESC byte reached stderr");
    Ok(())
}

#[test]
fn json_output_is_untouched_and_safe() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = hostile_tar(&tmp)?;

    let out = Command::new(rz_bin())
        .args(["list", "--json", archive.as_str()])
        .output()?;
    assert!(out.status.success());
    // serde_json escapes control characters itself, so the raw byte must be
    // absent while the decoded value round-trips to the original name.
    assert!(!out.stdout.contains(&0x1b));
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout)?;
    let path = rows.first().and_then(|r| r["path"].as_str());
    assert_eq!(path, Some(EVIL_NAME));
    Ok(())
}
