//! `rz convert` is documented as a format-only re-encode, so entry metadata
//! must survive the extract-to-tempdir round-trip: permissions (previously
//! stripped from zip/7z sources because the tempdir extraction ignored them)
//! and mtimes (directories were re-dated to the conversion time; zip sources
//! lost every mtime).

use std::io::Read;
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

fn run(args: &[&str]) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let out = Command::new(rz_bin()).args(args).output()?;
    if !out.status.success() {
        return Err(format!(
            "rz {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(out)
}

/// One tar entry's (name, mode, mtime).
type TarEntryMeta = (String, u32, u64);

/// Collect the metadata of every entry of an uncompressed or gzipped tar.
fn tar_entries(path: &Utf8PathBuf) -> Result<Vec<TarEntryMeta>, Box<dyn std::error::Error>> {
    let raw = fs_err::read(path)?;
    let plain: Box<dyn Read> = if path.as_str().ends_with(".gz") {
        Box::new(flate2::read::MultiGzDecoder::new(std::io::Cursor::new(raw)))
    } else {
        Box::new(std::io::Cursor::new(raw))
    };
    let mut archive = tar::Archive::new(plain);
    let mut out = Vec::new();
    for entry in archive.entries()? {
        let entry = entry?;
        let header = entry.header();
        let name = entry.path()?.to_string_lossy().into_owned();
        out.push((name, header.mode()?, header.mtime()?));
    }
    Ok(out)
}

fn find<'a>(
    entries: &'a [(String, u32, u64)],
    suffix: &str,
) -> Result<&'a (String, u32, u64), Box<dyn std::error::Error>> {
    entries
        .iter()
        .find(|(n, _, _)| n.trim_end_matches('/').ends_with(suffix))
        .ok_or_else(|| format!("no entry ending in {suffix}: {entries:?}").into())
}

const FILE_MTIME: i64 = 1_400_000_000;
const DIR_MTIME: i64 = 1_300_000_000;

#[cfg(unix)]
#[test]
fn zip_to_tar_preserves_exec_bit() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let (_guard, tmp) = temp_dir()?;
    let script = tmp.join("run.sh");
    fs_err::write(&script, "#!/bin/sh\n")?;
    fs_err::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;

    let src = tmp.join("src.zip");
    run(&["compress", script.as_str(), "-o", src.as_str()])?;
    let out = tmp.join("out.tar");
    run(&["convert", src.as_str(), "-o", out.as_str()])?;

    let entries = tar_entries(&out)?;
    let (_, mode, _) = find(&entries, "run.sh")?;
    assert_eq!(
        mode & 0o777,
        0o755,
        "executable bit must survive zip → tar conversion",
    );
    Ok(())
}

#[test]
fn tar_to_targz_preserves_file_and_dir_mtimes() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("tree");
    fs_err::create_dir_all(tree.join("sub"))?;
    fs_err::write(tree.join("sub/a.txt"), "alpha")?;

    let file_time = filetime::FileTime::from_unix_time(FILE_MTIME, 0);
    let dir_time = filetime::FileTime::from_unix_time(DIR_MTIME, 0);
    filetime::set_file_times(tree.join("sub/a.txt").as_std_path(), file_time, file_time)?;
    filetime::set_file_times(tree.join("sub").as_std_path(), dir_time, dir_time)?;
    filetime::set_file_times(tree.as_std_path(), dir_time, dir_time)?;

    let src = tmp.join("src.tar");
    run(&["compress", tree.as_str(), "-o", src.as_str()])?;
    let out = tmp.join("out.tar.gz");
    run(&["convert", src.as_str(), "--to", "tar-gz", "-o", out.as_str()])?;

    let entries = tar_entries(&out)?;
    assert_eq!(find(&entries, "sub/a.txt")?.2, FILE_MTIME as u64);
    assert_eq!(
        find(&entries, "tree/sub")?.2,
        DIR_MTIME as u64,
        "directory mtimes must not be re-dated to the conversion time",
    );
    Ok(())
}

