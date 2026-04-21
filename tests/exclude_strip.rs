mod helpers;

use camino::Utf8Path;
use globset::GlobSet;

use helpers::{TAR_GZ, TestResult, ZIP, build_file_tree, temp_utf8_dir};
use rz::filter::build_glob_set;
use rz::{CompressOpts, DecompressOpts};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn compress_opts(excludes: &[&str]) -> CompressOpts<'static> {
    CompressOpts::new(
        None,
        build_glob_set(&excludes.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
            .unwrap_or_else(|_| GlobSet::empty()),
    )
}

fn decompress_opts(strip: u32, excludes: &[&str]) -> DecompressOpts<'static> {
    DecompressOpts::new(
        false,
        strip,
        GlobSet::empty(),
        build_glob_set(&excludes.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
            .unwrap_or_else(|_| GlobSet::empty()),
    )
}

/// Build a richer tree for exclude / strip tests:
///
/// ```text
/// root/
///   hello.txt
///   notes.log
///   subdir/
///     nested.txt
///     debug.log
/// ```
fn build_extended_tree(root: &Utf8Path) -> std::io::Result<()> {
    fs_err::create_dir_all(root.join("subdir"))?;
    fs_err::write(root.join("hello.txt"), b"hello world\n")?;
    fs_err::write(root.join("notes.log"), b"log data\n")?;
    fs_err::write(root.join("subdir/nested.txt"), b"nested content\n")?;
    fs_err::write(root.join("subdir/debug.log"), b"debug log\n")?;
    Ok(())
}

// ── --exclude on compress ────────────────────────────────────────────────────

#[test]
fn tar_gz_compress_exclude_by_extension() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_extended_tree(&tree)?;

    let archive = tmp.join("archive.tar.gz");
    let opts = compress_opts(&["*.log"]);
    (TAR_GZ.compress)(&[tree], &archive, &opts)?;

    let entries = (TAR_GZ.list)(&archive)?;
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();

    assert!(paths.iter().any(|p| p.ends_with("hello.txt")));
    assert!(paths.iter().any(|p| p.ends_with("nested.txt")));
    assert!(
        !paths.iter().any(|p| p.ends_with(".log")),
        "log files should be excluded: {paths:?}"
    );
    Ok(())
}

#[test]
fn zip_compress_exclude_by_extension() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_extended_tree(&tree)?;

    let archive = tmp.join("archive.zip");
    let opts = compress_opts(&["*.log"]);
    (ZIP.compress)(&[tree], &archive, &opts)?;

    let entries = (ZIP.list)(&archive)?;
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();

    assert!(paths.iter().any(|p| p.ends_with("hello.txt")));
    assert!(paths.iter().any(|p| p.ends_with("nested.txt")));
    assert!(
        !paths.iter().any(|p| p.ends_with(".log")),
        "log files should be excluded: {paths:?}"
    );
    Ok(())
}

#[test]
fn tar_gz_compress_exclude_directory() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_extended_tree(&tree)?;

    let archive = tmp.join("archive.tar.gz");
    let opts = compress_opts(&["subdir"]);
    (TAR_GZ.compress)(&[tree], &archive, &opts)?;

    let entries = (TAR_GZ.list)(&archive)?;
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();

    assert!(paths.iter().any(|p| p.ends_with("hello.txt")));
    assert!(
        !paths.iter().any(|p| p.contains("subdir")),
        "subdir should be excluded: {paths:?}"
    );
    Ok(())
}

// ── --exclude on decompress ──────────────────────────────────────────────────

#[test]
fn tar_gz_decompress_exclude_by_extension() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_extended_tree(&tree)?;

    let archive = tmp.join("archive.tar.gz");
    (TAR_GZ.compress)(&[tree], &archive, &compress_opts(&[]))?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let opts = decompress_opts(0, &["*.log"]);
    (TAR_GZ.decompress)(&archive, &out, &opts)?;

    assert!(out.join("tree/hello.txt").exists());
    assert!(out.join("tree/subdir/nested.txt").exists());
    assert!(
        !out.join("tree/notes.log").exists(),
        "notes.log should be excluded"
    );
    assert!(
        !out.join("tree/subdir/debug.log").exists(),
        "debug.log should be excluded"
    );
    Ok(())
}

#[test]
fn zip_decompress_exclude_by_extension() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_extended_tree(&tree)?;

    let archive = tmp.join("archive.zip");
    (ZIP.compress)(&[tree], &archive, &compress_opts(&[]))?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let opts = decompress_opts(0, &["*.log"]);
    (ZIP.decompress)(&archive, &out, &opts)?;

    assert!(out.join("tree/hello.txt").exists());
    assert!(out.join("tree/subdir/nested.txt").exists());
    assert!(
        !out.join("tree/notes.log").exists(),
        "notes.log should be excluded"
    );
    assert!(
        !out.join("tree/subdir/debug.log").exists(),
        "debug.log should be excluded"
    );
    Ok(())
}

// ── --strip-components ───────────────────────────────────────────────────────

#[test]
fn tar_gz_strip_components_one() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let archive = tmp.join("archive.tar.gz");
    (TAR_GZ.compress)(std::slice::from_ref(&tree), &archive, &compress_opts(&[]))?;

    // Strip 1 component: "tree/hello.txt" → "hello.txt"
    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let opts = decompress_opts(1, &[]);
    (TAR_GZ.decompress)(&archive, &out, &opts)?;

    assert!(
        out.join("hello.txt").exists(),
        "hello.txt should be at top level"
    );
    assert!(
        out.join("subdir/nested.txt").exists(),
        "nested.txt should be under subdir/"
    );
    // The original top-level "tree" directory entry should have been stripped away
    assert!(!out.join("tree").exists(), "tree/ wrapper should not exist");
    Ok(())
}

#[test]
fn zip_strip_components_one() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let archive = tmp.join("archive.zip");
    (ZIP.compress)(std::slice::from_ref(&tree), &archive, &compress_opts(&[]))?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let opts = decompress_opts(1, &[]);
    (ZIP.decompress)(&archive, &out, &opts)?;

    assert!(
        out.join("hello.txt").exists(),
        "hello.txt should be at top level"
    );
    assert!(
        out.join("subdir/nested.txt").exists(),
        "nested.txt should be under subdir/"
    );
    assert!(!out.join("tree").exists(), "tree/ wrapper should not exist");
    Ok(())
}

#[test]
fn tar_gz_strip_and_exclude_combined() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_extended_tree(&tree)?;

    let archive = tmp.join("archive.tar.gz");
    (TAR_GZ.compress)(std::slice::from_ref(&tree), &archive, &compress_opts(&[]))?;

    // Strip the "tree" wrapper AND exclude .log files
    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let opts = decompress_opts(1, &["*.log"]);
    (TAR_GZ.decompress)(&archive, &out, &opts)?;

    assert!(out.join("hello.txt").exists());
    assert!(out.join("subdir/nested.txt").exists());
    assert!(
        !out.join("notes.log").exists(),
        "notes.log should be excluded"
    );
    assert!(
        !out.join("subdir/debug.log").exists(),
        "debug.log should be excluded"
    );
    assert!(
        !out.join("tree").exists(),
        "tree/ wrapper should be stripped"
    );
    Ok(())
}
