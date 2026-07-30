//! Hard-link entries must resolve inside the output directory.
//!
//! tar-rs only populates its internal `target_base` when an entry is unpacked
//! via `unpack_in`.  Unpacked directly, an `EntryType::Link` entry passes its
//! raw link name to `hard_link(2)`, which resolves it against the *process
//! working directory* — so a bare relative target links the extracted file to
//! whatever happens to sit next to the caller, and writes through it land in
//! the victim's file.  `safe_link_target` cannot catch this: a bare name has
//! no `..` and no leading `/`.
//!
//! These shell out to the compiled binary so the process working directory can
//! be set without racing the other tests in this process.

mod helpers;

use std::process::Command;

use camino::Utf8Path;
use helpers::{TestResult, temp_utf8_dir};

fn rz_archive_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz")
}

/// Build a tar holding `inside.txt` plus `linked.txt`, a hard link to it.
///
/// The link target is a bare relative name, which is what makes it ambiguous
/// between "inside the archive" and "next to the caller".
fn build_hard_link_tar(archive: &Utf8Path) -> TestResult {
    let file = fs_err::File::create(archive)?;
    let mut builder = tar::Builder::new(file);

    let body = b"archive-content";
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, "inside.txt", &body[..])?;

    let mut link = tar::Header::new_gnu();
    link.set_entry_type(tar::EntryType::Link);
    link.set_size(0);
    link.set_mode(0o644);
    builder.append_link(&mut link, "linked.txt", "inside.txt")?;

    builder.finish()?;
    Ok(())
}

#[cfg(unix)]
fn inode(path: &Utf8Path) -> Result<u64, Box<dyn std::error::Error>> {
    use std::os::unix::fs::MetadataExt;
    Ok(fs_err::metadata(path)?.ino())
}

/// The extracted link must point at the archive's own copy, never at the
/// identically-named file sitting in the working directory.
#[cfg(unix)]
#[test]
fn hard_link_target_resolves_inside_the_output_directory() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let archive = tmp.join("hl.tar");
    build_hard_link_tar(&archive)?;

    // The victim: same name as the archive's link target, in the directory the
    // extraction runs from.
    let victim = tmp.join("inside.txt");
    fs_err::write(&victim, b"victim-secret")?;

    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;

    let status = Command::new(rz_archive_bin())
        .current_dir(tmp.as_std_path())
        .args(["decompress", archive.as_str(), "-o", out.as_str()])
        .status()?;
    assert!(status.success(), "rz exited with {status}");

    let extracted_link = out.join("linked.txt");
    assert_ne!(
        inode(&extracted_link)?,
        inode(&victim)?,
        "extracted hard link shares an inode with the file outside the output directory",
    );
    assert_eq!(
        inode(&extracted_link)?,
        inode(&out.join("inside.txt"))?,
        "extracted hard link should share an inode with the archive's own copy",
    );

    // The link is a real link, and writing through it must not reach the victim.
    fs_err::write(&extracted_link, b"tampered")?;
    assert_eq!(fs_err::read(&victim)?, b"victim-secret");
    assert_eq!(fs_err::read(out.join("inside.txt"))?, b"tampered");
    Ok(())
}

/// `--strip-components` has to rewrite the link target too, or the link points
/// at a path that no longer exists after stripping.
#[cfg(unix)]
#[test]
fn hard_link_target_follows_strip_components() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let archive = tmp.join("hl.tar");
    let file = fs_err::File::create(&archive)?;
    let mut builder = tar::Builder::new(file);

    let body = b"archive-content";
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, "top/inside.txt", &body[..])?;

    let mut link = tar::Header::new_gnu();
    link.set_entry_type(tar::EntryType::Link);
    link.set_size(0);
    link.set_mode(0o644);
    builder.append_link(&mut link, "top/linked.txt", "top/inside.txt")?;
    builder.finish()?;

    let out = tmp.join("out");
    fs_err::create_dir_all(&out)?;

    let status = Command::new(rz_archive_bin())
        .current_dir(tmp.as_std_path())
        .args([
            "decompress",
            archive.as_str(),
            "-o",
            out.as_str(),
            "--strip-components",
            "1",
        ])
        .status()?;
    assert!(status.success(), "rz exited with {status}");

    assert_eq!(
        inode(&out.join("linked.txt"))?,
        inode(&out.join("inside.txt"))?,
        "stripped hard link should still point at the archive's own copy",
    );
    Ok(())
}
