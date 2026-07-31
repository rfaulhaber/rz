//! A zip whose entries map a *file* onto `x` and another entry onto `x/y`
//! cannot extract; the failure must be deterministic.  The parallel planner
//! groups by exact destination only, so the two paths used to land on
//! different workers and race dir-vs-file creation — "Is a directory" on some
//! runs, "File exists" on others, different surviving trees.  Such archives
//! now fall back to strict archive order, matching a serial run every time.

use std::io::Write;
use std::process::Command;

use camino::Utf8PathBuf;
use zip::write::SimpleFileOptions;

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

/// Write a zip with the given (name, is_dir) entries, in order.
fn build_zip(path: &Utf8PathBuf, entries: &[(&str, bool)]) -> TestResult {
    let file = fs_err::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);
    for (name, is_dir) in entries {
        if *is_dir {
            zip.add_directory(*name, SimpleFileOptions::default())?;
        } else {
            zip.start_file(*name, SimpleFileOptions::default())?;
            zip.write_all(b"payload")?;
        }
    }
    zip.finish()?;
    Ok(())
}

/// One extraction attempt's observable outcome: exit code and stderr.
type RunOutcome = (Option<i32>, String);

/// Run the same extraction `runs` times against fresh output dirs.
fn repeated_runs(
    tmp: &Utf8PathBuf,
    archive: &Utf8PathBuf,
    runs: usize,
) -> Result<Vec<RunOutcome>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();
    for i in 0..runs {
        let out_dir = tmp.join(format!("out{i}"));
        let out = Command::new(rz_bin())
            .args([
                "--threads",
                "8",
                "decompress",
                archive.as_str(),
                "-o",
                out_dir.as_str(),
                "-F",
            ])
            .output()?;
        // Each run uses a fresh output dir; normalise it out of the message
        // so the runs' errors are comparable.
        let stderr = String::from_utf8_lossy(&out.stderr)
            .replace(out_dir.as_str(), "<out>");
        results.push((out.status.code(), stderr));
    }
    Ok(results)
}

#[test]
fn file_ancestor_conflict_fails_deterministically() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = tmp.join("conflict.zip");
    build_zip(&archive, &[("x", false), ("x/y", false)])?;

    let results = repeated_runs(&tmp, &archive, 10)?;
    for (code, stderr) in &results {
        assert_eq!(
            *code,
            Some(1),
            "conflicting archive must always fail (stderr: {stderr})",
        );
    }
    let first_stderr = &results
        .first()
        .ok_or("no runs")?
        .1;
    for (_, stderr) in &results {
        assert_eq!(
            stderr, first_stderr,
            "the failure must be the same on every run",
        );
    }
    Ok(())
}

#[test]
fn reversed_conflict_order_also_fails_deterministically() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = tmp.join("conflict-rev.zip");
    build_zip(&archive, &[("x/y", false), ("x", false)])?;

    let results = repeated_runs(&tmp, &archive, 10)?;
    let first = results.first().ok_or("no runs")?.clone();
    for (code, stderr) in &results {
        assert_eq!(*code, Some(1));
        assert_eq!(stderr, &first.1);
    }
    Ok(())
}

#[test]
fn directory_ancestors_stay_extractable() -> TestResult {
    // `d/` before `d/f` is the normal shape of every archive — it must not
    // trip the conflict fallback into refusing or misbehaving.
    let (_guard, tmp) = temp_dir()?;
    let archive = tmp.join("normal.zip");
    build_zip(&archive, &[("d/", true), ("d/f", false), ("d/sub/", true), ("d/sub/g", false)])?;

    let out_dir = tmp.join("out");
    let out = Command::new(rz_bin())
        .args([
            "--threads",
            "8",
            "decompress",
            archive.as_str(),
            "-o",
            out_dir.as_str(),
        ])
        .output()?;
    assert!(
        out.status.success(),
        "normal nested archive failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out_dir.join("d/f").as_std_path().is_file());
    assert!(out_dir.join("d/sub/g").as_std_path().is_file());
    Ok(())
}

/// `a/` (dir) and `a` (file) canonicalize to the same destination and share
/// one group, so the conflict never appears between *consecutive* groups —
/// it must be caught inside the group, or the archive stays parallel and
/// leaves a different partial tree on every run.
#[test]
fn dir_vs_file_same_destination_is_deterministic() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = tmp.join("dirfile.zip");
    let mut entries: Vec<(String, bool)> = vec![("a/".to_owned(), true), ("a".to_owned(), false)];
    // Enough filler to keep several rayon workers busy while the conflict
    // races, if it is ever allowed to race again.
    for i in 0..80 {
        entries.push((format!("f{i:03}.txt"), false));
    }
    let borrowed: Vec<(&str, bool)> = entries.iter().map(|(n, d)| (n.as_str(), *d)).collect();
    build_zip(&archive, &borrowed)?;

    let mut outcomes: Vec<(Option<i32>, String, Vec<String>)> = Vec::new();
    for i in 0..12 {
        let out_dir = tmp.join(format!("dvf{i}"));
        let out = Command::new(rz_bin())
            .args([
                "--threads",
                "8",
                "decompress",
                archive.as_str(),
                "-o",
                out_dir.as_str(),
            ])
            .output()?;
        let stderr =
            String::from_utf8_lossy(&out.stderr).replace(out_dir.as_str(), "<out>");
        let mut on_disk: Vec<String> = walkdir_paths(&out_dir)?;
        on_disk.sort();
        outcomes.push((out.status.code(), stderr, on_disk));
    }
    let first = outcomes.first().ok_or("no runs")?.clone();
    for (code, stderr, on_disk) in &outcomes {
        assert_eq!(*code, first.0, "exit status must be deterministic");
        assert_eq!(stderr, &first.1, "failure message must be deterministic");
        assert_eq!(
            on_disk, &first.2,
            "the on-disk result must be identical on every run",
        );
    }
    Ok(())
}

/// Every path under `root`, relative to it; empty when `root` is missing.
fn walkdir_paths(root: &Utf8PathBuf) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    fn walk(
        root: &Utf8PathBuf,
        dir: &Utf8PathBuf,
        acc: &mut Vec<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for entry in fs_err::read_dir(dir)? {
            let entry = entry?;
            let p = Utf8PathBuf::try_from(entry.path())
                .map_err(|e| format!("non-UTF-8 path: {e}"))?;
            acc.push(p.strip_prefix(root).map_err(|e| e.to_string())?.to_string());
            if entry.file_type()?.is_dir() {
                walk(root, &p, acc)?;
            }
        }
        Ok(())
    }
    let mut acc = Vec::new();
    if fs_err::metadata(root).is_ok() {
        walk(root, root, &mut acc)?;
    }
    Ok(acc)
}
