//! 7z entry metadata and naming after the explicit-walk rework: tar/zip-style
//! prefixed names (no more tar-bombing or cross-input collisions), Unix modes
//! stored p7zip-style in the attribute word and honoured under -P, symlinks
//! stored as symlink entries, and listings that carry real mtimes/modes.

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

#[test]
fn multiple_inputs_with_equal_children_no_longer_collide() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    for (dir, content) in [("a", "from a"), ("b", "from b")] {
        let d = tmp.join(dir);
        fs_err::create_dir_all(&d)?;
        fs_err::write(d.join("x.txt"), content)?;
    }
    run(&tmp, &["compress", "a", "b", "-o", "both.7z"])?;

    let list = run(&tmp, &["list", "both.7z"])?;
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.lines().any(|l| l == "a/x.txt"), "listing: {stdout}");
    assert!(stdout.lines().any(|l| l == "b/x.txt"), "listing: {stdout}");

    run(&tmp, &["decompress", "both.7z", "-o", "out"])?;
    assert_eq!(fs_err::read_to_string(tmp.join("out/a/x.txt"))?, "from a");
    assert_eq!(fs_err::read_to_string(tmp.join("out/b/x.txt"))?, "from b");
    Ok(())
}

#[cfg(unix)]
#[test]
fn exec_bit_round_trips_under_preserve_permissions() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("src");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("run.sh"), "#!/bin/sh\n")?;
    fs_err::set_permissions(tree.join("run.sh"), std::fs::Permissions::from_mode(0o755))?;
    fs_err::write(tree.join("data.txt"), "data")?;
    fs_err::set_permissions(tree.join("data.txt"), std::fs::Permissions::from_mode(0o600))?;

    run(&tmp, &["compress", "src", "-o", "a.7z"])?;
    run(&tmp, &["decompress", "a.7z", "-o", "out", "-P"])?;

    let mode = |p: &str| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(fs_err::metadata(tmp.join(p))?.permissions().mode() & 0o7777)
    };
    assert_eq!(mode("out/src/run.sh")?, 0o755, "exec bit lost through 7z");
    assert_eq!(mode("out/src/data.txt")?, 0o600);
    Ok(())
}

#[cfg(unix)]
#[test]
fn readonly_dir_mode_is_restored_after_children() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("src");
    fs_err::create_dir_all(tree.join("ro"))?;
    fs_err::write(tree.join("ro/inner.txt"), "inner")?;
    fs_err::set_permissions(tree.join("ro"), std::fs::Permissions::from_mode(0o555))?;

    let compress = run(&tmp, &["compress", "src", "-o", "a.7z"]);
    // Restore writability so the tempdir can clean up regardless of outcome.
    fs_err::set_permissions(tree.join("ro"), std::fs::Permissions::from_mode(0o755))?;
    compress?;

    run(&tmp, &["decompress", "a.7z", "-o", "out", "-P"])?;
    let extracted = tmp.join("out/src/ro");
    let mode = fs_err::metadata(&extracted)?.permissions().mode() & 0o7777;
    let inner_ok = tmp.join("out/src/ro/inner.txt").as_std_path().is_file();
    let _ = fs_err::set_permissions(&extracted, std::fs::Permissions::from_mode(0o755));

    assert!(inner_ok, "children of the read-only dir must extract");
    assert_eq!(mode, 0o555, "-P must restore the directory mode afterwards");
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinks_round_trip_including_dangling() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("src");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("real.txt"), "real")?;
    std::os::unix::fs::symlink("real.txt", tree.join("link"))?;
    std::os::unix::fs::symlink("nowhere", tree.join("dangling"))?;

    run(&tmp, &["compress", "src", "-o", "a.7z"])?;
    run(&tmp, &["decompress", "a.7z", "-o", "out"])?;

    let link = tmp.join("out/src/link");
    assert!(fs_err::symlink_metadata(&link)?.file_type().is_symlink());
    assert_eq!(fs_err::read_link(&link)?.to_string_lossy(), "real.txt");

    let dangling = tmp.join("out/src/dangling");
    assert!(fs_err::symlink_metadata(&dangling)?.file_type().is_symlink());
    assert_eq!(fs_err::read_link(&dangling)?.to_string_lossy(), "nowhere");
    Ok(())
}

#[cfg(unix)]
#[test]
fn follow_symlinks_archives_target_content() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("src");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("real.txt"), "the content")?;
    std::os::unix::fs::symlink("real.txt", tree.join("link"))?;

    run(&tmp, &["compress", "src", "-o", "a.7z", "-H"])?;
    run(&tmp, &["decompress", "a.7z", "-o", "out"])?;

    let link = tmp.join("out/src/link");
    assert!(
        fs_err::symlink_metadata(&link)?.file_type().is_file(),
        "-H must dereference the link into a regular file",
    );
    assert_eq!(fs_err::read_to_string(&link)?, "the content");
    Ok(())
}

#[test]
fn listing_reports_mtime_and_mode() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("src");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("a.txt"), "alpha")?;
    let t = filetime::FileTime::from_unix_time(1_500_000_000, 0);
    filetime::set_file_times(tree.join("a.txt").as_std_path(), t, t)?;

    run(&tmp, &["compress", "src", "-o", "a.7z"])?;
    let out = run(&tmp, &["list", "--json", "a.7z"])?;
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout)?;
    let file_row = rows
        .iter()
        .find(|r| r["path"] == "src/a.txt")
        .ok_or("src/a.txt missing from listing")?;
    assert_eq!(
        file_row["mtime"].as_u64(),
        Some(1_500_000_000),
        "7z listings must carry the stored mtime",
    );
    #[cfg(unix)]
    {
        let mode = file_row["mode"].as_u64().unwrap_or(0);
        assert_eq!(mode & 0o170000, 0o100000, "mode must carry S_IFREG");
    }
    Ok(())
}
