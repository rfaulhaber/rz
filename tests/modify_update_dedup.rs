//! Compressed-tar `update` is a full read-rewrite, so replacing an entry must
//! drop the stale copy instead of carrying both: the keep-all copy pass used
//! to double `src` and `src/a.txt` on every update, growing backup archives
//! without bound.

use std::collections::HashMap;
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

/// Name → occurrence count from `rz list`.
fn name_counts(
    cwd: &Utf8PathBuf,
    archive: &str,
) -> Result<HashMap<String, usize>, Box<dyn std::error::Error>> {
    let out = Command::new(rz_bin())
        .current_dir(cwd.as_std_path())
        .args(["list", archive])
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    let mut counts = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        *counts
            .entry(line.trim_end_matches('/').to_owned())
            .or_insert(0) += 1;
    }
    Ok(counts)
}

fn set_mtime(path: &Utf8PathBuf, secs: i64) -> TestResult {
    let t = filetime::FileTime::from_unix_time(secs, 0);
    filetime::set_file_times(path.as_std_path(), t, t)?;
    Ok(())
}

#[test]
fn update_replaces_instead_of_duplicating() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let src = tmp.join("src");
    fs_err::create_dir_all(&src)?;
    fs_err::write(src.join("a.txt"), "v1")?;
    set_mtime(&src.join("a.txt"), 1_000_000_000)?;
    set_mtime(&src, 1_000_000_000)?;

    run(&tmp, &["compress", "src", "-o", "a.tar.gz"])?;

    // Newer content → update must rewrite exactly one copy.
    fs_err::write(src.join("a.txt"), "v2")?;
    set_mtime(&src.join("a.txt"), 1_100_000_000)?;
    run(&tmp, &["update", "a.tar.gz", "src"])?;

    let counts = name_counts(&tmp, "a.tar.gz")?;
    assert_eq!(counts.get("src/a.txt"), Some(&1), "counts: {counts:?}");
    assert_eq!(counts.get("src"), Some(&1), "dir entry duplicated: {counts:?}");

    // Extraction yields the new content.
    let out_dir = tmp.join("out");
    run(
        &tmp,
        &["decompress", "a.tar.gz", "-o", out_dir.as_str(), "-F"],
    )?;
    assert_eq!(fs_err::read_to_string(out_dir.join("src/a.txt"))?, "v2");
    Ok(())
}

#[test]
fn repeated_updates_do_not_grow_the_archive() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let src = tmp.join("src");
    fs_err::create_dir_all(&src)?;
    fs_err::write(src.join("a.txt"), "v1")?;
    set_mtime(&src.join("a.txt"), 1_000_000_000)?;
    set_mtime(&src, 1_000_000_000)?;
    run(&tmp, &["compress", "src", "-o", "a.tar.gz"])?;

    for round in 1..=3u32 {
        fs_err::write(src.join("a.txt"), format!("v{round}"))?;
        set_mtime(&src.join("a.txt"), 1_000_000_000 + i64::from(round) * 100)?;
        run(&tmp, &["update", "a.tar.gz", "src"])?;

        let total: usize = name_counts(&tmp, "a.tar.gz")?.values().sum();
        assert_eq!(
            total, 2,
            "round {round}: archive must hold exactly src + src/a.txt",
        );
    }
    Ok(())
}

#[test]
fn update_without_changes_is_a_no_op() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let src = tmp.join("src");
    fs_err::create_dir_all(&src)?;
    fs_err::write(src.join("a.txt"), "v1")?;
    set_mtime(&src.join("a.txt"), 1_000_000_000)?;
    set_mtime(&src, 1_000_000_000)?;
    run(&tmp, &["compress", "src", "-o", "a.tar.gz"])?;
    let before = name_counts(&tmp, "a.tar.gz")?;

    run(&tmp, &["update", "a.tar.gz", "src"])?;
    let after = name_counts(&tmp, "a.tar.gz")?;
    assert_eq!(before, after, "an update with nothing newer must not change the entry set");
    Ok(())
}

