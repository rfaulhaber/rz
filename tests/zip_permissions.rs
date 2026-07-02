//! Zip compress must store on-disk Unix permission bits (notably the executable
//! bit) instead of the crate default `0o644`.
//!
//! Regression guard: `zip::compress` previously never called
//! `.unix_permissions()`, so executable files came back non-executable.

#![cfg(unix)]

mod helpers;

use std::os::unix::fs::PermissionsExt;

use globset::GlobSet;
use helpers::{TestResult, temp_utf8_dir};
use rz_archive::{CompressOpts, DecompressOpts, zip};

#[test]
fn zip_stores_and_restores_executable_bit() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;

    let script = tmp.join("run.sh");
    fs_err::write(&script, b"#!/bin/sh\necho hi\n")?;
    fs_err::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;

    let archive = tmp.join("a.zip");
    zip::compress(
        std::slice::from_ref(&script),
        &archive,
        &CompressOpts::new(None, GlobSet::empty()),
    )?;

    // The central-directory entry must carry 0o755, not the crate default 0o644.
    {
        let file = fs_err::File::open(&archive)?;
        let mut z = ::zip::ZipArchive::new(file)?;
        let entry = z.by_index(0)?;
        let mode = entry.unix_mode().ok_or("zip entry has no unix mode")?;
        assert_eq!(
            mode & 0o777,
            0o755,
            "stored mode should be 0o755, got {:o}",
            mode & 0o777,
        );
    }

    // A round trip with --preserve-permissions must restore +x on disk.
    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let opts = DecompressOpts {
        preserve_permissions: true,
        ..DecompressOpts::new(false, 0, GlobSet::empty(), GlobSet::empty())
    };
    zip::decompress(&archive, &out, &opts)?;

    let extracted_mode = fs_err::metadata(out.join("run.sh"))?.permissions().mode();
    assert_eq!(
        extracted_mode & 0o111,
        0o111,
        "extracted file should be executable, mode={:o}",
        extracted_mode & 0o777,
    );
    Ok(())
}

#[test]
fn zip_stores_mode_for_nested_files() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;
    let script = tree.join("nested.sh");
    fs_err::write(&script, b"#!/bin/sh\n")?;
    fs_err::set_permissions(&script, std::fs::Permissions::from_mode(0o750))?;
    // A plain-mode sibling to confirm non-executable files keep their bits too.
    let data = tree.join("data.txt");
    fs_err::write(&data, b"data\n")?;
    fs_err::set_permissions(&data, std::fs::Permissions::from_mode(0o640))?;

    let archive = tmp.join("a.zip");
    zip::compress(
        std::slice::from_ref(&tree),
        &archive,
        &CompressOpts::new(None, GlobSet::empty()),
    )?;

    let file = fs_err::File::open(&archive)?;
    let mut z = ::zip::ZipArchive::new(file)?;
    let mut seen = std::collections::HashMap::new();
    for i in 0..z.len() {
        let e = z.by_index(i)?;
        if let Some(mode) = e.unix_mode() {
            seen.insert(e.name().to_owned(), mode & 0o777);
        }
    }
    let nested = seen
        .iter()
        .find(|(k, _)| k.ends_with("nested.sh"))
        .map(|(_, v)| *v)
        .ok_or("nested.sh not found in archive")?;
    let data_mode = seen
        .iter()
        .find(|(k, _)| k.ends_with("data.txt"))
        .map(|(_, v)| *v)
        .ok_or("data.txt not found in archive")?;
    assert_eq!(nested, 0o750, "nested.sh mode; got {nested:o}");
    assert_eq!(data_mode, 0o640, "data.txt mode; got {data_mode:o}");
    Ok(())
}
