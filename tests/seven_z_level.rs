//! `--level` / `--store` handling for the 7z backend.
//!
//! Regression guard: `seven_z::compress` previously ignored `opts.level`
//! entirely, so `-l 0` / `--store` still LZMA2-compressed and `-l N` was a
//! no-op.  These tests pin that the level is honoured.

mod helpers;

use camino::{Utf8Path, Utf8PathBuf};
use globset::GlobSet;
use helpers::{TestResult, temp_utf8_dir};
use rz_archive::{CompressOpts, DecompressOpts, seven_z};

fn compress_opts(level: Option<u32>) -> CompressOpts<'static> {
    CompressOpts::new(level, GlobSet::empty())
}

/// Force-overwrite decompress opts (7z needs `force` for its fast path).
fn decompress_opts() -> DecompressOpts<'static> {
    DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty())
}

/// Write a highly compressible payload large enough that LZMA2 clearly beats
/// storing it verbatim.
fn write_payload(dir: &Utf8Path) -> std::io::Result<Utf8PathBuf> {
    let p = dir.join("payload.txt");
    let body = "the quick brown fox jumps over the lazy dog\n".repeat(4000); // ~180 KB
    fs_err::write(&p, body.as_bytes())?;
    Ok(p)
}

#[test]
fn seven_z_store_is_much_larger_than_compressed() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let payload = write_payload(&tmp)?;

    // Level 0 == --store: COPY (no compression).
    let stored = tmp.join("stored.7z");
    seven_z::compress(std::slice::from_ref(&payload), &stored, &compress_opts(Some(0)))?;

    // Default: LZMA2.
    let compressed = tmp.join("compressed.7z");
    seven_z::compress(std::slice::from_ref(&payload), &compressed, &compress_opts(None))?;

    let stored_size = fs_err::metadata(&stored)?.len();
    let compressed_size = fs_err::metadata(&compressed)?.len();
    // Before the fix, level 0 was ignored and both archives were LZMA2 — nearly
    // the same size.  With COPY honoured, the stored archive is several times
    // larger than the compressed one.
    assert!(
        stored_size > compressed_size * 3,
        "level 0 (store) should be far larger than compressed: \
         stored={stored_size} compressed={compressed_size}",
    );
    Ok(())
}

/// Every level must produce a valid, round-trippable archive.
fn round_trips_at_level(level: Option<u32>) -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let payload = write_payload(&tmp)?;

    let archive = tmp.join("ar.7z");
    seven_z::compress(std::slice::from_ref(&payload), &archive, &compress_opts(level))?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    seven_z::decompress(&archive, &out, &decompress_opts())?;

    assert_eq!(fs_err::read(out.join("payload.txt"))?, fs_err::read(&payload)?);
    Ok(())
}

#[test]
fn seven_z_store_round_trips() -> TestResult {
    round_trips_at_level(Some(0))
}

#[test]
fn seven_z_level_1_round_trips() -> TestResult {
    round_trips_at_level(Some(1))
}

#[test]
fn seven_z_level_9_round_trips() -> TestResult {
    round_trips_at_level(Some(9))
}

/// An out-of-range level must clamp rather than error or panic (lzma-rust2
/// clamps presets to 9 internally).
#[test]
fn seven_z_out_of_range_level_clamps() -> TestResult {
    round_trips_at_level(Some(99))
}
