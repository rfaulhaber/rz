//! Zip compression-level handling: `--store` (and its `--level 0` spelling)
//! must select the `Stored` method rather than asking deflate for a level it
//! does not accept, and a compress run that dies partway must not leave the
//! half-written archive behind.

mod helpers;

use std::process::{Command, Output};

use camino::Utf8Path;
use helpers::{TestResult, temp_utf8_dir};

fn rz(args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_rz")).args(args).output()
}

/// Deliberately incompressible-looking payload so a stored archive is
/// distinguishable from a deflated one by size alone.
fn payload() -> Vec<u8> {
    (0..64u32 * 1024)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect()
}

fn assert_stored_round_trip(tmp: &Utf8Path, level_args: &[&str]) -> TestResult {
    let source = tmp.join("payload.bin");
    let bytes = payload();
    fs_err::write(&source, &bytes)?;

    let archive = tmp.join("out.zip");
    let mut args = vec!["compress", "-o", archive.as_str()];
    args.extend_from_slice(level_args);
    args.push(source.as_str());
    let result = rz(&args)?;
    assert!(
        result.status.success(),
        "compress {level_args:?} failed: {}",
        String::from_utf8_lossy(&result.stderr),
    );

    {
        let file = fs_err::File::open(&archive)?;
        let mut zip = ::zip::ZipArchive::new(file)?;
        let entry = zip.by_index_raw(0)?;
        assert_eq!(entry.name(), "payload.bin");
        assert_eq!(
            entry.compression(),
            ::zip::CompressionMethod::Stored,
            "{level_args:?} should store the entry uncompressed",
        );
    }

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let result = rz(&["decompress", archive.as_str(), "-o", out.as_str()])?;
    assert!(
        result.status.success(),
        "decompress failed: {}",
        String::from_utf8_lossy(&result.stderr),
    );
    assert_eq!(fs_err::read(out.join("payload.bin"))?, bytes);
    Ok(())
}

#[test]
fn zip_store_flag_produces_stored_entries() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    assert_stored_round_trip(&tmp, &["--store"])
}

#[test]
fn zip_level_zero_produces_stored_entries() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    assert_stored_round_trip(&tmp, &["--level", "0"])
}

#[cfg(unix)]
#[test]
fn zip_compress_failure_leaves_no_stub_archive() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let (_guard, tmp) = temp_utf8_dir()?;
    let source = tmp.join("locked.txt");
    fs_err::write(&source, b"secret\n")?;
    fs_err::set_permissions(&source, std::fs::Permissions::from_mode(0o000))?;

    // Running as root defeats the mode bits, so there is nothing to observe.
    if fs_err::File::open(&source).is_ok() {
        return Ok(());
    }

    let archive = tmp.join("out.zip");
    let result = rz(&["compress", "-o", archive.as_str(), source.as_str()])?;
    assert!(
        !result.status.success(),
        "compress of an unreadable input should fail",
    );
    assert!(!archive.exists(), "a failed compress left {archive} behind",);
    Ok(())
}
