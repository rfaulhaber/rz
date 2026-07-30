//! `plan_destinations` groups zip entries by their resolved destination so
//! that entries collapsing onto the same output path extract serially, in
//! archive order — never two rayon workers racing the same file. Before the
//! fix the group key was the *raw* rewritten path, not its canonical form:
//!
//! - `./f.bin` and `f.bin` are different `Utf8PathBuf`s (`Ord`/`Eq` compare
//!   components, and a leading `./` survives as a real `CurDir` component),
//!   so the two names landed in separate groups even though `output.join()`
//!   sends both to the same file on disk — two workers could truncate and
//!   write it at once.
//! - `d/` and `d` compare *equal* as paths (components are identical once the
//!   trailing separator is parsed away), so a directory entry and a file
//!   entry already shared one group — but the group's stored key was
//!   whichever name got inserted first. A file entry reached through the key
//!   `d/` hits `File::create("out/d/")`, which Linux always refuses for a
//!   regular file regardless of what exists on disk.
//!
//! `rz compress -o a.zip .` itself produces `./`-prefixed names, so this is a
//! real-world shape — it just needs hand-built archives here to pin down the
//! exact byte-for-byte spelling instead of depending on `rz` to reproduce it.

mod helpers;

use std::io::Write;
use std::process::{Command, Output};

use camino::Utf8Path;
use helpers::{TestResult, temp_utf8_dir};
use zip::write::SimpleFileOptions;

/// Big enough that two concurrent writers overlap for long enough to observe
/// the interleaving; small enough to keep the suite quick.
const PAYLOAD_LEN: usize = 8 * 1024 * 1024;

const REPEATS: usize = 5;

fn rz(args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_rz")).args(args).output()
}

/// Write a zip whose entries are `./f.bin` (filled with `'A'`) then `f.bin`
/// (filled with `'B'`) — the same file reached through two `Utf8PathBuf`
/// spellings that a component-wise `Ord`/`Eq` treats as distinct.
fn build_cur_dir_collision_zip(path: &Utf8Path) -> TestResult {
    let file = fs_err::File::create(path)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    writer.start_file("./f.bin", options)?;
    writer.write_all(&vec![b'A'; PAYLOAD_LEN])?;

    writer.start_file("f.bin", options)?;
    writer.write_all(&vec![b'B'; PAYLOAD_LEN])?;

    writer.finish()?;
    Ok(())
}

/// Write a zip whose entries are a directory `d/` then a file `d` — the same
/// destination string once the trailing separator is parsed away, one
/// spelling naming a directory and the other a file.
fn build_dir_file_collision_zip(path: &Utf8Path) -> TestResult {
    let file = fs_err::File::create(path)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    writer.add_directory("d/", options)?;
    writer.start_file("d", options)?;
    writer.write_all(b"payload")?;

    writer.finish()?;
    Ok(())
}

fn describe(bytes: &[u8]) -> String {
    let a = bytes.iter().filter(|b| **b == b'A').count();
    let b = bytes.iter().filter(|b| **b == b'B').count();
    format!("len={}, {a} 'A' bytes, {b} 'B' bytes", bytes.len())
}

#[test]
fn cur_dir_prefixed_collision_extracts_last_entry_intact() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("collide.zip");
    build_cur_dir_collision_zip(&archive)?;
    let expected = vec![b'B'; PAYLOAD_LEN];

    for run in 0..REPEATS {
        let out = tmp.join(format!("out{run}"));
        fs_err::create_dir(&out)?;

        let result = rz(&[
            "decompress",
            archive.as_str(),
            "-o",
            out.as_str(),
            "--force",
            "--threads",
            "8",
        ])?;
        assert!(
            result.status.success(),
            "run {run}: decompress failed: {}",
            String::from_utf8_lossy(&result.stderr),
        );

        let extracted = fs_err::read(out.join("f.bin"))?;
        assert!(
            extracted == expected,
            "run {run}: collided output is not the later ('f.bin') entry verbatim ({})",
            describe(&extracted),
        );
    }
    Ok(())
}

#[test]
fn cur_dir_prefixed_collision_without_force_reports_existing_file() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("collide.zip");
    build_cur_dir_collision_zip(&archive)?;

    for run in 0..REPEATS {
        let out = tmp.join(format!("out{run}"));
        fs_err::create_dir(&out)?;

        // The first entry (`./f.bin`) creates `f.bin`; the second (`f.bin`)
        // must then see it and refuse, exactly as a serial run would.
        let result = rz(&[
            "decompress",
            archive.as_str(),
            "-o",
            out.as_str(),
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
fn dir_and_file_name_collision_errors_deterministically_with_force() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("collide.zip");
    build_dir_file_collision_zip(&archive)?;

    for run in 0..REPEATS {
        let out = tmp.join(format!("out{run}"));
        fs_err::create_dir(&out)?;

        let result = rz(&[
            "decompress",
            archive.as_str(),
            "-o",
            out.as_str(),
            "--force",
            "--threads",
            "8",
        ])?;
        assert!(
            !result.status.success(),
            "run {run}: a directory and a file cannot share one path, expected failure, got: {}",
            String::from_utf8_lossy(&result.stdout),
        );
    }
    Ok(())
}

#[test]
fn dir_and_file_name_collision_errors_deterministically_without_force() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let archive = tmp.join("collide.zip");
    build_dir_file_collision_zip(&archive)?;

    for run in 0..REPEATS {
        let out = tmp.join(format!("out{run}"));
        fs_err::create_dir(&out)?;

        let result = rz(&[
            "decompress",
            archive.as_str(),
            "-o",
            out.as_str(),
            "--threads",
            "8",
        ])?;
        assert!(
            !result.status.success(),
            "run {run}: a directory and a file cannot share one path, expected failure, got: {}",
            String::from_utf8_lossy(&result.stdout),
        );
    }
    Ok(())
}
