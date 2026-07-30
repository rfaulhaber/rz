//! Extraction-path guards for the 7z backend.
//!
//! 7z extraction used to delegate to sevenz-rust2's own callback machinery,
//! which resolves each entry against the raw archive name.  That made the
//! overwrite guards unreachable, silently dropped `--prefix`/`--rename`, and
//! let a `..` entry write outside the output directory whenever `--force` was
//! set.  These tests pin the replacement behaviour.

mod helpers;

use camino::{Utf8Path, Utf8PathBuf};
use globset::GlobSet;
use helpers::{TestResult, temp_utf8_dir};
use rz_archive::error::Error;
use rz_archive::{CompressOpts, DecompressOpts, seven_z};

/// Decompress opts that refuse to clobber — the interesting default here.
fn opts() -> DecompressOpts<'static> {
    DecompressOpts::new(false, 0, GlobSet::empty(), GlobSet::empty())
}

/// Build an archive holding `src/foo.txt`, `src/top.txt` and `src/sub/n.txt`.
///
/// 7z names entries the same way tar and zip do — prefixed with the input
/// directory's own name.
fn build_archive(tmp: &Utf8Path) -> Result<Utf8PathBuf, Box<dyn std::error::Error>> {
    let src = tmp.join("src");
    fs_err::create_dir_all(src.join("sub"))?;
    fs_err::write(src.join("foo.txt"), b"foo-content")?;
    fs_err::write(src.join("top.txt"), b"top-content")?;
    fs_err::write(src.join("sub").join("n.txt"), b"nested")?;

    let archive = tmp.join("t.7z");
    seven_z::compress(
        std::slice::from_ref(&src),
        &archive,
        &CompressOpts::new(None, GlobSet::empty()),
    )?;
    Ok(archive)
}

/// Create `out/` with an existing `src/foo.txt` to collide with the archive.
fn out_with_existing(tmp: &Utf8Path) -> Result<Utf8PathBuf, Box<dyn std::error::Error>> {
    let out = tmp.join("out");
    fs_err::create_dir_all(out.join("src"))?;
    fs_err::write(out.join("src/foo.txt"), b"PRECIOUS")?;
    Ok(out)
}

/// A `..` entry must be refused even with `--force`.
///
/// sevenz-rust2 joins the raw entry name onto the output directory with no
/// filtering of its own, so nothing but this check stands between a hostile
/// archive and an arbitrary file write.
#[test]
fn traversal_entry_is_rejected_even_with_force() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;

    let archive = tmp.join("evil.7z");
    {
        let mut writer = sevenz_rust2::ArchiveWriter::create(&archive)?;
        writer.push_archive_entry(
            sevenz_rust2::ArchiveEntry::new_file("../../ESCAPED.txt"),
            Some(&b"pwned"[..]),
        )?;
        writer.finish()?;
    }

    // Two levels deep, so the entry resolves to `tmp/ESCAPED.txt`.
    let out = tmp.join("deep").join("out");
    fs_err::create_dir_all(&out)?;

    let mut o = opts();
    o.force = true;
    let result = seven_z::decompress(&archive, &out, &o);

    assert!(
        matches!(result, Err(Error::PathTraversal(_))),
        "must refuse a traversal entry, got {result:?}",
    );
    assert!(
        !tmp.join("ESCAPED.txt").exists(),
        "entry escaped the output directory",
    );
    Ok(())
}

#[test]
fn existing_file_is_not_overwritten_without_force() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let archive = build_archive(&tmp)?;
    let out = out_with_existing(&tmp)?;

    let result = seven_z::decompress(&archive, &out, &opts());

    assert!(
        matches!(result, Err(Error::FileExists(_))),
        "must refuse to clobber an existing file, got {result:?}",
    );
    assert_eq!(fs_err::read(out.join("src/foo.txt"))?, b"PRECIOUS");
    Ok(())
}

/// `--no-overwrite` keeps the existing file and carries on.
///
/// Doubles as the guard against a skipped entry desynchronising the solid
/// block: `top.txt` and `sub/n.txt` decode after the skipped `foo.txt`.
#[test]
fn no_overwrite_keeps_existing_and_extracts_the_rest() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let archive = build_archive(&tmp)?;
    let out = out_with_existing(&tmp)?;

    let mut o = opts();
    o.no_overwrite = true;
    seven_z::decompress(&archive, &out, &o)?;

    assert_eq!(fs_err::read(out.join("src/foo.txt"))?, b"PRECIOUS");
    assert_eq!(fs_err::read(out.join("src/top.txt"))?, b"top-content");
    assert_eq!(fs_err::read(out.join("src/sub/n.txt"))?, b"nested");
    Ok(())
}

#[test]
fn backup_moves_the_existing_file_aside() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let archive = build_archive(&tmp)?;
    let out = out_with_existing(&tmp)?;

    let mut o = opts();
    o.backup_suffix = Some(".bak".to_owned());
    seven_z::decompress(&archive, &out, &o)?;

    assert_eq!(fs_err::read(out.join("src/foo.txt.bak"))?, b"PRECIOUS");
    assert_eq!(fs_err::read(out.join("src/foo.txt"))?, b"foo-content");
    Ok(())
}

#[test]
fn prefix_and_rename_are_applied() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let archive = build_archive(&tmp)?;
    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;

    let mut o = opts();
    o.force = true;
    o.renames = vec![("foo".to_owned(), "bar".to_owned())];
    o.prefix = Some(Utf8PathBuf::from("restore/v2"));
    seven_z::decompress(&archive, &out, &o)?;

    assert_eq!(
        fs_err::read(out.join("restore/v2/src/bar.txt"))?,
        b"foo-content",
    );
    assert!(
        !out.join("src/foo.txt").exists(),
        "unrewritten path was written"
    );
    Ok(())
}

/// `--no-directory` flattens to basenames.  This previously failed outright,
/// because the doubly-joined path made the destination `out/top.txt/top.txt`.
#[test]
fn no_directory_flattens_to_basenames() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let archive = build_archive(&tmp)?;
    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;

    let mut o = opts();
    o.force = true;
    o.no_directory = true;
    seven_z::decompress(&archive, &out, &o)?;

    assert_eq!(fs_err::read(out.join("n.txt"))?, b"nested");
    assert_eq!(fs_err::read(out.join("foo.txt"))?, b"foo-content");
    assert!(!out.join("src").exists(), "directory structure survived -j");
    Ok(())
}

/// An excluded entry must still be read to its end.  Entry readers are bounded
/// views over one shared solid-block stream, so abandoning a payload used to
/// leave every later entry decoding from a misaligned offset.
#[test]
fn excluded_entry_is_drained_so_later_entries_survive() -> TestResult {
    let (_g, tmp) = temp_utf8_dir()?;
    let archive = build_archive(&tmp)?;
    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;

    // Built via the CLI's own glob helper so the bare pattern matches at any
    // depth (`**/top*`), the way `--exclude top*` behaves.
    let excludes = rz_archive::filter::build_glob_set(&["top*".to_owned()])?;
    let o = DecompressOpts::new(true, 0, GlobSet::empty(), excludes);
    seven_z::decompress(&archive, &out, &o)?;

    assert!(!out.join("src/top.txt").exists(), "excluded entry was written");
    assert_eq!(fs_err::read(out.join("src/foo.txt"))?, b"foo-content");
    assert_eq!(fs_err::read(out.join("src/sub/n.txt"))?, b"nested");
    Ok(())
}
