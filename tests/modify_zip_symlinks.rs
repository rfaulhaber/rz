//! zip append/update must store symlinks as symlink entries, like compress
//! does — the rewrite path used to `File::open` every planned entry, silently
//! dereferencing a link into a regular file holding the target's content.
#![cfg(unix)]

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

fn assert_extracted_symlink(link: &Utf8PathBuf, target: &str) -> TestResult {
    let meta = fs_err::symlink_metadata(link)?;
    assert!(
        meta.file_type().is_symlink(),
        "{link} must extract as a symlink, not a regular file",
    );
    assert_eq!(fs_err::read_link(link)?.to_string_lossy(), target);
    Ok(())
}

#[test]
fn append_stores_toplevel_symlink_as_symlink() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    fs_err::write(tmp.join("a.txt"), "target content")?;
    run(&tmp, &["compress", "a.txt", "-o", "arc.zip"])?;

    std::os::unix::fs::symlink("a.txt", tmp.join("link"))?;
    run(&tmp, &["append", "arc.zip", "link"])?;

    run(&tmp, &["decompress", "arc.zip", "-o", "out", "-F"])?;
    assert_extracted_symlink(&tmp.join("out/link"), "a.txt")?;
    Ok(())
}

#[test]
fn append_stores_walked_symlink_as_symlink() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    fs_err::write(tmp.join("seed.txt"), "seed")?;
    run(&tmp, &["compress", "seed.txt", "-o", "arc.zip"])?;

    let tree = tmp.join("tree");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("real.txt"), "real")?;
    std::os::unix::fs::symlink("real.txt", tree.join("link"))?;
    run(&tmp, &["append", "arc.zip", "tree"])?;

    run(&tmp, &["decompress", "arc.zip", "-o", "out", "-F"])?;
    assert_extracted_symlink(&tmp.join("out/tree/link"), "real.txt")?;
    assert_eq!(fs_err::read_to_string(tmp.join("out/tree/real.txt"))?, "real");
    Ok(())
}

#[test]
fn update_keeps_symlink_a_symlink() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("tree");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("real.txt"), "real")?;
    std::os::unix::fs::symlink("real.txt", tree.join("link"))?;

    // Backdate everything so the compress stores old mtimes...
    let old = filetime::FileTime::from_unix_time(1_000_000_000, 0);
    filetime::set_file_times(tree.join("real.txt").as_std_path(), old, old)?;
    filetime::set_symlink_file_times(tree.join("link").as_std_path(), old, old)?;
    run(&tmp, &["compress", "tree", "-o", "arc.zip"])?;

    // ...then freshen the link so update rewrites it.
    let newer = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    filetime::set_symlink_file_times(tree.join("link").as_std_path(), newer, newer)?;
    let before = fs_err::read(tmp.join("arc.zip"))?;
    run(&tmp, &["update", "arc.zip", "tree"])?;
    assert_ne!(
        fs_err::read(tmp.join("arc.zip"))?,
        before,
        "the freshened link must actually be rewritten — a no-op update \
         would only re-test the compress-side output",
    );

    run(&tmp, &["decompress", "arc.zip", "-o", "out", "-F"])?;
    assert_extracted_symlink(&tmp.join("out/tree/link"), "real.txt")?;
    Ok(())
}

/// The carry side of the rewrite: entries the plan does *not* touch must
/// survive appends and removes intact.  `raw_copy_file` flattens symlink
/// entries into regular files holding the target path (its rebuilt options
/// mask the mode to 0o777 and re-OR S_IFREG), so untouched kinds are
/// re-written through the typed APIs instead.
#[test]
fn append_and_remove_preserve_existing_symlink_entries() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("tree");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("real.txt"), "real")?;
    std::os::unix::fs::symlink("real.txt", tree.join("link"))?;
    run(&tmp, &["compress", "tree", "-o", "arc.zip"])?;

    fs_err::write(tmp.join("extra.txt"), "extra")?;
    run(&tmp, &["append", "arc.zip", "extra.txt"])?;
    run(&tmp, &["decompress", "arc.zip", "-o", "out1", "-F"])?;
    assert_extracted_symlink(&tmp.join("out1/tree/link"), "real.txt")?;

    run(&tmp, &["remove", "arc.zip", "extra.txt"])?;
    run(&tmp, &["decompress", "arc.zip", "-o", "out2", "-F"])?;
    assert_extracted_symlink(&tmp.join("out2/tree/link"), "real.txt")?;
    Ok(())
}

/// Carried-over directory entries must keep S_IFDIR in their mode word;
/// the raw copy used to relabel them as regular files.
#[test]
fn append_preserves_directory_entry_modes() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("tree");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("real.txt"), "real")?;
    run(&tmp, &["compress", "tree", "-o", "arc.zip"])?;

    fs_err::write(tmp.join("extra.txt"), "extra")?;
    run(&tmp, &["append", "arc.zip", "extra.txt"])?;

    let out = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["list", "arc.zip", "--json"])
        .output()?;
    assert!(out.status.success());
    let entries: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let dir = entries
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|e| e["path"].as_str().map(|p| p.trim_end_matches('/')) == Some("tree"))
        })
        .ok_or("no tree entry in listing")?;
    let mode = dir["mode"].as_u64().ok_or("no mode")?;
    assert_eq!(
        mode & 0o170000,
        0o040000,
        "directory entry lost S_IFDIR: mode {mode:o}",
    );
    Ok(())
}

#[test]
fn append_with_follow_symlinks_still_dereferences() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    fs_err::write(tmp.join("a.txt"), "target content")?;
    run(&tmp, &["compress", "a.txt", "-o", "arc.zip"])?;

    std::os::unix::fs::symlink("a.txt", tmp.join("link"))?;
    run(&tmp, &["append", "arc.zip", "link", "-H"])?;

    run(&tmp, &["decompress", "arc.zip", "-o", "out", "-F"])?;
    let meta = fs_err::symlink_metadata(tmp.join("out/link"))?;
    assert!(
        meta.file_type().is_file(),
        "-H must archive the target's content as a regular file",
    );
    assert_eq!(
        fs_err::read_to_string(tmp.join("out/link"))?,
        "target content",
    );
    Ok(())
}
