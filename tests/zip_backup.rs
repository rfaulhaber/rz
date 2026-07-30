//! `decompress --backup` for zip.  The extraction path stat'd the destination
//! once, renamed the original aside, and then acted on the stale "existed"
//! answer — aborting the whole run with NotFound right after moving the
//! user's file out of the way.

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

fn run(cwd: &Utf8PathBuf, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let out = Command::new(rz_bin())
        .current_dir(cwd.as_std_path())
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "rz {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(())
}

#[test]
fn backup_moves_existing_file_aside_and_extracts() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    fs_err::write(tmp.join("a.txt"), "new content")?;
    run(&tmp, &["compress", "a.txt", "-o", "a.zip"])?;

    let out_dir = tmp.join("out");
    fs_err::create_dir_all(&out_dir)?;
    fs_err::write(out_dir.join("a.txt"), "PRECIOUS")?;

    run(&tmp, &["decompress", "a.zip", "-o", "out", "--backup"])?;

    assert_eq!(fs_err::read_to_string(out_dir.join("a.txt.bak"))?, "PRECIOUS");
    assert_eq!(fs_err::read_to_string(out_dir.join("a.txt"))?, "new content");
    Ok(())
}

#[cfg(unix)]
#[test]
fn backup_of_a_symlink_entry_over_an_existing_file() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    fs_err::write(tmp.join("target.txt"), "the target")?;
    std::os::unix::fs::symlink("target.txt", tmp.join("link"))?;
    run(&tmp, &["compress", "link", "target.txt", "-o", "a.zip"])?;

    let out_dir = tmp.join("out");
    fs_err::create_dir_all(&out_dir)?;
    fs_err::write(out_dir.join("link"), "PRECIOUS")?;

    run(&tmp, &["decompress", "a.zip", "-o", "out", "--backup"])?;

    assert_eq!(fs_err::read_to_string(out_dir.join("link.bak"))?, "PRECIOUS");
    let meta = fs_err::symlink_metadata(out_dir.join("link"))?;
    assert!(meta.file_type().is_symlink(), "entry must extract as a symlink");
    Ok(())
}
