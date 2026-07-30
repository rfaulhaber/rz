//! Zip extraction runs entries in parallel, but flattening (`--no-directory`),
//! `--strip-components` and rename rules routinely collapse several entries
//! onto a single destination.  Those entries have to be applied one at a time
//! in archive order, or two workers truncate and write the same file at once
//! and the survivor is an interleaving of both — and the existence check that
//! drives `--force` / `--no-overwrite` / `--backup` races the write.

mod helpers;

use std::process::{Command, Output};

use camino::Utf8Path;
use helpers::{TestResult, temp_utf8_dir};

/// Big enough that two concurrent writers overlap for long enough to observe
/// the interleaving; small enough to keep the suite quick.
const PAYLOAD_LEN: usize = 8 * 1024 * 1024;

const REPEATS: usize = 5;

fn rz(args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_rz")).args(args).output()
}

/// Build `tree/d1/a.txt` and `tree/d2/a.txt` — same basename, different fill
/// byte — and zip them up with the real binary.
fn build_colliding_zip(tmp: &Utf8Path) -> Result<camino::Utf8PathBuf, Box<dyn std::error::Error>> {
    let tree = tmp.join("tree");
    fs_err::create_dir_all(tree.join("d1"))?;
    fs_err::create_dir_all(tree.join("d2"))?;
    fs_err::write(tree.join("d1/a.txt"), vec![b'A'; PAYLOAD_LEN])?;
    fs_err::write(tree.join("d2/a.txt"), vec![b'B'; PAYLOAD_LEN])?;

    let archive = tmp.join("collide.zip");
    let out = rz(&["compress", "-o", archive.as_str(), tree.as_str()])?;
    assert!(
        out.status.success(),
        "compress failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    Ok(archive)
}

/// Fill byte of the *last* `a.txt` entry in central-directory order — the one a
/// serial extractor would leave on disk.
fn last_colliding_fill(archive: &Utf8Path) -> Result<u8, Box<dyn std::error::Error>> {
    let file = fs_err::File::open(archive)?;
    let mut zip = ::zip::ZipArchive::new(file)?;
    let mut fill = None;
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i)?;
        if entry.name().ends_with("/a.txt") {
            fill = Some(if entry.name().contains("/d1/") {
                b'A'
            } else {
                b'B'
            });
        }
    }
    Ok(fill.ok_or("archive has no a.txt entries")?)
}

fn describe(bytes: &[u8]) -> String {
    let a = bytes.iter().filter(|b| **b == b'A').count();
    let b = bytes.iter().filter(|b| **b == b'B').count();
    format!("len={}, {a} 'A' bytes, {b} 'B' bytes", bytes.len())
}

#[test]
fn flattened_collision_extracts_last_entry_intact() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let archive = build_colliding_zip(&tmp)?;
    let expected = vec![last_colliding_fill(&archive)?; PAYLOAD_LEN];

    for run in 0..REPEATS {
        let out = tmp.join(format!("out{run}"));
        fs_err::create_dir(&out)?;

        let result = rz(&[
            "decompress",
            archive.as_str(),
            "-o",
            out.as_str(),
            "--no-directory",
            "--force",
            "--threads",
            "8",
        ])?;
        assert!(
            result.status.success(),
            "run {run}: decompress failed: {}",
            String::from_utf8_lossy(&result.stderr),
        );

        let extracted = fs_err::read(out.join("a.txt"))?;
        assert!(
            extracted == expected,
            "run {run}: collided output is not the last entry verbatim ({})",
            describe(&extracted),
        );
    }
    Ok(())
}

#[test]
fn flattened_collision_without_force_reports_existing_file() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let archive = build_colliding_zip(&tmp)?;

    for run in 0..REPEATS {
        let out = tmp.join(format!("out{run}"));
        fs_err::create_dir(&out)?;

        // The first entry creates `a.txt`; the second must then see it and
        // refuse, exactly as a serial run would.
        let result = rz(&[
            "decompress",
            archive.as_str(),
            "-o",
            out.as_str(),
            "--no-directory",
            "--threads",
            "8",
        ])?;
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            !result.status.success(),
            "run {run}: expected failure, got success",
        );
        assert!(
            stderr.contains("already exists"),
            "run {run}: unexpected error: {stderr}",
        );
    }
    Ok(())
}

#[test]
fn flattened_collision_respects_overwrite_guards() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let archive = build_colliding_zip(&tmp)?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let target = out.join("a.txt");
    fs_err::write(&target, b"pre-existing\n")?;

    // Pre-existing destination, no --force: the very first colliding entry
    // must abort the run.
    let result = rz(&[
        "decompress",
        archive.as_str(),
        "-o",
        out.as_str(),
        "--no-directory",
        "--threads",
        "8",
    ])?;
    assert!(!result.status.success(), "expected failure, got success");
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("already exists"),
        "unexpected error: {}",
        String::from_utf8_lossy(&result.stderr),
    );
    assert_eq!(fs_err::read(&target)?, b"pre-existing\n");

    // --no-overwrite: both entries skip, the file is left alone.
    let result = rz(&[
        "decompress",
        archive.as_str(),
        "-o",
        out.as_str(),
        "--no-directory",
        "--no-overwrite",
        "--threads",
        "8",
    ])?;
    assert!(
        result.status.success(),
        "--no-overwrite run failed: {}",
        String::from_utf8_lossy(&result.stderr),
    );
    assert_eq!(fs_err::read(&target)?, b"pre-existing\n");
    Ok(())
}
