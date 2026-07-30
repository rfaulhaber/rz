//! `rz compress ... -o -` must not exit 0 when the destination can't actually
//! accept the bytes. `/dev/full` always fails writes with `ENOSPC`, which
//! stands in for a full disk or a `> file` redirect that runs out of space.
//!
//! For small archives the compressed bytes fit entirely inside internal
//! buffers, so no write ever reaches the OS until the final flush — if that
//! flush's error is discarded (e.g. by running only in a dropped
//! `BufWriter`'s `Drop` impl), the process reports success despite delivering
//! nothing.

mod helpers;

use std::process::{Command, Stdio};

use helpers::{TestResult, temp_utf8_dir};

fn rz_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz")
}

/// Run `rz compress <tree> -f <fmt> -o -` with stdout wired directly to
/// `/dev/full`, and assert the process fails loudly instead of exiting 0.
///
/// A single small file (rather than `helpers::build_file_tree`'s multi-file
/// tree) keeps every format's compressed output small and consistent enough
/// to reliably stay inside internal buffers — a bigger or differently-shaped
/// payload can trigger an incidental early write that masks the bug,
/// independent of whether the fix is present.
#[cfg(target_os = "linux")]
fn assert_compress_to_full_device_fails(fmt: &str) -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;
    fs_err::write(tree.join("a.txt"), b"hello world\n")?;

    let full = fs_err::OpenOptions::new().write(true).open("/dev/full")?;
    let full: std::fs::File = full.into();

    let output = Command::new(rz_bin())
        .args(["compress", tree.as_str(), "-f", fmt, "-o", "-"])
        .stdout(Stdio::from(full))
        .stderr(Stdio::piped())
        .output()?;

    assert!(
        !output.status.success(),
        "compress -f {fmt} to a full device should fail, got exit {:?}",
        output.status.code(),
    );
    assert!(
        !output.stderr.is_empty(),
        "compress -f {fmt} to a full device should print a diagnostic on stderr",
    );
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn compress_tar_to_full_device_fails() -> TestResult {
    assert_compress_to_full_device_fails("tar")
}

#[test]
#[cfg(target_os = "linux")]
fn compress_tar_gz_to_full_device_fails() -> TestResult {
    assert_compress_to_full_device_fails("tar-gz")
}

#[test]
#[cfg(target_os = "linux")]
fn compress_tar_zst_to_full_device_fails() -> TestResult {
    assert_compress_to_full_device_fails("tar-zst")
}

#[test]
#[cfg(target_os = "linux")]
fn compress_tar_xz_to_full_device_fails() -> TestResult {
    assert_compress_to_full_device_fails("tar-xz")
}

#[test]
#[cfg(all(target_os = "linux", feature = "bzip2"))]
fn compress_tar_bz2_to_full_device_fails() -> TestResult {
    assert_compress_to_full_device_fails("tar-bz2")
}
