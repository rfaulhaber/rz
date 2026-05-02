//! Pre-validation of compress inputs.
//!
//! These tests pin down the contract from the input-validation work:
//!   - A missing top-level input aborts compress *before* the output file is
//!     created (no empty 22-byte zips left on disk).
//!   - `--ignore-failed-read` (`opts.ignore_failed_read`) demotes individual
//!     missing inputs to warnings and continues with the rest.
//!   - When *every* input is missing or excluded, compress refuses to write
//!     an empty archive and returns `NoReadableInputs`.
//!
//! Each behaviour is checked against every backend so a regression in any one
//! format won't slip through.

mod helpers;

use camino::Utf8PathBuf;
use globset::GlobSet;

use helpers::{FormatHarness, SEVEN_Z, TAR, TAR_GZ, TAR_XZ, TAR_ZST, TestResult, ZIP, temp_utf8_dir};
use rz::CompressOpts;
use rz::error::Error;

#[cfg(feature = "bzip2")]
use helpers::TAR_BZ2;

/// Same as `helpers::default_compress_opts` but exposes `ignore_failed_read`.
fn opts_with_ignore(ignore: bool) -> CompressOpts<'static> {
    let mut opts = CompressOpts::new(None, GlobSet::empty());
    opts.ignore_failed_read = ignore;
    opts
}

// ── Default behaviour: missing input aborts before File::create ─────────────

fn missing_input_errors_without_creating_archive(harness: &FormatHarness) -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let missing = tmp.join("does-not-exist");
    let archive = tmp.join(format!("archive{}", harness.ext));

    let result = (harness.compress)(
        std::slice::from_ref(&missing),
        &archive,
        &opts_with_ignore(false),
    );

    let path_match = matches!(&result, Err(Error::CannotReadInput { path, .. }) if path == &missing);
    assert!(
        path_match,
        "{}: expected CannotReadInput for {missing}, got {result:?}",
        harness.format_name,
    );

    assert!(
        !archive.exists(),
        "{}: archive file should NOT have been created on validation failure (path: {archive})",
        harness.format_name,
    );
    Ok(())
}

#[test]
fn zip_missing_input_aborts_cleanly() -> TestResult {
    missing_input_errors_without_creating_archive(&ZIP)
}

#[test]
fn tar_missing_input_aborts_cleanly() -> TestResult {
    missing_input_errors_without_creating_archive(&TAR)
}

#[test]
fn tar_gz_missing_input_aborts_cleanly() -> TestResult {
    missing_input_errors_without_creating_archive(&TAR_GZ)
}

#[test]
fn tar_zst_missing_input_aborts_cleanly() -> TestResult {
    missing_input_errors_without_creating_archive(&TAR_ZST)
}

#[test]
fn tar_xz_missing_input_aborts_cleanly() -> TestResult {
    missing_input_errors_without_creating_archive(&TAR_XZ)
}

#[cfg(feature = "bzip2")]
#[test]
fn tar_bz2_missing_input_aborts_cleanly() -> TestResult {
    missing_input_errors_without_creating_archive(&TAR_BZ2)
}

#[test]
fn seven_z_missing_input_aborts_cleanly() -> TestResult {
    missing_input_errors_without_creating_archive(&SEVEN_Z)
}

// ── --ignore-failed-read: missing inputs are skipped, not fatal ─────────────

fn ignore_failed_read_skips_missing(harness: &FormatHarness) -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let good = tmp.join("good.txt");
    fs_err::write(&good, b"hello\n")?;
    let missing = tmp.join("missing");
    let archive = tmp.join(format!("archive{}", harness.ext));

    let inputs: Vec<Utf8PathBuf> = vec![missing, good];
    (harness.compress)(&inputs, &archive, &opts_with_ignore(true))?;

    assert!(archive.exists(), "{}: archive should exist", harness.format_name);
    let entries = (harness.list)(&archive)?;
    let has_good = entries.iter().any(|e| e.path.as_str().ends_with("good.txt"));
    assert!(
        has_good,
        "{}: archive should contain good.txt; got entries: {:?}",
        harness.format_name,
        entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
    );
    Ok(())
}

