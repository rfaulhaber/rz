//! End-to-end coverage for `append` / `update` through the compiled binary,
//! focused on what happens to the *existing* archive: a rejected or failed
//! append must leave it exactly as it was, and re-adding a name already in a
//! zip must replace that entry rather than blow up.

mod helpers;

use std::io::Write;
use std::process::Command;
use std::thread;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};

use helpers::{
    TAR, TAR_GZ, TestResult, ZIP, default_compress_opts, default_decompress_opts, temp_utf8_dir,
};

fn rz_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz")
}

fn run(args: &[&str]) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    Ok(Command::new(rz_bin()).args(args).output()?)
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Sorted entry names of a tar archive.
fn tar_names(archive: &Utf8Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut names: Vec<String> = (TAR.list)(archive)?
        .iter()
        .map(|e| e.path.to_string())
        .collect();
    names.sort();
    Ok(names)
}

/// Sorted entry names of a zip archive.
fn zip_names(archive: &Utf8Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut names: Vec<String> = (ZIP.list)(archive)?
        .iter()
        .map(|e| e.path.to_string())
        .collect();
    names.sort();
    Ok(names)
}

// ── tar: the archive is only mutated once the inputs are known good ──────────

#[test]
fn tar_append_missing_input_leaves_archive_byte_identical() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let src = tmp.join("src");
    fs_err::create_dir(&src)?;
    fs_err::write(src.join("a.txt"), b"alpha\n")?;

    let archive = tmp.join("ar.tar");
    (TAR.compress)(
        std::slice::from_ref(&src),
        &archive,
        &default_compress_opts(None),
    )?;
    let before = fs_err::read(&archive)?;

    let missing = tmp.join("does-not-exist");
    let out = run(&["append", archive.as_str(), src.as_str(), missing.as_str()])?;
    assert!(
        !out.status.success(),
        "append with a missing input must fail; stderr={}",
        stderr_of(&out),
    );

    assert_eq!(
        before,
        fs_err::read(&archive)?,
        "a rejected append must not touch the archive",
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn tar_append_rolls_back_when_walk_fails_midway() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let (_g, tmp) = temp_utf8_dir()?;
    let src = tmp.join("src");
    fs_err::create_dir(&src)?;
    fs_err::write(src.join("a.txt"), b"alpha\n")?;

    let archive = tmp.join("ar.tar");
    (TAR.compress)(
        std::slice::from_ref(&src),
        &archive,
        &default_compress_opts(None),
    )?;
    let before_names = tar_names(&archive)?;
    let before_bytes = fs_err::read(&archive)?;

    // `add/` stats fine, so validation passes; the walk writes `add/one.txt`
    // and only then fails opening the unreadable second file.
    let add = tmp.join("add");
    fs_err::create_dir(&add)?;
    fs_err::write(add.join("one.txt"), b"readable\n")?;
    let blocked = add.join("two.bin");
    fs_err::write(&blocked, b"unreadable\n")?;
    fs_err::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000))?;
    if fs_err::File::open(&blocked).is_ok() {
        // Mode bits do not stop root, so there is no mid-walk failure to test.
        return Ok(());
    }

    let out = run(&["append", archive.as_str(), add.as_str()])?;
    assert!(
        !out.status.success(),
        "append over an unreadable file must fail; stderr={}",
        stderr_of(&out),
    );

    assert_eq!(
        tar_names(&archive)?,
        before_names,
        "a failed append must roll back to the original entries",
    );
    assert_eq!(
        fs_err::read(&archive)?,
        before_bytes,
        "rolled-back archive must match the original bytes",
    );

    // The rolled-back archive must still extract cleanly.
    let outdir = tmp.join("out");
    fs_err::create_dir(&outdir)?;
    (TAR.decompress)(&archive, &outdir, &default_decompress_opts())?;
    assert_eq!(fs_err::read(outdir.join("src/a.txt"))?, b"alpha\n");
    Ok(())
}

