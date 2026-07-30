//! The modify rewrite paths copy raw entry groups (src/tar_raw.rs), so GNU
//! long-name extensions and pax records must survive verbatim and keep
//! decisions must match on the *resolved* names.

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

fn run(cwd: &Utf8PathBuf, args: &[&str]) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let out = Command::new(rz_bin())
        .current_dir(cwd.as_std_path())
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "rz {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(out)
}

/// A name long enough to force a GNU `L` long-name extension (> 100 bytes).
fn long_name() -> String {
    format!("{}/leaf.txt", "very-long-directory-name".repeat(5))
}

fn write_long_name_tar(path: &Utf8PathBuf) -> TestResult {
    let mut builder = tar::Builder::new(Vec::new());
    let body = b"long-name payload";
    let mut h = tar::Header::new_gnu();
    h.set_size(body.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(1_000_000);
    builder.append_data(&mut h, long_name(), body.as_slice())?;

    let plain = b"plain";
    let mut p = tar::Header::new_gnu();
    p.set_size(plain.len() as u64);
    p.set_mode(0o644);
    p.set_mtime(1_000_000);
    builder.append_data(&mut p, "plain.txt", plain.as_slice())?;

    fs_err::write(path, builder.into_inner()?)?;
    Ok(())
}

/// Handcraft a pax `x` header + real entry whose pax records override the
/// path and mtime, followed by a plain entry.
fn write_pax_tar(path: &Utf8PathBuf) -> TestResult {
    let mut out: Vec<u8> = Vec::new();

    let records = b"30 path=pax/override-name.txt\n22 mtime=1234567890.5\n";
    let mut x = tar::Header::new_ustar();
    x.set_entry_type(tar::EntryType::XHeader);
    x.set_size(records.len() as u64);
    x.set_mode(0o644);
    x.set_mtime(0);
    x.set_path("paxheader")?;
    x.set_cksum();
    out.extend_from_slice(x.as_bytes());
    out.extend_from_slice(records);
    out.resize(out.len().next_multiple_of(512), 0);

    let body = b"pax payload";
    let mut real = tar::Header::new_ustar();
    real.set_size(body.len() as u64);
    real.set_mode(0o644);
    real.set_mtime(1_000_000);
    real.set_path("fallback-name.txt")?;
    real.set_cksum();
    out.extend_from_slice(real.as_bytes());
    out.extend_from_slice(body);
    out.resize(out.len().next_multiple_of(512), 0);

    let plain = b"plain";
    let mut p = tar::Header::new_ustar();
    p.set_size(plain.len() as u64);
    p.set_mode(0o644);
    p.set_mtime(1_000_000);
    p.set_path("plain.txt")?;
    p.set_cksum();
    out.extend_from_slice(p.as_bytes());
    out.extend_from_slice(plain);
    out.resize(out.len().next_multiple_of(512), 0);

    out.extend_from_slice(&[0u8; 1024]);
    fs_err::write(path, &out)?;
    Ok(())
}

#[test]
fn remove_matches_resolved_long_names_and_preserves_them() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = tmp.join("long.tar");
    write_long_name_tar(&archive)?;
    let before = fs_err::read(&archive)?;

    // No-match remove: byte-identical, long-name group intact.
    run(&tmp, &["remove", "long.tar", "nomatch"])?;
    assert_eq!(fs_err::read(&archive)?, before, "no-match remove must be a byte-level no-op");

    // Removing by the *resolved* long name works.
    run(&tmp, &["remove", "long.tar", &long_name()])?;
    let listing = run(&tmp, &["list", "long.tar"])?;
    let stdout = String::from_utf8_lossy(&listing.stdout);
    assert!(!stdout.contains("leaf.txt"), "long-name entry must be gone: {stdout}");
    assert!(stdout.lines().any(|l| l == "plain.txt"));
    Ok(())
}

#[test]
fn pax_records_survive_and_keep_matches_pax_path() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = tmp.join("pax.tar");
    write_pax_tar(&archive)?;
    let before = fs_err::read(&archive)?;

    // The listing resolves the pax path (tar-rs side), sanity-checking the
    // fixture.
    let listing = run(&tmp, &["list", "pax.tar"])?;
    let stdout = String::from_utf8_lossy(&listing.stdout).into_owned();
    assert!(
        stdout.lines().any(|l| l == "pax/override-name.txt"),
        "fixture listing: {stdout}",
    );

    // No-match remove: pax group carried through verbatim.
    run(&tmp, &["remove", "pax.tar", "nomatch"])?;
    assert_eq!(fs_err::read(&archive)?, before, "no-match remove must be a byte-level no-op");

    // Removing by the pax-resolved name drops the whole group.
    run(&tmp, &["remove", "pax.tar", "pax/override-name.txt"])?;
    let listing = run(&tmp, &["list", "pax.tar"])?;
    let stdout = String::from_utf8_lossy(&listing.stdout).into_owned();
    assert!(
        !stdout.contains("override-name") && !stdout.contains("fallback-name"),
        "pax group must be removed whole: {stdout}",
    );
    assert!(stdout.lines().any(|l| l == "plain.txt"));
    Ok(())
}

#[test]
fn append_after_pax_and_long_name_entries_keeps_them_readable() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = tmp.join("pax.tar");
    write_pax_tar(&archive)?;
    let extra = tmp.join("extra.txt");
    fs_err::write(&extra, "extra")?;

    run(&tmp, &["append", "pax.tar", extra.as_str()])?;
    let listing = run(&tmp, &["list", "pax.tar"])?;
    let stdout = String::from_utf8_lossy(&listing.stdout).into_owned();
    for name in ["pax/override-name.txt", "plain.txt", "extra.txt"] {
        assert!(stdout.lines().any(|l| l == name), "missing {name}: {stdout}");
    }

    let out_dir = tmp.join("out");
    run(&tmp, &["decompress", "pax.tar", "-o", out_dir.as_str()])?;
    assert_eq!(
        fs_err::read_to_string(out_dir.join("pax/override-name.txt"))?,
        "pax payload",
    );
    Ok(())
}