/// `update <archive> .` walks names spelled `./f.txt` while tar stores
/// `f.txt`; the gate and keep-plan must compare written names or every
/// update re-appends the whole tree.
#[test]
fn update_from_dot_input_does_not_duplicate() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let src = tmp.join("src");
    fs_err::create_dir_all(src.join("sub"))?;
    fs_err::write(src.join("a.txt"), "v1")?;
    fs_err::write(src.join("sub/b.txt"), "b")?;
    for p in [&src.join("a.txt"), &src.join("sub/b.txt"), &src.join("sub"), &src] {
        set_mtime(p, 1_000_000_000)?;
    }

    run(&src, &["compress", ".", "-o", "../a.tar.gz"])?;
    let baseline: usize = name_counts(&tmp, "a.tar.gz")?.values().sum();

    run(&src, &["update", "../a.tar.gz", "."])?;
    run(&src, &["update", "../a.tar.gz", "."])?;
    let after_noop: usize = name_counts(&tmp, "a.tar.gz")?.values().sum();
    assert_eq!(after_noop, baseline, "no-op updates from `.` must not grow the archive");

    fs_err::write(src.join("a.txt"), "v2")?;
    set_mtime(&src.join("a.txt"), 1_100_000_000)?;
    run(&src, &["update", "../a.tar.gz", "."])?;
    let counts = name_counts(&tmp, "a.tar.gz")?;
    assert_eq!(counts.values().sum::<usize>(), baseline, "counts: {counts:?}");
    assert_eq!(counts.get("a.txt"), Some(&1), "counts: {counts:?}");

    let out_dir = tmp.join("out");
    run(&tmp, &["decompress", "a.tar.gz", "-o", out_dir.as_str()])?;
    assert_eq!(fs_err::read_to_string(out_dir.join("a.txt"))?, "v2");
    Ok(())
}

/// Dropping a superseded entry that a kept hard link points at would move the
/// target *after* the link, which no extractor can resolve.  The plan must
/// keep the stale copy; the appended version still wins by coming last.
#[test]
fn update_keeps_hardlink_target_extractable() -> TestResult {
    use std::io::Write as _;

    let (_guard, tmp) = temp_dir()?;

    // f (regular, "old"), link (hardlink -> h/f): the shape GNU tar produces
    // for two hardlinked files.  Built by hand so the entry order is fixed.
    let mut builder = tar::Builder::new(Vec::new());
    let body = b"old";
    let mut h = tar::Header::new_gnu();
    h.set_size(body.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(1_000_000_000);
    builder.append_data(&mut h, "h/f", body.as_slice())?;
    let mut l = tar::Header::new_gnu();
    l.set_entry_type(tar::EntryType::Link);
    l.set_size(0);
    l.set_mode(0o644);
    l.set_mtime(1_000_000_000);
    l.set_link_name("h/f")?;
    builder.append_data(&mut l, "h/link", std::io::empty())?;
    let tar_bytes = builder.into_inner()?;

    let archive = tmp.join("h.tar.gz");
    let mut enc =
        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&tar_bytes)?;
    fs_err::write(&archive, enc.finish()?)?;

    // On disk: only a newer f, so the update supersedes the link's target.
    let src = tmp.join("h");
    fs_err::create_dir_all(&src)?;
    fs_err::write(src.join("f"), "new")?;
    set_mtime(&src.join("f"), 1_100_000_000)?;
    set_mtime(&src, 1_000_000_000)?;
    run(&tmp, &["update", "h.tar.gz", "h"])?;

    let out_dir = tmp.join("out");
    run(&tmp, &["decompress", "h.tar.gz", "-o", out_dir.as_str()])?;
    assert_eq!(fs_err::read_to_string(out_dir.join("h/f"))?, "new");
    // The link was created from the pre-update copy — same as GNU tar.
    assert_eq!(fs_err::read_to_string(out_dir.join("h/link"))?, "old");
    Ok(())
}

#[test]
fn update_still_adds_genuinely_new_files() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let src = tmp.join("src");
    fs_err::create_dir_all(&src)?;
    fs_err::write(src.join("a.txt"), "v1")?;
    run(&tmp, &["compress", "src", "-o", "a.tar.zst"])?;

    fs_err::write(src.join("b.txt"), "new file")?;
    run(&tmp, &["update", "a.tar.zst", "src"])?;

    let counts = name_counts(&tmp, "a.tar.zst")?;
    assert_eq!(counts.get("src/b.txt"), Some(&1), "counts: {counts:?}");
    assert_eq!(counts.get("src/a.txt"), Some(&1));
    Ok(())
}
