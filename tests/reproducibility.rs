mod helpers;

use std::io::BufReader;

use globset::GlobSet;
use helpers::{TestResult, temp_utf8_dir};

use rz::CompressOpts;

fn find_tar_header(
    archive: &camino::Utf8Path,
    name: &str,
) -> Result<tar::Header, Box<dyn std::error::Error>> {
    let file = fs_err::File::open(archive)?;
    let mut archive = tar::Archive::new(BufReader::new(file));
    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        if path.to_string_lossy().ends_with(name) {
            return Ok(entry.header().clone());
        }
    }
    Err(format!("entry {name} not found in archive").into())
}

#[test]
fn mode_override_applies_to_file_entries() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;
    fs_err::write(tree.join("a.txt"), b"hi\n")?;

    let archive = tmp.join("out.tar");
    let mut opts = CompressOpts::new(None, GlobSet::empty());
    opts.fixed_mode = Some(0o600);
    rz::tar::compress(std::slice::from_ref(&tree), &archive, &opts)?;

    let header = find_tar_header(&archive, "a.txt")?;
    assert_eq!(header.mode()? & 0o7777, 0o600);
    Ok(())
}

#[test]
fn mtime_owner_group_mode_overrides_combine() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;
    fs_err::write(tree.join("f.txt"), b"data\n")?;

    let archive = tmp.join("out.tar");
    let mut opts = CompressOpts::new(None, GlobSet::empty());
    opts.fixed_mtime = Some(1_000_000);
    opts.fixed_uid = Some(42);
    opts.fixed_gid = Some(43);
    opts.fixed_mode = Some(0o640);
    rz::tar::compress(std::slice::from_ref(&tree), &archive, &opts)?;

    let header = find_tar_header(&archive, "f.txt")?;
    assert_eq!(header.mtime()?, 1_000_000);
    assert_eq!(header.uid()?, 42);
    assert_eq!(header.gid()?, 43);
    assert_eq!(header.mode()? & 0o7777, 0o640);
    Ok(())
}

#[test]
fn cli_rejects_mode_on_zip() -> TestResult {
    // Exec the binary to exercise the CLI-layer reject path.  If the binary
    // lives in CARGO_BIN_EXE_rz, cargo's test harness provides the path.
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
            "--mode",
            "644",
        ])
        .output()?;
    assert!(!out.status.success(), "expected rejection on zip + --mode");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--mode"),
        "stderr should mention --mode: {stderr}"
    );
    Ok(())
}

#[test]
fn cli_accepts_octal_mode_forms() -> TestResult {
    // `644`, `0644`, `0o644` must all parse to the same mode.  Exercised by
    // compressing three times with each spelling and comparing header bits.
    let bin = env!("CARGO_BIN_EXE_rz");
    let (_guard, tmp) = temp_utf8_dir()?;
    let file = tmp.join("x.txt");
    fs_err::write(&file, b"x")?;

    for spelling in ["644", "0644", "0o644"] {
        let archive = tmp.join(format!("out-{spelling}.tar"));
        let out = std::process::Command::new(bin)
            .args([
                "compress",
                file.as_str(),
                "-o",
                archive.as_str(),
                "--mode",
                spelling,
            ])
            .output()?;
        assert!(
            out.status.success(),
            "--mode {spelling} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let header = find_tar_header(&archive, "x.txt")?;
        assert_eq!(
            header.mode()? & 0o7777,
            0o644,
            "spelling {spelling} did not produce 0o644",
        );
    }
    Ok(())
}

#[test]
fn cli_rejects_same_owner_on_zip() -> TestResult {
    let bin = env!("CARGO_BIN_EXE_rz");
    let (_guard, tmp) = temp_utf8_dir()?;
    let file = tmp.join("x.txt");
    fs_err::write(&file, b"x")?;
    let archive = tmp.join("out.zip");

    // First build a real zip archive to decompress against.
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
            "--same-owner",
        ])
        .output()?;
    assert!(
        !out.status.success(),
        "expected rejection on zip + --same-owner"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--same-owner"),
        "stderr should mention --same-owner: {stderr}"
    );
    Ok(())
}
