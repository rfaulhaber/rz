//! Hostile zip metadata must not steer allocation.
//!
//! `ZipFile::size()` returns the central directory's `uncompressed_size`
//! verbatim — the crate never validates it against the actual content, and a
//! ZIP64 entry can declare up to `u64::MAX`.  Sizing a buffer from it turns a
//! 152-byte archive into a failed allocation, and a failed allocation calls
//! `handle_alloc_error`, which aborts the process rather than unwinding: the
//! crate's own no-panic lints give no protection at all.

mod helpers;

use camino::Utf8Path;
use globset::GlobSet;
use helpers::{TestResult, temp_utf8_dir};
use rz_archive::error::Error;
use rz_archive::{DecompressOpts, zip};

/// Bitwise CRC-32 (IEEE), so the fixtures below carry a checksum the zip
/// reader accepts without pulling in another dependency.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Build a single-entry ZIP64 archive whose entry is marked as a Unix symlink.
///
/// `declared` is written into the ZIP64 extra field as the uncompressed size,
/// independently of how many bytes `data` actually holds — which is the whole
/// point: real archives cannot be trusted to describe themselves honestly.
fn build_symlink_zip64(path: &Utf8Path, data: &[u8], declared: u64) -> TestResult {
    const NAME: &[u8] = b"link";
    let crc = crc32(data);

    let mut extra = Vec::new();
    extra.extend_from_slice(&0x0001u16.to_le_bytes()); // ZIP64 extra field tag
    extra.extend_from_slice(&16u16.to_le_bytes()); // payload size
    extra.extend_from_slice(&declared.to_le_bytes()); // uncompressed size
    extra.extend_from_slice(&(data.len() as u64).to_le_bytes()); // compressed size

    let mut local = Vec::new();
    local.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
    local.extend_from_slice(&45u16.to_le_bytes()); // version needed: 4.5 (zip64)
    local.extend_from_slice(&0u16.to_le_bytes()); // flags
    local.extend_from_slice(&0u16.to_le_bytes()); // method: stored
    local.extend_from_slice(&0u16.to_le_bytes()); // mod time
    local.extend_from_slice(&0u16.to_le_bytes()); // mod date
    local.extend_from_slice(&crc.to_le_bytes());
    local.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // sizes live in the
    local.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // zip64 extra field
    local.extend_from_slice(&(NAME.len() as u16).to_le_bytes());
    local.extend_from_slice(&(extra.len() as u16).to_le_bytes());
    local.extend_from_slice(NAME);
    local.extend_from_slice(&extra);

    let cd_offset = local.len() + data.len();

    let mut central = Vec::new();
    central.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
    central.extend_from_slice(&((3u16 << 8) | 45).to_le_bytes()); // made by: unix
    central.extend_from_slice(&45u16.to_le_bytes());
    central.extend_from_slice(&0u16.to_le_bytes()); // flags
    central.extend_from_slice(&0u16.to_le_bytes()); // method: stored
    central.extend_from_slice(&0u16.to_le_bytes()); // mod time
    central.extend_from_slice(&0u16.to_le_bytes()); // mod date
    central.extend_from_slice(&crc.to_le_bytes());
    central.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    central.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    central.extend_from_slice(&(NAME.len() as u16).to_le_bytes());
    central.extend_from_slice(&(extra.len() as u16).to_le_bytes());
    central.extend_from_slice(&0u16.to_le_bytes()); // comment length
    central.extend_from_slice(&0u16.to_le_bytes()); // disk number start
    central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
    // external attributes: S_IFLNK | 0777 in the high 16 bits.
    central.extend_from_slice(&((0o120777u32) << 16).to_le_bytes());
    central.extend_from_slice(&0u32.to_le_bytes()); // local header offset
    central.extend_from_slice(NAME);
    central.extend_from_slice(&extra);

    let mut eocd = Vec::new();
    eocd.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes()); // this disk
    eocd.extend_from_slice(&0u16.to_le_bytes()); // disk with central directory
    eocd.extend_from_slice(&1u16.to_le_bytes()); // entries on this disk
    eocd.extend_from_slice(&1u16.to_le_bytes()); // entries total
    eocd.extend_from_slice(&(central.len() as u32).to_le_bytes());
    eocd.extend_from_slice(&(cd_offset as u32).to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes()); // comment length

    let mut blob = local;
    blob.extend_from_slice(data);
    blob.extend_from_slice(&central);
    blob.extend_from_slice(&eocd);
    fs_err::write(path, &blob)?;
    Ok(())
}

fn force_opts() -> DecompressOpts<'static> {
    DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty())
}

/// A symlink entry declaring ~72 PB with six bytes behind it must extract
/// normally.  Before the fix this aborted the process outright (SIGABRT), which
/// no test could have caught as a failure — the whole test binary went down.
#[cfg(unix)]
#[test]
fn zip64_symlink_lying_about_its_size_does_not_abort() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let archive = tmp.join("evil.zip");
    build_symlink_zip64(&archive, b"target", 0x00FF_FFFF_FFFF_FFFF)?;

    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;
    zip::decompress(&archive, &out, &force_opts())?;

    let link = out.join("link");
    let meta = fs_err::symlink_metadata(&link)?;
    assert!(meta.file_type().is_symlink(), "expected a symlink at {link}");
    assert_eq!(fs_err::read_link(&link)?.to_string_lossy(), "target");
    Ok(())
}

/// A target longer than any real path is refused rather than read in full.
#[cfg(unix)]
#[test]
fn oversized_symlink_target_is_rejected() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let huge = vec![b'a'; 9000];
    let archive = tmp.join("huge.zip");
    build_symlink_zip64(&archive, &huge, huge.len() as u64)?;

    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;
    let result = zip::decompress(&archive, &out, &force_opts());

    assert!(
        matches!(result, Err(Error::SymlinkTargetTooLong { .. })),
        "oversized symlink target should be refused, got {result:?}",
    );
    Ok(())
}
