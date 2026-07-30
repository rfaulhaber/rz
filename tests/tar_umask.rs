//! Extraction must honour the process umask unless `-P` is given, matching
//! GNU tar: a mode-0777 entry extracted under umask 022 yields 0755, not a
//! world-writable 0777 file.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
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

/// Build a tar whose entries all carry mode 0777: a directory and a file.
fn wide_open_tar(tmp: &Utf8PathBuf) -> Result<Utf8PathBuf, Box<dyn std::error::Error>> {
    let mut builder = tar::Builder::new(Vec::new());

    let mut dir = tar::Header::new_gnu();
    dir.set_entry_type(tar::EntryType::Directory);
    dir.set_size(0);
    dir.set_mode(0o777);
    dir.set_mtime(1_000_000_000);
    builder.append_data(&mut dir, "d/", std::io::empty())?;

    let content = b"wide open";
    let mut file = tar::Header::new_gnu();
    file.set_size(content.len() as u64);
    file.set_mode(0o777);
    file.set_mtime(1_000_000_000);
    builder.append_data(&mut file, "d/wide.txt", content.as_slice())?;

    let archive = tmp.join("wide.tar");
    fs_err::write(&archive, builder.into_inner()?)?;
    Ok(archive)
}

/// Run the binary with the child's umask forced to `mask`.
fn run_with_umask(
    args: &[&str],
    cwd: &Utf8PathBuf,
    mask: u32,
) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let mut cmd = Command::new(rz_bin());
    cmd.args(args).current_dir(cwd.as_std_path());
    unsafe {
        cmd.pre_exec(move || {
            libc::umask(mask as libc::mode_t);
            Ok(())
        });
    }
    Ok(cmd.output()?)
}

fn mode_of(path: &Utf8PathBuf) -> Result<u32, Box<dyn std::error::Error>> {
    Ok(fs_err::metadata(path)?.permissions().mode() & 0o7777)
}

#[test]
fn extraction_applies_umask_by_default() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = wide_open_tar(&tmp)?;

    let out = run_with_umask(
        &["decompress", archive.as_str(), "-o", "out"],
        &tmp,
        0o022,
    )?;
    assert!(
        out.status.success(),
        "decompress failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(
        mode_of(&tmp.join("out/d/wide.txt"))?,
        0o755,
        "file mode must be masked by umask 022",
    );
    assert_eq!(
        mode_of(&tmp.join("out/d"))?,
        0o755,
        "directory mode must be masked by umask 022",
    );
    Ok(())
}

#[test]
fn preserve_permissions_ignores_umask() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = wide_open_tar(&tmp)?;

    let out = run_with_umask(
        &["decompress", archive.as_str(), "-o", "out", "-P"],
        &tmp,
        0o022,
    )?;
    assert!(
        out.status.success(),
        "decompress -P failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(mode_of(&tmp.join("out/d/wide.txt"))?, 0o777);
    assert_eq!(mode_of(&tmp.join("out/d"))?, 0o777);
    Ok(())
}
