mod helpers;

use camino::Utf8PathBuf;
use globset::GlobSet;
use helpers::{TAR_GZ, TestResult, temp_utf8_dir};
use rz_archive::DecompressOpts;
use rz_archive::cmd::parse_rename;

// ── rename: substring replacement ────────────────────────────────────────────

#[test]
fn rename_replaces_substring_in_extracted_paths() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    // Build a tree with a `src/` directory.
    let tree = tmp.join("project");
    fs_err::create_dir_all(tree.join("src"))?;
    fs_err::write(tree.join("src/main.rs"), b"fn main() {}")?;

    let archive = tmp.join("project.tar.gz");
    (TAR_GZ.compress)(&[tree], &archive, &helpers::default_compress_opts(None))?;

    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;

    let mut opts = DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty());
    opts.renames = vec![("src/".to_owned(), "lib/".to_owned())];

    (TAR_GZ.decompress)(&archive, &out, &opts)?;

    // The renamed path must exist.
    assert!(
        out.join("project/lib/main.rs").exists(),
        "expected project/lib/main.rs to exist"
    );
    // The original path must not exist.
    assert!(
        !out.join("project/src/main.rs").exists(),
        "expected project/src/main.rs to be absent after rename"
    );
    Ok(())
}

// ── prefix: prepend to every entry ───────────────────────────────────────────

#[test]
fn prefix_prepends_to_every_entry() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("data");
    fs_err::create_dir_all(tree.join("sub"))?;
    fs_err::write(tree.join("file.txt"), b"content")?;
    fs_err::write(tree.join("sub/nested.txt"), b"nested")?;

    let archive = tmp.join("data.tar.gz");
    (TAR_GZ.compress)(&[tree], &archive, &helpers::default_compress_opts(None))?;

    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;

    let mut opts = DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty());
    opts.prefix = Some(Utf8PathBuf::from("backup"));

    (TAR_GZ.decompress)(&archive, &out, &opts)?;

    // Every entry should be prefixed with `backup/`.
    assert!(
        out.join("backup/data/file.txt").exists(),
        "expected backup/data/file.txt"
    );
    assert!(
        out.join("backup/data/sub/nested.txt").exists(),
        "expected backup/data/sub/nested.txt"
    );
    // Original paths (without prefix) must not exist.
    assert!(
        !out.join("data/file.txt").exists(),
        "data/file.txt should be absent (prefix not applied)"
    );
    Ok(())
}

// ── rename + prefix combined ──────────────────────────────────────────────────

#[test]
fn rename_then_prefix_combines() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("app");
    fs_err::create_dir_all(tree.join("src"))?;
    fs_err::write(tree.join("src/lib.rs"), b"// lib")?;

    let archive = tmp.join("app.tar.gz");
    (TAR_GZ.compress)(&[tree], &archive, &helpers::default_compress_opts(None))?;

    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;

    let mut opts = DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty());
    opts.renames = vec![("src/".to_owned(), "lib/".to_owned())];
    opts.prefix = Some(Utf8PathBuf::from("v2"));

    (TAR_GZ.decompress)(&archive, &out, &opts)?;

    assert!(
        out.join("v2/app/lib/lib.rs").exists(),
        "expected v2/app/lib/lib.rs"
    );
    assert!(
        !out.join("app/src/lib.rs").exists(),
        "original path should be absent"
    );
    Ok(())
}

// ── path-traversal guard ──────────────────────────────────────────────────────

#[test]
fn rename_rejects_path_traversal() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("safe");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("foo.txt"), b"data")?;

    let archive = tmp.join("safe.tar.gz");
    (TAR_GZ.compress)(&[tree], &archive, &helpers::default_compress_opts(None))?;

    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;

    // The rename maps "safe" → "../escape", which after applying produces a
    // path that starts with "..".
    let mut opts = DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty());
    opts.renames = vec![("safe".to_owned(), "../escape".to_owned())];

    let result = (TAR_GZ.decompress)(&archive, &out, &opts);
    assert!(
        result.is_err(),
        "expected an error for path-traversal rename, got Ok"
    );
    Ok(())
}

// ── CLI parser unit tests ─────────────────────────────────────────────────────

#[test]
fn cli_rename_parses_old_equals_new() {
    let result = parse_rename("src/=lib/");
    assert_eq!(result, Ok(("src/".to_owned(), "lib/".to_owned())));
}

#[test]
fn cli_rename_parses_empty_new_is_allowed() {
    // NEW can be empty (deletes the substring).
    let result = parse_rename("debug=");
    assert_eq!(result, Ok(("debug".to_owned(), String::new())));
}

#[test]
fn cli_rename_rejects_missing_equals() {
    let result = parse_rename("nodivider");
    assert!(
        result.is_err(),
        "expected Err for input without '=', got {:?}",
        result
    );
}

#[test]
fn cli_rename_rejects_empty_old() {
    let result = parse_rename("=newvalue");
    assert!(
        result.is_err(),
        "expected Err for empty OLD, got {:?}",
        result
    );
}