#[test]
fn zip_stores_and_convert_carries_source_mtimes() -> TestResult {
    // Even seconds and >= 1980, since zip DOS times have 2s resolution.
    const T: i64 = 1_500_000_000;

    let (_guard, tmp) = temp_dir()?;
    let payload = tmp.join("a.txt");
    fs_err::write(&payload, "alpha")?;
    let t = filetime::FileTime::from_unix_time(T, 0);
    filetime::set_file_times(payload.as_std_path(), t, t)?;

    let src = tmp.join("src.zip");
    run(&["compress", payload.as_str(), "-o", src.as_str()])?;

    // The zip itself must record the source mtime (the crate default would
    // stamp compression time instead).
    let listed = run(&["list", "--json", src.as_str()])?;
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&listed.stdout)?;
    let mtime = rows.first().and_then(|r| r["mtime"].as_u64());
    assert_eq!(mtime, Some(T as u64), "zip must store the source file mtime");

    // ... and conversion must carry it into the tar output.
    let out = tmp.join("out.tar");
    run(&["convert", src.as_str(), "-o", out.as_str()])?;
    assert_eq!(find(&tar_entries(&out)?, "a.txt")?.2, T as u64);
    Ok(())
}

/// Directory entries used to return from the zip extractor before any
/// permission handling, so a 0700 directory in a zip came out at the umask
/// default (world-readable) through both `decompress -P` and convert.
#[cfg(unix)]
#[test]
fn zip_sources_preserve_directory_modes() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("d");
    fs_err::create_dir_all(tree.join("secret"))?;
    fs_err::write(tree.join("secret/f.txt"), "x")?;
    fs_err::set_permissions(tree.join("secret"), std::fs::Permissions::from_mode(0o700))?;

    let src = tmp.join("src.zip");
    run(&["compress", tree.as_str(), "-o", src.as_str()])?;

    // Plain extraction under -P.
    let out_dir = tmp.join("out");
    run(&["decompress", src.as_str(), "-o", out_dir.as_str(), "-P"])?;
    let mode = fs_err::metadata(out_dir.join("d/secret"))?.permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "decompress -P must restore zip directory modes");

    // And through convert into tar.
    let out = tmp.join("out.tar");
    run(&["convert", src.as_str(), "-o", out.as_str()])?;
    let entries = tar_entries(&out)?;
    let (_, mode, _) = find(&entries, "secret")?;
    assert_eq!(
        mode & 0o777,
        0o700,
        "directory mode must survive zip → tar conversion",
    );
    Ok(())
}

/// Out-of-DOS-range mtimes clamp to the 1980 floor when writing zip; the
/// crate default would stamp the conversion wall-clock, off by decades and
/// different on every run.
#[test]
fn pre_1980_mtimes_clamp_to_the_dos_floor_in_zip() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("pre");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("old.txt"), "x")?;
    let t = filetime::FileTime::from_unix_time(170_856_000, 0); // 1975
    filetime::set_file_times(tree.join("old.txt").as_std_path(), t, t)?;

    let src = tmp.join("src.tar");
    run(&["compress", tree.as_str(), "-o", src.as_str()])?;
    let out = tmp.join("out.zip");
    run(&["convert", src.as_str(), "-o", out.as_str()])?;

    let listing = run(&["list", "--json", out.as_str()])?;
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&listing.stdout)?;
    let mtime = rows
        .iter()
        .find(|r| r["path"].as_str().is_some_and(|p| p.ends_with("old.txt")))
        .and_then(|r| r["mtime"].as_u64())
        .ok_or("old.txt missing from listing")?;
    assert_eq!(mtime, 315_532_800, "1975 must clamp to 1980-01-01, not 'now'");
    Ok(())
}

/// mtime 0 is a real value in epoch-stamped reproducible tars; the restore
/// pass used to skip it, leaving tar-rs's mtime-1 write in place.
#[test]
fn epoch_mtimes_survive_tar_conversions_exactly() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let tree = tmp.join("e");
    fs_err::create_dir_all(&tree)?;
    fs_err::write(tree.join("epoch.txt"), "x")?;
    let t = filetime::FileTime::from_unix_time(0, 0);
    filetime::set_file_times(tree.join("epoch.txt").as_std_path(), t, t)?;

    let src = tmp.join("src.tar");
    run(&["compress", tree.as_str(), "-o", src.as_str()])?;
    let out = tmp.join("out.tar.gz");
    run(&["convert", src.as_str(), "-o", out.as_str()])?;

    let entries = tar_entries(&out)?;
    let (_, _, mtime) = find(&entries, "epoch.txt")?;
    assert_eq!(*mtime, 0, "epoch mtime must survive conversion exactly");
    Ok(())
}
