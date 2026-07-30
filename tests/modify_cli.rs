//! End-to-end coverage for `append` / `update` through the compiled binary,
//! focused on what happens to the *existing* archive: a rejected or failed
//! append must leave it exactly as it was, and re-adding a name already in a
//! zip must replace that entry rather than blow up.

mod helpers;

use std::process::Command;
use std::thread;
use std::time::Duration;

use camino::Utf8Path;

use helpers::{
    TAR, TestResult, ZIP, default_compress_opts, default_decompress_opts, temp_utf8_dir,
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
