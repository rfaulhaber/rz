//! Read-only directory entries must not block extraction of their own
//! contents.  tar-rs's `Entry::unpack` chmods a directory the moment its
//! entry is seen, so a `dr-xr-xr-x` entry appearing before its children made
//! the whole extraction fail with EACCES — GNU tar and tar-rs's own
//! `Archive::_unpack` defer directory metadata instead.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use camino::Utf8PathBuf;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rz_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz")
}

fn temp_dir() -> Result<(tempfile::TempDir, Utf8PathBuf), Box<dyn std::error::Error>> {
    let guard = tempfile::tempdir()?;
    let path = Utf8PathBuf::try_from(guard.path().to_path_buf())
        .map_err(|e| format!("non-UTF-8 tempdir: {e}"))?;
    Ok((guard, path))
}

const DIR_MTIME: u64 = 1_000_000_000;

/// Tar layout: `ro/` (mode 0555, before its children) → `ro/sub/` (0555) →
/// `ro/sub/file.txt`.
fn readonly_dir_tar(tmp: &Utf8PathBuf) -> Result<Utf8PathBuf, Box<dyn std::error::Error>> {
    let mut builder = tar::Builder::new(Vec::new());

    for dir in ["ro/", "ro/sub/"] {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Directory);
        h.set_size(0);
        h.set_mode(0o555);
        h.set_mtime(DIR_MTIME);
        builder.append_data(&mut h, dir, std::io::empty())?;
    }

    let content = b"locked in";
    let mut h = tar::Header::new_gnu();
    h.set_size(content.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(DIR_MTIME + 5);
    builder.append_data(&mut h, "ro/sub/file.txt", content.as_slice())?;

    let archive = tmp.join("ro.tar");
    fs_err::write(&archive, builder.into_inner()?)?;
    Ok(archive)
}

/// Restore write permission so the tempdir can be cleaned up.
fn unlock(tmp: &Utf8PathBuf) {
    for p in [tmp.join("out/ro/sub"), tmp.join("out/ro")] {
        let _ = fs_err::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn readonly_dirs_extract_with_their_contents() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = readonly_dir_tar(&tmp)?;

    let out = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["decompress", archive.as_str(), "-o", "out", "-P"])
        .output()?;
    let ok = out.status.success();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    let file = tmp.join("out/ro/sub/file.txt");
    let file_ok = file.as_std_path().exists();
    let dir_mode = fs_err::metadata(tmp.join("out/ro"))
        .map(|m| m.permissions().mode() & 0o7777)
        .unwrap_or(0);
    let sub_mode = fs_err::metadata(tmp.join("out/ro/sub"))
        .map(|m| m.permissions().mode() & 0o7777)
        .unwrap_or(0);
    unlock(&tmp);

    assert!(ok, "extraction failed: {stderr}");
    assert!(file_ok, "file inside read-only dirs was not extracted");
    assert_eq!(dir_mode, 0o555, "-P must restore the exact directory mode");
    assert_eq!(sub_mode, 0o555);
    Ok(())
}

#[test]
fn directory_mtimes_are_restored_after_children() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = readonly_dir_tar(&tmp)?;

    let out = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["decompress", archive.as_str(), "-o", "out"])
        .output()?;
    let ok = out.status.success();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    let mtime = fs_err::metadata(tmp.join("out/ro"))
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    unlock(&tmp);

    assert!(ok, "extraction failed: {stderr}");
    assert_eq!(
        mtime,
        Some(DIR_MTIME),
        "directory mtime must come from the archive, not the extraction time",
    );
    Ok(())
}
