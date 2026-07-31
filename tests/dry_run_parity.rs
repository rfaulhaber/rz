//! `decompress --dry-run` must predict the real run: same destination paths
//! (mtime window, --no-directory, --rename, --prefix applied) and the same
//! flag-support rejections — previously it printed pre-rewrite names and
//! returned before validation, so `-n` exited 0 for commands whose real run
//! errors.

use std::collections::BTreeSet;
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

/// Build `<tmp>/p.tar` holding `d/` and `d/a.txt`.
fn simple_tar(tmp: &Utf8PathBuf) -> Result<Utf8PathBuf, Box<dyn std::error::Error>> {
    let tree = tmp.join("d");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("a.txt"), "alpha")?;
    let archive = tmp.join("p.tar");
    let out = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["compress", "d", "-o", "p.tar"])
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(archive)
}

/// Every path on disk under `root`, relative to it (files and directories).
fn paths_under(root: &Utf8PathBuf) -> Result<BTreeSet<String>, Box<dyn std::error::Error>> {
    fn walk(
        root: &Utf8PathBuf,
        dir: &Utf8PathBuf,
        acc: &mut BTreeSet<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for entry in fs_err::read_dir(dir)? {
            let entry = entry?;
            let p = Utf8PathBuf::try_from(entry.path())
                .map_err(|e| format!("non-UTF-8 path: {e}"))?;
            let rel = p
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string();
            acc.insert(rel);
            if entry.file_type()?.is_dir() {
                walk(root, &p, acc)?;
            }
        }
        Ok(())
    }
    let mut acc = BTreeSet::new();
    walk(root, root, &mut acc)?;
    Ok(acc)
}

#[test]
fn dry_run_names_the_paths_a_real_run_creates() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = simple_tar(&tmp)?;
    let rewrite_args = ["--rename", "d=renamed", "--prefix", "restore/v2"];

    let dry = Command::new(rz_bin())
        .args(["decompress", archive.as_str(), "-n"])
        .args(rewrite_args)
        .output()?;
    assert!(
        dry.status.success(),
        "dry-run failed: {}",
        String::from_utf8_lossy(&dry.stderr),
    );
    let predicted: BTreeSet<String> = String::from_utf8_lossy(&dry.stdout)
        .lines()
        .map(str::to_owned)
        .collect();

    let out_dir = tmp.join("real");
    let real = Command::new(rz_bin())
        .args(["decompress", archive.as_str(), "-o", out_dir.as_str()])
        .args(rewrite_args)
        .output()?;
    assert!(
        real.status.success(),
        "real run failed: {}",
        String::from_utf8_lossy(&real.stderr),
    );
    let created = paths_under(&out_dir)?;

    assert!(
        predicted.contains("restore/v2/renamed/a.txt"),
        "dry-run must name the rewritten file path, got: {predicted:?}",
    );
    for path in &predicted {
        assert!(
            created.contains(path),
            "dry-run predicted `{path}` but the real run created {created:?}",
        );
    }
    for path in &created {
        // Intermediate directories (from --prefix) exist on disk without a
        // matching archive entry; every *file* must have been predicted.
        if tmp.join("real").join(path).is_file() {
            assert!(
                predicted.contains(path),
                "real run created `{path}` but dry-run predicted {predicted:?}",
            );
        }
    }
    Ok(())
}

#[test]
fn dry_run_applies_no_directory_flattening() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = simple_tar(&tmp)?;

    let dry = Command::new(rz_bin())
        .args(["decompress", archive.as_str(), "-n", "-j"])
        .output()?;
    assert!(dry.status.success());
    let lines: Vec<String> = String::from_utf8_lossy(&dry.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        lines,
        vec!["a.txt".to_owned()],
        "-j must flatten to basenames and drop directory entries",
    );
    Ok(())
}

#[test]
fn dry_run_rejects_unsupported_flags_like_a_real_run() -> TestResult {
    let (_guard, tmp) = temp_dir()?;

    // zip + --newer-than is rejected for real runs; -n must not exit 0.
    let payload = tmp.join("z.txt");
    fs_err::write(&payload, "z")?;
    let zip = tmp.join("z.zip");
    let ok = Command::new(rz_bin())
        .args(["compress", payload.as_str(), "-o", zip.as_str()])
        .output()?;
    assert!(ok.status.success());

    let out = Command::new(rz_bin())
        .args([
            "decompress",
            zip.as_str(),
            "-n",
            "--newer-than",
            "2030-01-01",
        ])
        .output()?;
    assert_eq!(
        out.status.code(),
        Some(1),
        "dry-run must reject --newer-than for zip exactly like a real run",
    );
    Ok(())
}

#[test]
fn dry_run_mirrors_seven_z_strip_rejection() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let payload = tmp.join("s.txt");
    fs_err::write(&payload, "s")?;
    let archive = tmp.join("s.7z");
    let ok = Command::new(rz_bin())
        .args(["compress", payload.as_str(), "-o", archive.as_str()])
        .output()?;
    assert!(ok.status.success());

    for extra in [&["-n"][..], &[][..]] {
        let out = Command::new(rz_bin())
            .args(["decompress", archive.as_str(), "--strip-components", "1"])
            .args(extra)
            .output()?;
        assert_eq!(
            out.status.code(),
            Some(1),
            "7z --strip-components must be rejected (extra args: {extra:?})",
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--strip-components"),
        );
    }
    Ok(())
}

#[test]
fn dry_run_applies_the_mtime_window() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("d");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("old.txt"), "old")?;
    fs_err::write(tree.join("new.txt"), "new")?;
    let old = filetime::FileTime::from_unix_time(1_000_000_000, 0);
    let new = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    filetime::set_file_times(tree.join("old.txt").as_std_path(), old, old)?;
    filetime::set_file_times(tree.join("new.txt").as_std_path(), new, new)?;
    let archive = tmp.join("t.tar");
    let ok = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["compress", "d", "-o", "t.tar"])
        .output()?;
    assert!(ok.status.success());

    let dry = Command::new(rz_bin())
        .args([
            "decompress",
            archive.as_str(),
            "-n",
            "--newer-than",
            "@1500000000",
        ])
        .output()?;
    assert!(dry.status.success());
    let stdout = String::from_utf8_lossy(&dry.stdout);
    assert!(stdout.contains("new.txt"), "in-window entry missing: {stdout}");
    assert!(
        !stdout.contains("old.txt"),
        "out-of-window entry must be filtered: {stdout}",
    );
    Ok(())
}

/// `compress -n` must mirror the real run's format-level rejections: a
/// preview that exits 0 for a command whose real run errors is a trap.
#[test]
fn compress_dry_run_rejects_what_the_real_run_rejects() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    fs_err::write(tmp.join("a.txt"), "x")?;

    // Reproducibility flags are tar-only; zip must be rejected in preview
    // exactly like in the real run.
    let dry = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["compress", "-n", "a.txt", "--mtime", "0", "-o", "out.zip"])
        .output()?;
    let real = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["compress", "a.txt", "--mtime", "0", "-o", "out.zip"])
        .output()?;
    assert_eq!(dry.status.code(), Some(1), "preview must reject like the real run");
    assert_eq!(dry.stderr, real.stderr, "identical rejection message expected");

    // With no output and no format the walk preview still works.
    let bare = Command::new(rz_bin())
        .current_dir(tmp.as_std_path())
        .args(["compress", "-n", "a.txt"])
        .output()?;
    assert!(bare.status.success());
    assert!(String::from_utf8_lossy(&bare.stdout).contains("a.txt"));
    Ok(())
}