#[test]
fn zip_ignore_failed_read_skips_missing() -> TestResult {
    ignore_failed_read_skips_missing(&ZIP)
}

#[test]
fn tar_ignore_failed_read_skips_missing() -> TestResult {
    ignore_failed_read_skips_missing(&TAR)
}

#[test]
fn tar_gz_ignore_failed_read_skips_missing() -> TestResult {
    ignore_failed_read_skips_missing(&TAR_GZ)
}

#[test]
fn tar_zst_ignore_failed_read_skips_missing() -> TestResult {
    ignore_failed_read_skips_missing(&TAR_ZST)
}

#[test]
fn tar_xz_ignore_failed_read_skips_missing() -> TestResult {
    ignore_failed_read_skips_missing(&TAR_XZ)
}

#[cfg(feature = "bzip2")]
#[test]
fn tar_bz2_ignore_failed_read_skips_missing() -> TestResult {
    ignore_failed_read_skips_missing(&TAR_BZ2)
}

#[test]
fn seven_z_ignore_failed_read_skips_missing() -> TestResult {
    ignore_failed_read_skips_missing(&SEVEN_Z)
}

// ── All inputs missing: NoReadableInputs (refuse to create empty archive) ───

fn all_missing_returns_no_readable_inputs(harness: &FormatHarness) -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let missing_a = tmp.join("missing-a");
    let missing_b = tmp.join("missing-b");
    let archive = tmp.join(format!("archive{}", harness.ext));

    let inputs: Vec<Utf8PathBuf> = vec![missing_a, missing_b];
    let result = (harness.compress)(&inputs, &archive, &opts_with_ignore(true));

    assert!(
        matches!(result, Err(Error::NoReadableInputs)),
        "{}: expected NoReadableInputs, got {result:?}",
        harness.format_name,
    );
    assert!(
        !archive.exists(),
        "{}: should not have written an empty archive at {archive}",
        harness.format_name,
    );
    Ok(())
}

#[test]
fn zip_all_missing_no_readable_inputs() -> TestResult {
    all_missing_returns_no_readable_inputs(&ZIP)
}

#[test]
fn tar_all_missing_no_readable_inputs() -> TestResult {
    all_missing_returns_no_readable_inputs(&TAR)
}

#[test]
fn tar_gz_all_missing_no_readable_inputs() -> TestResult {
    all_missing_returns_no_readable_inputs(&TAR_GZ)
}

#[test]
fn tar_zst_all_missing_no_readable_inputs() -> TestResult {
    all_missing_returns_no_readable_inputs(&TAR_ZST)
}

#[test]
fn tar_xz_all_missing_no_readable_inputs() -> TestResult {
    all_missing_returns_no_readable_inputs(&TAR_XZ)
}

#[cfg(feature = "bzip2")]
#[test]
fn tar_bz2_all_missing_no_readable_inputs() -> TestResult {
    all_missing_returns_no_readable_inputs(&TAR_BZ2)
}

#[test]
fn seven_z_all_missing_no_readable_inputs() -> TestResult {
    all_missing_returns_no_readable_inputs(&SEVEN_Z)
}

// ── Error message wording: no more "metadata of symlink" leakage ────────────

#[test]
fn error_message_does_not_say_symlink_for_regular_directory() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let missing = tmp.join("does-not-exist");
    let archive = tmp.join("out.zip");
    let result = rz::zip::compress(
        std::slice::from_ref(&missing),
        &archive,
        &opts_with_ignore(false),
    );
    // Render the error message, defaulting to "<ok>" if compress somehow
    // succeeded — the assertions below catch both wording regressions and
    // the missing-error case in one place.
    let msg = result.err().map(|e| e.to_string()).unwrap_or_else(|| "<ok>".to_owned());
    assert!(
        !msg.contains("symlink"),
        "error wording should not mention 'symlink' for a missing path: {msg}"
    );
    assert!(
        msg.contains("cannot read input"),
        "error should be the new CannotReadInput variant: {msg}"
    );
    Ok(())
}
