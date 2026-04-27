//! Integration tests for `append` / `update` / `remove`.
//!
//! Each test follows the same pattern: build a small archive via the format's
//! normal `compress` path, then exercise a `modify::*` operation, then list
//! or decompress to verify the resulting archive is well-formed and contains
//! exactly the entries we expect.

mod helpers;

use std::thread;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};

use helpers::{TestResult, default_compress_opts, default_decompress_opts, temp_utf8_dir};
use rz::cmd::Format;
use rz::error::Result as RzResult;
use rz::modify::{self, AppendMode};

// ── small helpers local to this file ─────────────────────────────────────────

/// Write `name` under `dir` with `body`, returning its absolute path.
fn write_file(dir: &Utf8Path, name: &str, body: &[u8]) -> std::io::Result<Utf8PathBuf> {
    let p = dir.join(name);
    fs_err::write(&p, body)?;
    Ok(p)
}

/// Sorted list of every entry name in `archive`, dispatched by format.
fn list_names(archive: &Utf8Path, fmt: Format) -> RzResult<Vec<String>> {
    let entries = match fmt {
        Format::Tar => rz::tar::list(archive)?,
        Format::TarGz => rz::tar_gz::list(archive)?,
        Format::TarZst => rz::tar_zst::list(archive)?,
        Format::TarXz => rz::tar_xz::list(archive)?,
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => rz::tar_bz2::list(archive)?,
        #[cfg(not(feature = "bzip2"))]
        Format::TarBz2 => return Err(rz::error::Error::UnsupportedFormat("tar.bz2".into())),
        Format::Zip => rz::zip::list(archive)?,
        Format::SevenZ => rz::seven_z::list(archive)?,
    };
    let mut names: Vec<String> = entries.iter().map(|e| e.path.to_string()).collect();
    names.sort();
    Ok(names)
}

fn compress(archive: &Utf8Path, fmt: Format, inputs: &[Utf8PathBuf]) -> RzResult<()> {
    let opts = default_compress_opts(None);
    match fmt {
        Format::Tar => rz::tar::compress(inputs, archive, &opts),
        Format::TarGz => rz::tar_gz::compress(inputs, archive, &opts),
        Format::TarZst => rz::tar_zst::compress(inputs, archive, &opts),
        Format::TarXz => rz::tar_xz::compress(inputs, archive, &opts),
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => rz::tar_bz2::compress(inputs, archive, &opts),
        #[cfg(not(feature = "bzip2"))]
        Format::TarBz2 => Err(rz::error::Error::UnsupportedFormat("tar.bz2".into())),
        Format::Zip => rz::zip::compress(inputs, archive, &opts),
        Format::SevenZ => rz::seven_z::compress(inputs, archive, &opts),
    }
}

fn decompress(archive: &Utf8Path, fmt: Format, out: &Utf8Path) -> RzResult<()> {
    let opts = default_decompress_opts();
    decompress_with(archive, fmt, out, &opts)
}

fn decompress_with(
    archive: &Utf8Path,
    fmt: Format,
    out: &Utf8Path,
    opts: &rz::DecompressOpts<'_>,
) -> RzResult<()> {
    match fmt {
        Format::Tar => rz::tar::decompress(archive, out, opts),
        Format::TarGz => rz::tar_gz::decompress(archive, out, opts),
        Format::TarZst => rz::tar_zst::decompress(archive, out, opts),
        Format::TarXz => rz::tar_xz::decompress(archive, out, opts),
        #[cfg(feature = "bzip2")]
        Format::TarBz2 => rz::tar_bz2::decompress(archive, out, opts),
        #[cfg(not(feature = "bzip2"))]
        Format::TarBz2 => Err(rz::error::Error::UnsupportedFormat("tar.bz2".into())),
        Format::Zip => rz::zip::decompress(archive, out, opts),
        Format::SevenZ => rz::seven_z::decompress(archive, out, opts),
    }
}

/// Append-mode test that runs against any modifiable format.
fn append_then_list(fmt: Format, ext: &str) -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let a = write_file(&tmp, "a.txt", b"alpha\n")?;
    let archive = tmp.join(format!("ar{ext}"));
    compress(&archive, fmt, std::slice::from_ref(&a))?;

    let b = write_file(&tmp, "b.txt", b"bravo\n")?;
    let opts = default_compress_opts(None);
    modify::append(&archive, fmt, std::slice::from_ref(&b), AppendMode::Append, &opts)?;

    let names = list_names(&archive, fmt)?;
    assert!(names.iter().any(|n| n.ends_with("a.txt")), "missing a.txt: {names:?}");
    assert!(names.iter().any(|n| n.ends_with("b.txt")), "missing b.txt: {names:?}");
    Ok(())
}

