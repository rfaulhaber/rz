mod helpers;

use std::os::unix::fs::symlink;

use camino::{Utf8Path, Utf8PathBuf};
use globset::GlobSet;
use helpers::{TestResult, temp_utf8_dir};

use rz_archive::{CompressOpts, DecompressOpts};

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
    rz_archive::tar::compress(std::slice::from_ref(&tree), &archive, &compress_opts())?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz_archive::tar::decompress(&archive, &out, &decompress_opts())?;

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
    rz_archive::tar::compress(std::slice::from_ref(&tree), &archive, &opts)?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz_archive::tar::decompress(&archive, &out, &decompress_opts())?;

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
fn tar_rejects_absolute_symlink_target() -> TestResult {
    // Build a tar archive containing a symlink `evil` whose target is the
    // absolute path `/tmp/rz_archive-escape`.  Extraction must refuse this rather
    // than silently creating the symlink.
    let (_guard, tmp) = temp_utf8_dir()?;

    let archive = tmp.join("evil.tar");
    {
        let file = fs_err::File::create(&archive)?;
        let mut builder = ::tar::Builder::new(file);
        let mut header = ::tar::Header::new_gnu();
        header.set_entry_type(::tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        builder.append_link(&mut header, "evil", "/tmp/rz_archive-escape")?;
        builder.finish()?;
    }

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let res = rz_archive::tar::decompress(&archive, &out, &decompress_opts());
    assert!(
        res.is_err(),
        "extraction should reject absolute symlink target"
    );
    Ok(())
}

#[test]
fn tar_rejects_parent_dir_symlink_target() -> TestResult {
    // Same idea with `../../etc/passwd`, which is how real zip-slip symlink
    // attacks are typically packaged.
    let (_guard, tmp) = temp_utf8_dir()?;

    let archive = tmp.join("evil.tar");
    {
        let file = fs_err::File::create(&archive)?;
        let mut builder = ::tar::Builder::new(file);
        let mut header = ::tar::Header::new_gnu();
        header.set_entry_type(::tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        builder.append_link(&mut header, "evil", "../../etc/passwd")?;
        builder.finish()?;
    }

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let res = rz_archive::tar::decompress(&archive, &out, &decompress_opts());
    assert!(
        res.is_err(),
        "extraction should reject ..-containing symlink target"
    );
    Ok(())
}

#[test]
fn tar_handles_broken_symlink() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;
    symlink("does-not-exist", tree.join("dangling").as_std_path())?;

    let archive = tmp.join("archive.tar");
    rz_archive::tar::compress(std::slice::from_ref(&tree), &archive, &compress_opts())?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz_archive::tar::decompress(&archive, &out, &decompress_opts())?;

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
    rz_archive::zip::compress(std::slice::from_ref(&tree), &archive, &compress_opts())?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz_archive::zip::decompress(&archive, &out, &decompress_opts())?;

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
fn zip_overwrites_existing_symlink_on_force() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;
    fs_err::write(tree.join("real.txt"), b"target content\n")?;
    symlink("real.txt", tree.join("link.txt").as_std_path())?;

    let archive = tmp.join("archive.zip");
    rz_archive::zip::compress(std::slice::from_ref(&tree), &archive, &compress_opts())?;

    // First extraction creates the symlink.
    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz_archive::zip::decompress(&archive, &out, &decompress_opts())?;

    // Second extraction with --force must replace the existing symlink without
    // writing through it to the target.
    let mut opts = decompress_opts();
    opts.force = true;
    rz_archive::zip::decompress(&archive, &out, &opts)?;

    let link = out.join("tree/link.txt");
    let meta = fs_err::symlink_metadata(&link)?;
    assert!(
        meta.file_type().is_symlink(),
        "link.txt must still be a symlink"
    );
    Ok(())
}

#[test]
fn zip_rejects_parent_dir_symlink_target() -> TestResult {
    // A zip containing a symlink whose target climbs out of the extraction root
    // — the classic packaging for a symlink-based zip-slip.  rz can't produce
    // such an archive itself, so we hand-build one with the `zip` crate.
    let (_guard, tmp) = temp_utf8_dir()?;

    let archive = tmp.join("evil.zip");
    {
        let file = fs_err::File::create(&archive)?;
        let mut zw = ::zip::ZipWriter::new(file);
        let opts = ::zip::write::SimpleFileOptions::default();
        zw.add_symlink("evil", "../../../../tmp/rz_zip_escape", opts)?;
        zw.finish()?;
    }

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let res = rz_archive::zip::decompress(&archive, &out, &decompress_opts());
    assert!(
        res.is_err(),
        "extraction should reject ..-containing symlink target"
    );
    Ok(())
}

#[test]
fn zip_rejects_absolute_symlink_target() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let archive = tmp.join("evil.zip");
    {
        let file = fs_err::File::create(&archive)?;
        let mut zw = ::zip::ZipWriter::new(file);
        let opts = ::zip::write::SimpleFileOptions::default();
        zw.add_symlink("evil", "/tmp/rz_zip_escape", opts)?;
        zw.finish()?;
    }

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let res = rz_archive::zip::decompress(&archive, &out, &decompress_opts());
    assert!(
        res.is_err(),
        "extraction should reject absolute symlink target"
    );
    Ok(())
}

/// Build a tree with a regular file on each side of a symlink, so a
/// misaligned symlink entry would corrupt the file that follows it in
/// archive order.
fn build_straddling_tree(tree: &Utf8Path) -> TestResult {
    fs_err::create_dir(tree)?;
    fs_err::write(tree.join("a_data.bin"), vec![b'x'; 4096])?;
    symlink("a_data.bin", tree.join("m_link").as_std_path())?;
    fs_err::write(tree.join("z_after.txt"), b"after the symlink\n")?;
    Ok(())
}