#[test]
fn tar_append_repeated_failure_does_not_accumulate_entries() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let src = tmp.join("src");
    fs_err::create_dir(&src)?;
    fs_err::write(src.join("a.txt"), b"alpha\n")?;

    let archive = tmp.join("ar.tar");
    (TAR.compress)(
        std::slice::from_ref(&src),
        &archive,
        &default_compress_opts(None),
    )?;
    let before_names = tar_names(&archive)?;

    let missing = tmp.join("does-not-exist");
    for _ in 0..3 {
        let out = run(&["append", archive.as_str(), src.as_str(), missing.as_str()])?;
        assert!(
            !out.status.success(),
            "append must fail: {}",
            stderr_of(&out)
        );
    }

    assert_eq!(
        tar_names(&archive)?,
        before_names,
        "retrying a failing append must not stack up copies",
    );
    Ok(())
}

// ── zip: re-adding an archived name replaces it ──────────────────────────────

#[test]
fn zip_update_rewrites_changed_entry_once() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let src = tmp.join("src");
    fs_err::create_dir(&src)?;
    fs_err::write(src.join("a.txt"), b"v1\n")?;
    fs_err::write(src.join("b.txt"), b"bravo\n")?;

    let archive = tmp.join("ar.zip");
    (ZIP.compress)(
        std::slice::from_ref(&src),
        &archive,
        &default_compress_opts(None),
    )?;

    // Zip stores DOS timestamps at 2-second resolution, so the rewrite has to
    // land in a later slot for `update` to consider the file newer.
    thread::sleep(Duration::from_millis(2100));
    fs_err::write(src.join("a.txt"), b"v2 content\n")?;

    let out = run(&["update", archive.as_str(), src.as_str()])?;
    assert!(
        out.status.success(),
        "update of a changed file failed: {}",
        stderr_of(&out),
    );

    let names = zip_names(&archive)?;
    assert_eq!(
        names.iter().filter(|n| n.as_str() == "src/a.txt").count(),
        1,
        "updated entry must appear exactly once: {names:?}",
    );
    assert!(
        names.iter().any(|n| n.as_str() == "src/b.txt"),
        "untouched sibling must survive: {names:?}",
    );

    let outdir = tmp.join("out");
    fs_err::create_dir(&outdir)?;
    (ZIP.decompress)(&archive, &outdir, &default_decompress_opts())?;
    assert_eq!(fs_err::read(outdir.join("src/a.txt"))?, b"v2 content\n");
    assert_eq!(fs_err::read(outdir.join("src/b.txt"))?, b"bravo\n");
    Ok(())
}

#[test]
fn zip_append_level_zero_stores_entry() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let a = tmp.join("a.txt");
    fs_err::write(&a, b"alpha\n")?;

    let archive = tmp.join("ar.zip");
    (ZIP.compress)(
        std::slice::from_ref(&a),
        &archive,
        &default_compress_opts(None),
    )?;

    let b = tmp.join("b.txt");
    fs_err::write(&b, b"bravo\n")?;
    let out = run(&["append", archive.as_str(), "--level", "0", b.as_str()])?;
    assert!(
        out.status.success(),
        "append --level 0 failed: {}",
        stderr_of(&out),
    );

    let file = fs_err::File::open(&archive)?;
    let mut zip = ::zip::ZipArchive::new(file)?;
    let entry = zip.by_name("b.txt")?;
    assert_eq!(
        entry.compression(),
        ::zip::CompressionMethod::Stored,
        "level 0 must store the appended entry uncompressed",
    );
    Ok(())
}

#[test]
fn zip_append_adds_new_and_replaces_existing() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let a = tmp.join("a.txt");
    fs_err::write(&a, b"alpha\n")?;

    let archive = tmp.join("ar.zip");
    (ZIP.compress)(
        std::slice::from_ref(&a),
        &archive,
        &default_compress_opts(None),
    )?;

    let b = tmp.join("b.txt");
    fs_err::write(&b, b"bravo\n")?;
    let out = run(&["append", archive.as_str(), b.as_str()])?;
    assert!(
        out.status.success(),
        "appending a new file failed: {}",
        stderr_of(&out),
    );
    assert_eq!(zip_names(&archive)?, vec!["a.txt", "b.txt"]);

    fs_err::write(&a, b"alpha v2\n")?;
    let out = run(&["append", archive.as_str(), a.as_str()])?;
    assert!(
        out.status.success(),
        "appending an already-archived name failed: {}",
        stderr_of(&out),
    );
    assert_eq!(
        zip_names(&archive)?,
        vec!["a.txt", "b.txt"],
        "re-appending a name must replace it, not duplicate it",
    );

    let outdir = tmp.join("out");
    fs_err::create_dir(&outdir)?;
    (ZIP.decompress)(&archive, &outdir, &default_decompress_opts())?;
    assert_eq!(fs_err::read(outdir.join("a.txt"))?, b"alpha v2\n");
    assert_eq!(fs_err::read(outdir.join("b.txt"))?, b"bravo\n");
    Ok(())
}