/// Append + decompress round trip — proves the resulting archive is valid
/// end-to-end (every byte of every original entry is still readable, plus the
/// new one).
fn append_then_decompress(fmt: Format, ext: &str, preserves_top_dir: bool) -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let a = write_file(&tmp, "a.txt", b"alpha\n")?;
    let archive = tmp.join(format!("ar{ext}"));
    compress(&archive, fmt, std::slice::from_ref(&a))?;

    let b = write_file(&tmp, "b.txt", b"bravo\n")?;
    let opts = default_compress_opts(None);
    modify::append(&archive, fmt, std::slice::from_ref(&b), AppendMode::Append, &opts)?;

    let out = tmp.join("out");
    if preserves_top_dir {
        fs_err::create_dir(&out)?;
    }
    decompress(&archive, fmt, &out)?;

    let a_out = fs_err::read(out.join("a.txt"))?;
    let b_out = fs_err::read(out.join("b.txt"))?;
    assert_eq!(a_out, b"alpha\n");
    assert_eq!(b_out, b"bravo\n");
    Ok(())
}

/// Update mode: re-appending an unchanged file should not duplicate it; a
/// strictly-newer mtime should write a fresh entry.
///
/// We bump mtime by sleeping > 1s and rewriting the file (cross-platform —
/// no `filetime` dep needed). Slow but reliable.
fn update_skips_unchanged_writes_newer(fmt: Format, ext: &str) -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let a = write_file(&tmp, "a.txt", b"alpha\n")?;
    let archive = tmp.join(format!("ar{ext}"));
    compress(&archive, fmt, std::slice::from_ref(&a))?;

    let baseline = list_names(&archive, fmt)?;
    let baseline_count = baseline.iter().filter(|n| n.ends_with("a.txt")).count();
    assert_eq!(baseline_count, 1, "baseline must have exactly one a.txt: {baseline:?}");

    let opts = default_compress_opts(None);

    // Update with no mtime change → must skip.
    modify::append(&archive, fmt, std::slice::from_ref(&a), AppendMode::Update, &opts)?;
    let after_skip = list_names(&archive, fmt)?;
    assert_eq!(
        after_skip.iter().filter(|n| n.ends_with("a.txt")).count(),
        baseline_count,
        "update with unchanged mtime should not append: {after_skip:?}",
    );

    // Bump mtime by waiting past the 1-second tar mtime resolution boundary
    // and rewriting. Now update should append.
    thread::sleep(Duration::from_millis(1100));
    fs_err::write(&a, b"alpha v2\n")?;
    modify::append(&archive, fmt, std::slice::from_ref(&a), AppendMode::Update, &opts)?;

    let after_update = list_names(&archive, fmt)?;
    let count_after = after_update.iter().filter(|n| n.ends_with("a.txt")).count();
    assert!(
        count_after > baseline_count,
        "update with newer mtime should add a fresh entry; \
         before={baseline_count} after={count_after} listing={after_update:?}",
    );

    // Decompressing the updated archive must yield the v2 contents.  Tar
    // archives legitimately contain both copies after an update; with
    // `force=true` the later (v2) entry overwrites the earlier one on extract,
    // matching standard `tar -x` behavior.
    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let dec_opts = rz::DecompressOpts {
        force: true,
        ..default_decompress_opts()
    };
    decompress_with(&archive, fmt, &out, &dec_opts)?;
    let extracted = fs_err::read(out.join("a.txt"))?;
    assert_eq!(extracted, b"alpha v2\n", "extracted contents must reflect the update");

    Ok(())
}

/// Remove drops a matching entry and leaves siblings intact.
fn remove_drops_matching(fmt: Format, ext: &str) -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let a = write_file(&tmp, "a.txt", b"alpha\n")?;
    let b = write_file(&tmp, "b.txt", b"bravo\n")?;
    let archive = tmp.join(format!("ar{ext}"));
    compress(&archive, fmt, &[a, b])?;

    modify::remove(&archive, fmt, &["a.txt".into()], None)?;

    let names = list_names(&archive, fmt)?;
    assert!(!names.iter().any(|n| n.ends_with("a.txt")), "a.txt should be gone: {names:?}");
    assert!(names.iter().any(|n| n.ends_with("b.txt")), "b.txt should remain: {names:?}");
    Ok(())
}

