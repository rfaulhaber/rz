//! Extraction semantics around imperfect runs and imperfect archives: the
//! deferred-directory flush on abort, tar's last-wins rule for duplicate
//! names, `.`-rooted archives, and `--keep-newer` against a dangling symlink.

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

fn run(cwd: &Utf8PathBuf, args: &[&str]) -> Result<std::process::Output, Box<dyn std::error::Error>> {
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
    Ok(out)
}

/// Duplicate names are routine in appended-to archives; later entries replace
/// earlier ones from the same run (tar's last-wins) without tripping the
/// overwrite guard, which protects only files that predate the extraction.
#[test]
fn duplicate_entries_extract_last_wins_without_force() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let mut builder = tar::Builder::new(Vec::new());
    for body in [b"one".as_slice(), b"two".as_slice()] {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(1_000_000);
        builder.append_data(&mut h, "x.txt", body)?;
    }
    fs_err::write(tmp.join("dup.tar"), builder.into_inner()?)?;

    let out_dir = tmp.join("out");
    run(&tmp, &["decompress", "dup.tar", "-o", out_dir.as_str()])?;
    assert_eq!(fs_err::read_to_string(out_dir.join("x.txt"))?, "two");
    Ok(())
}

/// An archive built from `.` carries a literal `.` root entry; extraction
/// must map it to the output directory itself instead of failing to create
/// `out/.` when `out` does not exist yet.
#[test]
fn dot_rooted_archive_extracts_into_fresh_directory() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let src = tmp.join("src");
    fs_err::create_dir_all(src.join("sub"))?;
    fs_err::write(src.join("f.txt"), "hi")?;
    fs_err::write(src.join("sub/g.txt"), "there")?;
    run(&src, &["compress", ".", "-o", "../dot.tar.gz"])?;

    let out_dir = tmp.join("out");
    run(&tmp, &["decompress", "dot.tar.gz", "-o", out_dir.as_str()])?;
    assert_eq!(fs_err::read_to_string(out_dir.join("f.txt"))?, "hi");
    assert_eq!(fs_err::read_to_string(out_dir.join("sub/g.txt"))?, "there");
    Ok(())
}

/// When extraction aborts mid-run, directories that already received their
/// contents must still get their recorded modes — a 0700 directory holding a
/// 0600 secret must not stay at create_dir_all's permissive default.
#[cfg(unix)]
#[test]
fn aborted_extraction_still_applies_deferred_directory_modes() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let (_guard, tmp) = temp_dir()?;
    let src = tmp.join("sec");
    fs_err::create_dir_all(src.join("ssh"))?;
    fs_err::write(src.join("ssh/key"), "SECRET")?;
    fs_err::set_permissions(src.join("ssh/key"), std::fs::Permissions::from_mode(0o600))?;
    fs_err::set_permissions(src.join("ssh"), std::fs::Permissions::from_mode(0o700))?;
    // A conflicting file sorted after ssh/ makes the run abort once the
    // secrets are already on disk.
    fs_err::write(src.join("zzz.txt"), "conflict")?;
    run(&tmp, &["compress", "sec", "-o", "sec.tar"])?;

    let out_dir = tmp.join("out");
    fs_err::create_dir_all(out_dir.join("sec"))?;
    fs_err::write(out_dir.join("sec/zzz.txt"), "pre-existing")?;

    let out = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["decompress", "sec.tar", "-o", out_dir.as_str(), "-p"])
        .output()?;
    assert!(!out.status.success(), "the conflict must abort the run");

    let mode = fs_err::metadata(out_dir.join("sec/ssh"))?.permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "aborted run left the directory at mode {mode:o}");
    Ok(())
}

/// `--keep-newer` stats the destination through symlinks; a dangling symlink
/// used to abort the whole extraction with NotFound instead of being
/// replaced.
#[cfg(unix)]
#[test]
fn keep_newer_replaces_dangling_symlink_destination() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let src = tmp.join("kn");
    fs_err::create_dir_all(&src)?;
    fs_err::write(src.join("file.txt"), "fresh")?;
    run(&tmp, &["compress", "kn", "-o", "kn.tar"])?;

    let out_dir = tmp.join("out");
    fs_err::create_dir_all(out_dir.join("kn"))?;
    std::os::unix::fs::symlink("/nonexistent", out_dir.join("kn/file.txt").as_std_path())?;

    run(
        &tmp,
        &["decompress", "kn.tar", "-o", out_dir.as_str(), "--keep-newer"],
    )?;
    assert_eq!(fs_err::read_to_string(out_dir.join("kn/file.txt"))?, "fresh");
    Ok(())
}