// ── GNU sparse entries: modify operations must refuse, not corrupt ──────────
//
// tar-rs reports `entry.size()` for a GNU sparse entry as the *expanded*
// logical size (its sparse-header parser sets it from the header's
// real-size field), not the bytes actually stored in the archive. Every
// modify path that trusted it either seeks/truncates to the wrong offset
// (the in-place `tar_append`) or clones the header while feeding the
// fully-expanded body through `append_data` (every read-rewrite path), so
// this must be refused before anything is written.

/// Path of the temp file a read-rewrite modify operation would use.
fn tmp_sibling(archive: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(format!("{archive}.tmp.rzappend"))
}

/// Build the bytes of a tar archive holding one GNU-sparse entry — 5
/// physical bytes standing in for a 100_000-byte logical file, with the
/// data placed at the tail so parsing it requires a leading hole — followed
/// by an ordinary entry, so a corrupting rewrite has something to lose.
fn build_gnu_sparse_tar_bytes() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut builder = tar::Builder::new(Vec::new());

    let data = b"alpha";
    let logical_len = 100_000u64;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::GNUSparse);
    header.set_mode(0o644);
    header.set_size(data.len() as u64);
    {
        let gnu = header
            .as_gnu_mut()
            .ok_or("Header::new_gnu() must report as_gnu_mut()")?;
        gnu.set_real_size(logical_len);
        gnu.sparse[0].set_offset(logical_len - data.len() as u64);
        gnu.sparse[0].set_length(data.len() as u64);
    }
    builder.append_data(&mut header, "sparse.dat", &data[..])?;

    let body = b"plain file\n";
    let mut plain = tar::Header::new_gnu();
    plain.set_entry_type(tar::EntryType::Regular);
    plain.set_mode(0o644);
    plain.set_size(body.len() as u64);
    builder.append_data(&mut plain, "plain.txt", &body[..])?;

    Ok(builder.into_inner()?)
}

fn write_sparse_tar(path: &Utf8Path) -> TestResult {
    fs_err::write(path, build_gnu_sparse_tar_bytes()?)?;
    Ok(())
}

fn write_sparse_tar_gz(path: &Utf8Path) -> TestResult {
    let raw = build_gnu_sparse_tar_bytes()?;
    let file = fs_err::File::create(path)?;
    let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    enc.write_all(&raw)?;
    enc.finish()?;
    Ok(())
}

#[test]
fn sparse_tar_append_refuses_and_leaves_archive_untouched() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("sparse.tar");
    write_sparse_tar(&archive)?;
    let before = fs_err::read(&archive)?;

    let f = tmp.join("f.txt");
    fs_err::write(&f, b"new\n")?;

    let out = run(&["append", archive.as_str(), f.as_str()])?;
    assert!(
        !out.status.success(),
        "append over a GNU sparse archive must fail",
    );
    assert!(
        stderr_of(&out).to_lowercase().contains("sparse"),
        "error should mention sparse entries: {}",
        stderr_of(&out),
    );
    assert_eq!(
        fs_err::read(&archive)?,
        before,
        "a refused append must not touch the archive",
    );
    assert!(
        !tmp_sibling(&archive).exists(),
        "no temp file should remain after a refused append",
    );
    Ok(())
}

