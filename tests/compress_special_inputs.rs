//! Compress-walk handling of inputs zip/7z cannot represent: FIFOs (whose
//! `open(2)` blocks forever without a writer) are warned about and skipped,
//! and `..` inputs resolve to the real directory name instead of baking
//! `../` into entry names that extraction rejects as traversal.
#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

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

fn mkfifo(path: &Utf8PathBuf) -> TestResult {
    let c_path = std::ffi::CString::new(path.as_str())?;
    // SAFETY: c_path is a valid NUL-terminated string for the duration of
    // the call.
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Run rz with a hard wall-clock cap so a regression hangs the test, not CI.
fn run_bounded(
    cwd: &Utf8PathBuf,
    args: &[&str],
) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let mut child = Command::new(rz_bin())
        .current_dir(cwd.as_std_path())
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() > deadline {
            child.kill()?;
            child.wait()?;
            return Err(format!("rz {args:?} hung past the deadline").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn list(cwd: &Utf8PathBuf, archive: &str) -> Result<String, Box<dyn std::error::Error>> {
    let out = run_bounded(cwd, &["list", archive])?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn zip_and_seven_z_skip_fifos_with_a_warning() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let src = tmp.join("F");
    fs_err::create_dir_all(&src)?;
    fs_err::write(src.join("a.txt"), "content")?;
    mkfifo(&src.join("pipe"))?;

    for archive in ["out.zip", "out.7z"] {
        let out = run_bounded(&tmp, &["compress", "F", "-o", archive])?;
        assert!(
            out.status.success(),
            "{archive}: {}",
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("skipping"),
            "{archive}: a skipped FIFO must be warned about",
        );
        let listing = list(&tmp, archive)?;
        assert!(listing.contains("F/a.txt"), "{archive}: {listing}");
        assert!(!listing.contains("pipe"), "{archive}: {listing}");
    }
    Ok(())
}

#[test]
fn zip_append_skips_fifos_instead_of_hanging() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    fs_err::write(tmp.join("seed.txt"), "seed")?;
    let out = run_bounded(&tmp, &["compress", "seed.txt", "-o", "arc.zip"])?;
    assert!(out.status.success());

    let src = tmp.join("F");
    fs_err::create_dir_all(&src)?;
    fs_err::write(src.join("a.txt"), "content")?;
    mkfifo(&src.join("pipe"))?;
    let out = run_bounded(&tmp, &["append", "arc.zip", "F"])?;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let listing = list(&tmp, "arc.zip")?;
    assert!(listing.contains("F/a.txt"), "{listing}");
    assert!(!listing.contains("pipe"), "{listing}");
    Ok(())
}

#[test]
fn tar_still_stores_fifos_as_entries() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let src = tmp.join("F");
    fs_err::create_dir_all(&src)?;
    fs_err::write(src.join("a.txt"), "content")?;
    mkfifo(&src.join("pipe"))?;

    let out = run_bounded(&tmp, &["compress", "F", "-o", "out.tar"])?;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let listing = list(&tmp, "out.tar")?;
    assert!(listing.contains("F/pipe"), "{listing}");
    Ok(())
}

#[test]
fn dot_dot_input_resolves_to_the_real_directory_name() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let inner = tmp.join("project/inner");
    fs_err::create_dir_all(&inner)?;
    fs_err::write(tmp.join("project/file.txt"), "x")?;

    for archive in ["out.zip", "out.7z"] {
        let dest = tmp.join(archive);
        let out = run_bounded(&inner, &["compress", "..", "-o", dest.as_str()])?;
        assert!(
            out.status.success(),
            "{archive}: {}",
            String::from_utf8_lossy(&out.stderr),
        );
        let listing = list(&tmp, archive)?;
        assert!(
            !listing.contains(".."),
            "{archive}: `..` leaked into entry names: {listing}",
        );
        assert!(listing.contains("project/file.txt"), "{archive}: {listing}");

        // The point of the rewrite: rz itself can extract the result.
        let out_dir = tmp.join(format!("extract-{archive}"));
        let out = run_bounded(
            &tmp,
            &["decompress", archive, "-o", out_dir.as_str()],
        )?;
        assert!(
            out.status.success(),
            "{archive}: {}",
            String::from_utf8_lossy(&out.stderr),
        );
        assert_eq!(fs_err::read_to_string(out_dir.join("project/file.txt"))?, "x");
    }
    Ok(())
}
