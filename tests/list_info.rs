mod helpers;

use helpers::{SEVEN_Z, TAR, TAR_GZ, TAR_XZ, TAR_ZST, TestResult, ZIP};

// ── tar.gz ───────────────────────────────────────────────────────────────────

#[test]
fn tar_gz_list_contains_expected_entries() -> TestResult {
    TAR_GZ.list_contains_expected_entries()
}

#[test]
fn tar_gz_info_reports_correct_metadata() -> TestResult {
    TAR_GZ.info_reports_correct_metadata()
}

// ── tar (plain) ──────────────────────────────────────────────────────────────

#[test]
fn tar_list_contains_expected_entries() -> TestResult {
    TAR.list_contains_expected_entries()
}

#[test]
fn tar_info_reports_correct_metadata() -> TestResult {
    TAR.info_reports_correct_metadata()
}

// ── tar.zst ──────────────────────────────────────────────────────────────────

#[test]
fn tar_zst_list_contains_expected_entries() -> TestResult {
    TAR_ZST.list_contains_expected_entries()
}

#[test]
fn tar_zst_info_reports_correct_metadata() -> TestResult {
    TAR_ZST.info_reports_correct_metadata()
}

// ── tar.xz ──────────────────────────────────────────────────────────────────

#[test]
fn tar_xz_list_contains_expected_entries() -> TestResult {
    TAR_XZ.list_contains_expected_entries()
}

#[test]
fn tar_xz_info_reports_correct_metadata() -> TestResult {
    TAR_XZ.info_reports_correct_metadata()
}

// ── tar.bz2 (requires `bzip2` feature) ───────────────────────────────────────

#[cfg(feature = "bzip2")]
use helpers::TAR_BZ2;

#[test]
#[cfg(feature = "bzip2")]
fn tar_bz2_list_contains_expected_entries() -> TestResult {
    TAR_BZ2.list_contains_expected_entries()
}

#[test]
#[cfg(feature = "bzip2")]
fn tar_bz2_info_reports_correct_metadata() -> TestResult {
    TAR_BZ2.info_reports_correct_metadata()
}

// ── zip ──────────────────────────────────────────────────────────────────────

#[test]
fn zip_list_contains_expected_entries() -> TestResult {
    ZIP.list_contains_expected_entries()
}

#[test]
fn zip_info_reports_correct_metadata() -> TestResult {
    ZIP.info_reports_correct_metadata()
}

// ── 7z ───────────────────────────────────────────────────────────────────────

#[test]
fn seven_z_list_contains_expected_entries() -> TestResult {
    SEVEN_Z.list_contains_expected_entries()
}

#[test]
fn seven_z_info_reports_correct_metadata() -> TestResult {
    SEVEN_Z.info_reports_correct_metadata()
}