#[test]
fn sparse_tar_remove_nomatch_refuses_and_leaves_archive_untouched() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("sparse.tar");
    write_sparse_tar(&archive)?;
    let before = fs_err::read(&archive)?;

    // Even a pattern that matches nothing must be refused: the read-rewrite
    // path clones the sparse header before it knows whether anything will
    // be dropped, so a no-op remove is just as destructive pre-fix.
    let out = run(&["remove", archive.as_str(), "nomatch"])?;
    assert!(
        !out.status.success(),
        "remove over a GNU sparse archive must fail even with no matches",
    );
    assert!(
        stderr_of(&out).to_lowercase().contains("sparse"),
        "error should mention sparse entries: {}",
        stderr_of(&out),
    );
    assert_eq!(
        fs_err::read(&archive)?,
        before,
        "a refused remove must not touch the archive",
    );
    assert!(
        !tmp_sibling(&archive).exists(),
        "no temp file should remain after a refused remove",
    );
    Ok(())
}

#[test]
fn sparse_tar_gz_append_refuses_and_leaves_archive_untouched() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("sparse.tar.gz");
    write_sparse_tar_gz(&archive)?;
    let before = fs_err::read(&archive)?;

    let f = tmp.join("f.txt");
    fs_err::write(&f, b"new\n")?;

    let out = run(&["append", archive.as_str(), f.as_str()])?;
    assert!(
        !out.status.success(),
        "append over a GNU sparse tar.gz archive must fail",
    );
    assert!(
        stderr_of(&out).to_lowercase().contains("sparse"),
        "error should mention sparse entries: {}",
        stderr_of(&out),
    );
    assert_eq!(
        fs_err::read(&archive)?,
        before,
        "a refused append must not touch the archive",
    );
    assert!(
        !tmp_sibling(&archive).exists(),
        "no temp file should remain after a refused append",
    );
    Ok(())
}

#[test]
fn sparse_tar_gz_remove_nomatch_refuses_and_leaves_archive_untouched() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("sparse.tar.gz");
    write_sparse_tar_gz(&archive)?;
    let before = fs_err::read(&archive)?;

    let out = run(&["remove", archive.as_str(), "nomatch"])?;
    assert!(
        !out.status.success(),
        "remove over a GNU sparse tar.gz archive must fail even with no matches",
    );
    assert!(
        stderr_of(&out).to_lowercase().contains("sparse"),
        "error should mention sparse entries: {}",
        stderr_of(&out),
    );
    assert_eq!(
        fs_err::read(&archive)?,
        before,
        "a refused remove must not touch the archive",
    );
    assert!(
        !tmp_sibling(&archive).exists(),
        "no temp file should remain after a refused remove",
    );
    Ok(())
}

// ── temp-file cleanup on failure (read-rewrite paths) ────────────────────────

#[cfg(unix)]
#[test]
fn tar_gz_append_cleans_up_temp_file_when_walk_fails_midway() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let (_g, tmp) = temp_utf8_dir()?;
    let src = tmp.join("src");
    fs_err::create_dir(&src)?;
    fs_err::write(src.join("a.txt"), b"alpha\n")?;

    let archive = tmp.join("ar.tar.gz");
    (TAR_GZ.compress)(
        std::slice::from_ref(&src),
        &archive,
        &default_compress_opts(None),
    )?;
    let before_bytes = fs_err::read(&archive)?;

    // `add/` stats fine, so validation passes; the walk copies the existing
    // entries, writes `add/one.txt`, and only then fails opening the
    // unreadable second file — partway through the temp file.
    let add = tmp.join("add");
    fs_err::create_dir(&add)?;
    fs_err::write(add.join("one.txt"), b"readable\n")?;
    let blocked = add.join("two.bin");
    fs_err::write(&blocked, b"unreadable\n")?;
    fs_err::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000))?;
    if fs_err::File::open(&blocked).is_ok() {
        // Mode bits do not stop root, so there is no mid-walk failure to test.
        return Ok(());
    }

    let out = run(&["append", archive.as_str(), add.as_str()])?;
    assert!(
        !out.status.success(),
        "append over an unreadable file must fail; stderr={}",
        stderr_of(&out),
    );

    assert_eq!(
        fs_err::read(&archive)?,
        before_bytes,
        "a failed compressed append must leave the original archive untouched",
    );
    assert!(
        !tmp_sibling(&archive).exists(),
        "a failed compressed append must not leave its temp file behind",
    );
    Ok(())
}
