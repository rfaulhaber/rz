mod helpers;

use std::time::{Duration, UNIX_EPOCH};

use camino::Utf8Path;
use globset::GlobSet;
use helpers::{TestResult, temp_utf8_dir};

use rz::{CompressOpts, DecompressOpts};

/// Build a tar archive with two file entries, each given an explicit mtime.
/// Used by decompress tests to isolate mtime-window behaviour from filesystem
/// timing quirks.
fn build_tar_with_mtimes(archive: &Utf8Path, old_mtime: u64, new_mtime: u64) -> TestResult {
    let file = fs_err::File::create(archive)?;
    let mut builder = tar::Builder::new(file);

    for (name, mtime, body) in [
        ("old.txt", old_mtime, b"old\n" as &[u8]),
        ("new.txt", new_mtime, b"new\n" as &[u8]),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(mtime);
        header.set_cksum();
        builder.append_data(&mut header, name, body)?;
    }
    builder.finish()?;
    Ok(())
}

#[test]
fn decompress_newer_than_filters_old_entries() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("in.tar");
    build_tar_with_mtimes(&archive, 100, 200)?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;

    let mut opts = DecompressOpts::new(false, 0, GlobSet::empty(), GlobSet::empty());
    opts.newer_than = Some(150);
    rz::tar::decompress(&archive, &out, &opts)?;

    assert!(!out.join("old.txt").exists(), "old.txt should be filtered");
    assert!(out.join("new.txt").exists(), "new.txt should extract");
    Ok(())
}

#[test]
fn decompress_older_than_filters_new_entries() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("in.tar");
    build_tar_with_mtimes(&archive, 100, 200)?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;

    let mut opts = DecompressOpts::new(false, 0, GlobSet::empty(), GlobSet::empty());
    opts.older_than = Some(150);
    rz::tar::decompress(&archive, &out, &opts)?;

    assert!(out.join("old.txt").exists(), "old.txt should extract");
    assert!(!out.join("new.txt").exists(), "new.txt should be filtered");
    Ok(())
}

