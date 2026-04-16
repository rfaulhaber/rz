mod helpers;

use std::os::unix::fs::symlink;

use camino::{Utf8Path, Utf8PathBuf};
use globset::GlobSet;
use helpers::{TestResult, temp_utf8_dir};

use rz::{CompressOpts, DecompressOpts};

fn compress_opts() -> CompressOpts<'static> {
    CompressOpts::new(None, GlobSet::empty())
}

fn decompress_opts() -> DecompressOpts<'static> {
    DecompressOpts::new(false, 0, GlobSet::empty(), GlobSet::empty())
}

#[test]
fn tar_preserves_symlinks_by_default() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;
    fs_err::write(tree.join("real.txt"), b"target content\n")?;
    symlink("real.txt", tree.join("link.txt").as_std_path())?;

    let archive = tmp.join("archive.tar");
    rz::tar::compress(std::slice::from_ref(&tree), &archive, &compress_opts())?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz::tar::decompress(&archive, &out, &decompress_opts())?;

    let link = out.join("tree/link.txt");
    let meta = fs_err::symlink_metadata(&link)?;
    assert!(
        meta.file_type().is_symlink(),
        "extracted link.txt should be a symlink, got {:?}",
        meta.file_type(),
    );
    let target = fs_err::read_link(&link)?;
    let target = Utf8PathBuf::try_from(target)?;
    assert_eq!(target, Utf8Path::new("real.txt"));
    Ok(())
}

#[test]
fn tar_follow_symlinks_dereferences() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;
    fs_err::write(tree.join("real.txt"), b"target content\n")?;
    symlink("real.txt", tree.join("link.txt").as_std_path())?;

    let archive = tmp.join("archive.tar");
    let mut opts = compress_opts();
    opts.follow_symlinks = true;
    rz::tar::compress(std::slice::from_ref(&tree), &archive, &opts)?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz::tar::decompress(&archive, &out, &decompress_opts())?;

    let link = out.join("tree/link.txt");
    let meta = fs_err::symlink_metadata(&link)?;
    assert!(
        !meta.file_type().is_symlink(),
        "with --follow-symlinks the entry should be a regular file",
    );
    let contents = fs_err::read(&link)?;
    assert_eq!(contents, b"target content\n");
    Ok(())
}

#[test]
fn tar_handles_broken_symlink() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;
    symlink("does-not-exist", tree.join("dangling").as_std_path())?;

    let archive = tmp.join("archive.tar");
    rz::tar::compress(std::slice::from_ref(&tree), &archive, &compress_opts())?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz::tar::decompress(&archive, &out, &decompress_opts())?;

    let link = out.join("tree/dangling");
    let meta = fs_err::symlink_metadata(&link)?;
    assert!(meta.file_type().is_symlink());
    Ok(())
}

#[test]
fn zip_preserves_symlinks_by_default() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;
    fs_err::write(tree.join("real.txt"), b"target content\n")?;
    symlink("real.txt", tree.join("link.txt").as_std_path())?;

    let archive = tmp.join("archive.zip");
    rz::zip::compress(std::slice::from_ref(&tree), &archive, &compress_opts())?;

    // Inspect the archive directly — the rz zip extractor does not yet
    // recreate symlinks on unpack, so we verify the stored entry instead.
    let file = fs_err::File::open(&archive)?;
    let mut z = ::zip::ZipArchive::new(file)?;
    let mut found = false;
    for i in 0..z.len() {
        let entry = z.by_index(i)?;
        if entry.name().ends_with("link.txt") {
            assert!(entry.is_symlink(), "link.txt should be a symlink entry");
            found = true;
        }
    }
    assert!(found, "link.txt missing from archive");
    Ok(())
}

#[test]
fn zip_top_level_symlink_is_preserved() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let real = tmp.join("real.txt");
    fs_err::write(&real, b"target\n")?;
    let link = tmp.join("link.txt");
    symlink("real.txt", link.as_std_path())?;

    let archive = tmp.join("archive.zip");
    rz::zip::compress(
        &[Utf8PathBuf::from(&link)],
        &archive,
        &compress_opts(),
    )?;

    let file = fs_err::File::open(&archive)?;
    let mut z = ::zip::ZipArchive::new(file)?;
    let entry = z.by_index(0)?;
    assert_eq!(entry.name(), "link.txt");
    assert!(entry.is_symlink(), "top-level symlink must be preserved");
    Ok(())
}
