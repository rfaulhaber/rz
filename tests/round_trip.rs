mod helpers;

use helpers::{TestResult, SEVEN_Z, TAR, TAR_GZ, TAR_XZ, TAR_ZST, ZIP};

// ── tar.gz ───────────────────────────────────────────────────────────────────

#[test]
fn tar_gz_round_trip_directory() -> TestResult {
    TAR_GZ.round_trip_directory(None)
}

#[test]
fn tar_gz_round_trip_single_file() -> TestResult {
    TAR_GZ.round_trip_single_file()
}

#[test]
fn tar_gz_round_trip_custom_level() -> TestResult {
    TAR_GZ.round_trip_directory(Some(9))
}

// ── tar (plain) ──────────────────────────────────────────────────────────────

#[test]
fn tar_round_trip_directory() -> TestResult {
    TAR.round_trip_directory(None)
}

#[test]
fn tar_round_trip_single_file() -> TestResult {
    TAR.round_trip_single_file()
}

// ── tar.zst ──────────────────────────────────────────────────────────────────

#[test]
fn tar_zst_round_trip_directory() -> TestResult {
    TAR_ZST.round_trip_directory(None)
}

#[test]
fn tar_zst_round_trip_single_file() -> TestResult {
    TAR_ZST.round_trip_single_file()
}

// ── tar.xz ──────────────────────────────────────────────────────────────────

#[test]
fn tar_xz_round_trip_directory() -> TestResult {
    TAR_XZ.round_trip_directory(None)
}

#[test]
fn tar_xz_round_trip_single_file() -> TestResult {
    TAR_XZ.round_trip_single_file()
}

// ── tar.bz2 (requires `bzip2` feature) ───────────────────────────────────────

#[cfg(feature = "bzip2")]
use helpers::TAR_BZ2;

#[test]
#[cfg(feature = "bzip2")]
fn tar_bz2_round_trip_directory() -> TestResult {
    TAR_BZ2.round_trip_directory(None)
}

#[test]
#[cfg(feature = "bzip2")]
fn tar_bz2_round_trip_single_file() -> TestResult {
    TAR_BZ2.round_trip_single_file()
}

// ── zip ──────────────────────────────────────────────────────────────────────

#[test]
fn zip_round_trip_directory() -> TestResult {
    ZIP.round_trip_directory(None)
}

#[test]
fn zip_round_trip_single_file() -> TestResult {
    ZIP.round_trip_single_file()
}

// ── 7z ───────────────────────────────────────────────────────────────────────

#[test]
fn seven_z_round_trip_directory() -> TestResult {
    SEVEN_Z.round_trip_directory(None)
}