#[test]
fn decompress_bounds_are_exclusive() -> TestResult {
    // GNU-tar semantics: `--newer-than 100` means strictly greater than 100.
    // Both bounds therefore exclude an entry with mtime exactly 100.
    let (_guard, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("in.tar");
    build_tar_with_mtimes(&archive, 100, 100)?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;

    let mut opts = DecompressOpts::new(false, 0, GlobSet::empty(), GlobSet::empty());
    opts.newer_than = Some(100);
    rz::tar::decompress(&archive, &out, &opts)?;
    assert!(!out.join("old.txt").exists());
    assert!(!out.join("new.txt").exists());

    fs_err::remove_dir_all(&out)?;
    fs_err::create_dir(&out)?;

    let mut opts = DecompressOpts::new(false, 0, GlobSet::empty(), GlobSet::empty());
    opts.older_than = Some(100);
    rz::tar::decompress(&archive, &out, &opts)?;
    assert!(!out.join("old.txt").exists());
    assert!(!out.join("new.txt").exists());
    Ok(())
}

#[test]
fn decompress_window_keeps_entries_inside_range() -> TestResult {
    // Three entries at mtime 100, 200, 300; extract with newer=150, older=250.
    let (_guard, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("in.tar");
    let file = fs_err::File::create(&archive)?;
    let mut builder = tar::Builder::new(file);
    for (name, mtime) in [("a.txt", 100u64), ("b.txt", 200), ("c.txt", 300)] {
        let body = name.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(mtime);
        header.set_cksum();
        builder.append_data(&mut header, name, body)?;
    }
    builder.finish()?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;

    let mut opts = DecompressOpts::new(false, 0, GlobSet::empty(), GlobSet::empty());
    opts.newer_than = Some(150);
    opts.older_than = Some(250);
    rz::tar::decompress(&archive, &out, &opts)?;

    assert!(!out.join("a.txt").exists());
    assert!(out.join("b.txt").exists());
    assert!(!out.join("c.txt").exists());
    Ok(())
}

#[test]
fn compress_newer_than_skips_old_files() -> TestResult {
    // Set explicit filesystem mtimes so the test is not clock-sensitive.
    let (_guard, tmp) = temp_utf8_dir()?;
    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;

    let old = tree.join("old.txt");
    let new = tree.join("new.txt");
    fs_err::write(&old, b"old\n")?;
    fs_err::write(&new, b"new\n")?;

    let old_time = UNIX_EPOCH + Duration::from_secs(1_000_000);
    let new_time = UNIX_EPOCH + Duration::from_secs(2_000_000);
    fs_err::File::options()
        .write(true)
        .open(&old)?
        .set_modified(old_time)?;
    fs_err::File::options()
        .write(true)
        .open(&new)?
        .set_modified(new_time)?;

    let archive = tmp.join("out.tar");
    let mut opts = CompressOpts::new(None, GlobSet::empty());
    opts.newer_than = Some(1_500_000);
    rz::tar::compress(std::slice::from_ref(&tree), &archive, &opts)?;

    // List the archive and check which files made it in.
    let entries = rz::tar::list(&archive)?;
    let names: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert!(
        !names.iter().any(|n| n.ends_with("old.txt")),
        "old.txt leaked in: {names:?}",
    );
    assert!(
        names.iter().any(|n| n.ends_with("new.txt")),
        "new.txt missing: {names:?}",
    );
    Ok(())
}

#[test]
fn compress_older_than_skips_new_files() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;

    let old = tree.join("old.txt");
    let new = tree.join("new.txt");
    fs_err::write(&old, b"old\n")?;
    fs_err::write(&new, b"new\n")?;

    fs_err::File::options()
        .write(true)
        .open(&old)?
        .set_modified(UNIX_EPOCH + Duration::from_secs(1_000_000))?;
    fs_err::File::options()
        .write(true)
        .open(&new)?
        .set_modified(UNIX_EPOCH + Duration::from_secs(2_000_000))?;

    let archive = tmp.join("out.tar");
    let mut opts = CompressOpts::new(None, GlobSet::empty());
    opts.older_than = Some(1_500_000);
    rz::tar::compress(std::slice::from_ref(&tree), &archive, &opts)?;

    let entries = rz::tar::list(&archive)?;
    let names: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert!(names.iter().any(|n| n.ends_with("old.txt")));
    assert!(!names.iter().any(|n| n.ends_with("new.txt")));
    Ok(())
}

#[test]
fn cli_parses_date_spellings() -> TestResult {
    // RFC 3339, date-only, and `@unix` must all be accepted by --newer-than.
    let bin = env!("CARGO_BIN_EXE_rz");
    let (_guard, tmp) = temp_utf8_dir()?;
    let file = tmp.join("x.txt");
    fs_err::write(&file, b"x")?;

    for spelling in ["2020-01-01", "2020-01-01T00:00:00Z", "@1577836800"] {
        let archive = tmp.join(format!("out-{}.tar", spelling.replace([':', '@'], "_")));
        let out = std::process::Command::new(bin)
            .args([
                "compress",
                file.as_str(),
                "-o",
                archive.as_str(),
                "--newer-than",
                spelling,
            ])
            .output()?;
        assert!(
            out.status.success(),
            "--newer-than {spelling} failed: {}",
            String::from_utf8_lossy(&out.stderr),
        );
    }
    Ok(())
}

#[test]
fn cli_rejects_newer_than_on_zip_compress() -> TestResult {
    let bin = env!("CARGO_BIN_EXE_rz");
    let (_guard, tmp) = temp_utf8_dir()?;
    let file = tmp.join("x.txt");
    fs_err::write(&file, b"x")?;
    let archive = tmp.join("out.zip");

    let out = std::process::Command::new(bin)
        .args([
            "compress",
            file.as_str(),
            "-o",
            archive.as_str(),
            "--newer-than",
            "2020-01-01",
        ])
        .output()?;
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--newer-than"),
        "stderr should mention --newer-than: {stderr}"
    );
    Ok(())
}

#[test]
fn cli_rejects_older_than_on_zip_decompress() -> TestResult {
    let bin = env!("CARGO_BIN_EXE_rz");
    let (_guard, tmp) = temp_utf8_dir()?;
    let file = tmp.join("x.txt");
    fs_err::write(&file, b"x")?;
    let archive = tmp.join("out.zip");

    // Build a real zip to decompress against.
    let status = std::process::Command::new(bin)
        .args(["compress", file.as_str(), "-o", archive.as_str()])
        .status()?;
    assert!(status.success());

    let out_dir = tmp.join("extracted");
    let out = std::process::Command::new(bin)
        .args([
            "decompress",
            archive.as_str(),
            "-o",
            out_dir.as_str(),
            "--older-than",
            "2030-01-01",
        ])
        .output()?;
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--older-than"),
        "stderr should mention --older-than: {stderr}"
    );
    Ok(())
}