// ── format-by-format invocations ─────────────────────────────────────────────

#[test] fn tar_append_then_list() -> TestResult { append_then_list(Format::Tar, ".tar") }
#[test] fn tar_append_then_decompress() -> TestResult { append_then_decompress(Format::Tar, ".tar", true) }
#[test] fn tar_update_skips_unchanged_writes_newer() -> TestResult { update_skips_unchanged_writes_newer(Format::Tar, ".tar") }
#[test] fn tar_remove_drops_matching() -> TestResult { remove_drops_matching(Format::Tar, ".tar") }

#[test] fn tar_gz_append_then_list() -> TestResult { append_then_list(Format::TarGz, ".tar.gz") }
#[test] fn tar_gz_append_then_decompress() -> TestResult { append_then_decompress(Format::TarGz, ".tar.gz", true) }
#[test] fn tar_gz_remove_drops_matching() -> TestResult { remove_drops_matching(Format::TarGz, ".tar.gz") }

#[test] fn tar_zst_append_then_list() -> TestResult { append_then_list(Format::TarZst, ".tar.zst") }
#[test] fn tar_zst_append_then_decompress() -> TestResult { append_then_decompress(Format::TarZst, ".tar.zst", true) }
#[test] fn tar_zst_remove_drops_matching() -> TestResult { remove_drops_matching(Format::TarZst, ".tar.zst") }

#[test] fn tar_xz_append_then_list() -> TestResult { append_then_list(Format::TarXz, ".tar.xz") }
#[test] fn tar_xz_append_then_decompress() -> TestResult { append_then_decompress(Format::TarXz, ".tar.xz", true) }
#[test] fn tar_xz_remove_drops_matching() -> TestResult { remove_drops_matching(Format::TarXz, ".tar.xz") }

#[cfg(feature = "bzip2")]
#[test] fn tar_bz2_append_then_decompress() -> TestResult { append_then_decompress(Format::TarBz2, ".tar.bz2", true) }
#[cfg(feature = "bzip2")]
#[test] fn tar_bz2_remove_drops_matching() -> TestResult { remove_drops_matching(Format::TarBz2, ".tar.bz2") }

#[test] fn zip_append_then_list() -> TestResult { append_then_list(Format::Zip, ".zip") }
#[test] fn zip_append_then_decompress() -> TestResult { append_then_decompress(Format::Zip, ".zip", true) }
#[test] fn zip_remove_drops_matching() -> TestResult { remove_drops_matching(Format::Zip, ".zip") }

// ── error paths ──────────────────────────────────────────────────────────────

#[test]
fn seven_z_append_unsupported() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let a = write_file(&tmp, "a.txt", b"alpha\n")?;
    let archive = tmp.join("ar.7z");
    compress(&archive, Format::SevenZ, std::slice::from_ref(&a))?;

    let opts = default_compress_opts(None);
    let err = modify::append(&archive, Format::SevenZ, &[a], AppendMode::Append, &opts);
    assert!(matches!(err, Err(rz::error::Error::ModifyUnsupported { .. })));
    Ok(())
}

#[test]
fn seven_z_remove_unsupported() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let a = write_file(&tmp, "a.txt", b"alpha\n")?;
    let archive = tmp.join("ar.7z");
    compress(&archive, Format::SevenZ, &[a])?;

    let err = modify::remove(&archive, Format::SevenZ, &["a.txt".into()], None);
    assert!(matches!(err, Err(rz::error::Error::ModifyUnsupported { .. })));
    Ok(())
}

// ── tar in-place: ensure the tail bytes really land on a 512-byte boundary
//    and a fresh EOF terminator follows. This is the property that lets a
//    plain GNU tar reader re-open the archive without complaint.

#[test]
fn tar_append_archive_ends_with_eof_terminator() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let a = write_file(&tmp, "a.txt", b"alpha\n")?;
    let archive = tmp.join("ar.tar");
    compress(&archive, Format::Tar, std::slice::from_ref(&a))?;

    let b = write_file(&tmp, "b.txt", b"bravo\n")?;
    let opts = default_compress_opts(None);
    modify::append(&archive, Format::Tar, std::slice::from_ref(&b), AppendMode::Append, &opts)?;

    let bytes = fs_err::read(&archive)?;
    assert_eq!(bytes.len() % 512, 0, "tar archive must be a multiple of 512 bytes");
    let tail = &bytes[bytes.len().saturating_sub(1024)..];
    assert!(tail.iter().all(|b| *b == 0), "tar archive must end with two zero blocks");
    Ok(())
}