#[test]
fn tar_mtime_override_does_not_corrupt_symlink_entry() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_straddling_tree(&tree)?;

    let archive = tmp.join("archive.tar");
    let mut opts = compress_opts();
    opts.fixed_mtime = Some(0);
    rz_archive::tar::compress(std::slice::from_ref(&tree), &archive, &opts)?;

    // (a) list succeeds and reports every entry with correct sizes.
    let entries = rz_archive::tar::list(&archive)?;
    let names: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    let by_name = |suffix: &str| -> Result<&rz_archive::Entry, Box<dyn std::error::Error>> {
        entries
            .iter()
            .find(|e| e.path.as_str().ends_with(suffix))
            .ok_or_else(|| format!("missing entry ending in {suffix} among {names:?}").into())
    };
    assert_eq!(by_name("a_data.bin")?.size, 4096);
    assert_eq!(by_name("m_link")?.size, 0);
    assert_eq!(by_name("z_after.txt")?.size, 18);

    // (b) + (c) decompress and check the symlink and the trailing file.
    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz_archive::tar::decompress(&archive, &out, &decompress_opts())?;

    let link = out.join("tree/m_link");
    let meta = fs_err::symlink_metadata(&link)?;
    assert!(
        meta.file_type().is_symlink(),
        "m_link should still be a symlink, got {:?}",
        meta.file_type(),
    );
    let target = fs_err::read_link(&link)?;
    let target = Utf8PathBuf::try_from(target)?;
    assert_eq!(target, Utf8Path::new("a_data.bin"));

    let after = fs_err::read(out.join("tree/z_after.txt"))?;
    assert_eq!(after, b"after the symlink\n");
    Ok(())
}

#[test]
fn tar_no_override_round_trips_straddling_symlink() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_straddling_tree(&tree)?;

    let archive = tmp.join("archive.tar");
    rz_archive::tar::compress(std::slice::from_ref(&tree), &archive, &compress_opts())?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz_archive::tar::decompress(&archive, &out, &decompress_opts())?;

    let link = out.join("tree/m_link");
    let meta = fs_err::symlink_metadata(&link)?;
    assert!(meta.file_type().is_symlink());
    let target = fs_err::read_link(&link)?;
    let target = Utf8PathBuf::try_from(target)?;
    assert_eq!(target, Utf8Path::new("a_data.bin"));

    let after = fs_err::read(out.join("tree/z_after.txt"))?;
    assert_eq!(after, b"after the symlink\n");
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
    rz_archive::zip::compress(&[Utf8PathBuf::from(&link)], &archive, &compress_opts())?;

    let file = fs_err::File::open(&archive)?;
    let mut z = ::zip::ZipArchive::new(file)?;
    let entry = z.by_index(0)?;
    assert_eq!(entry.name(), "link.txt");
    assert!(entry.is_symlink(), "top-level symlink must be preserved");
    Ok(())
}

// ── non-regular files under header overrides ─────────────────────────────

#[cfg(unix)]
#[test]
fn tar_rejects_unix_socket_with_header_overrides() -> TestResult {
    // Under --mtime (or any header override), append_file_entry used to fall
    // through to the generic "other" branch for anything that isn't a
    // symlink or regular file, turning a socket into a bogus archive entry
    // instead of erroring the way the no-override path always has.
    use std::os::unix::net::UnixListener;

    let (_guard, tmp) = temp_utf8_dir()?;
    let sock_path = tmp.join("test.sock");
    let _listener = UnixListener::bind(sock_path.as_std_path())?;

    let archive = tmp.join("archive.tar");
    let mut opts = compress_opts();
    opts.fixed_mtime = Some(0);
    let res = rz_archive::tar::compress(std::slice::from_ref(&sock_path), &archive, &opts);
    assert!(
        res.is_err(),
        "compressing a unix socket under a header override must fail, not silently archive it"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn tar_rejects_unix_socket_without_header_overrides() -> TestResult {
    // Guard for the no-override path, which already goes through tar's own
    // `append_special` and errors on sockets today.
    use std::os::unix::net::UnixListener;

    let (_guard, tmp) = temp_utf8_dir()?;
    let sock_path = tmp.join("test.sock");
    let _listener = UnixListener::bind(sock_path.as_std_path())?;

    let archive = tmp.join("archive.tar");
    let res =
        rz_archive::tar::compress(std::slice::from_ref(&sock_path), &archive, &compress_opts());
    assert!(
        res.is_err(),
        "compressing a unix socket must fail even without header overrides"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn tar_mtime_override_preserves_non_utf8_symlink_target() -> TestResult {
    // The header-override symlink branch used to hard-fail converting the
    // link target to a Utf8PathBuf, leaving a truncated archive. The target
    // must round-trip as raw bytes, same as the no-override path.
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    fs_err::create_dir(&tree)?;
    let raw_target: &[u8] = b"bad\xff\xfename";
    symlink(
        OsStr::from_bytes(raw_target),
        tree.join("link").as_std_path(),
    )?;

    let archive = tmp.join("archive.tar");
    let mut opts = compress_opts();
    opts.fixed_mtime = Some(0);
    rz_archive::tar::compress(std::slice::from_ref(&tree), &archive, &opts)?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz_archive::tar::decompress(&archive, &out, &decompress_opts())?;

    let link = out.join("tree/link");
    let meta = fs_err::symlink_metadata(&link)?;
    assert!(meta.file_type().is_symlink());
    let restored = fs_err::read_link(&link)?;
    assert_eq!(
        restored.as_os_str().as_bytes(),
        raw_target,
        "symlink target bytes must survive the round trip unmodified"
    );
    Ok(())
}
