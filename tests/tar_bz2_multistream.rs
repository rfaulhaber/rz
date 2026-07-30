//! Multi-stream `.tar.bz2` — the normal on-disk form produced by
//! pbzip2/lbzip2, which compress independent blocks as concatenated bzip2
//! streams.  The single-stream `BzDecoder` stopped at the first stream end,
//! making such archives unreadable ("unexpected EOF during skip") and — worse
//! — silently truncating modify's read-rewrite at the stream boundary.
#![cfg(feature = "bzip2")]

use std::io::Write;
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

/// Build a tar holding `first.txt` and `second.txt`, then compress it the way
/// pbzip2 does: split the *uncompressed* bytes and compress each half as an
/// independent bzip2 stream, concatenated.
fn multi_stream_tar_bz2(tmp: &Utf8PathBuf) -> Result<Utf8PathBuf, Box<dyn std::error::Error>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, content) in [("first.txt", "alpha"), ("second.txt", "beta")] {
        let path = tmp.join(name);
        fs_err::write(&path, content)?;
        let mut f = fs_err::File::open(&path)?;
        builder.append_file(name, f.file_mut())?;
    }
    let tar_bytes = builder.into_inner()?;

    let split = tar_bytes.len() / 2;
    let mut out = Vec::new();
    for chunk in [&tar_bytes[..split], &tar_bytes[split..]] {
        let mut enc = bzip2::write::BzEncoder::new(&mut out, bzip2::Compression::default());
        enc.write_all(chunk)?;
        enc.finish()?;
    }

    let archive = tmp.join("multi.tar.bz2");
    fs_err::write(&archive, &out)?;
    Ok(archive)
}

#[test]
fn list_test_info_read_all_streams() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = multi_stream_tar_bz2(&tmp)?;

    for sub in ["list", "test", "info"] {
        let out = Command::new(rz_bin()).arg(sub).arg(archive.as_str()).output()?;
        assert!(
            out.status.success(),
            "{sub} failed on multi-stream bz2: {}",
            String::from_utf8_lossy(&out.stderr),
        );
    }

    let out = Command::new(rz_bin()).arg("list").arg(archive.as_str()).output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("first.txt") && stdout.contains("second.txt"));
    Ok(())
}

#[test]
fn decompress_recovers_entries_from_both_streams() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = multi_stream_tar_bz2(&tmp)?;
    let out_dir = tmp.join("out");

    let out = Command::new(rz_bin())
        .args(["decompress", archive.as_str(), "-o", out_dir.as_str()])
        .output()?;
    assert!(
        out.status.success(),
        "decompress failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(fs_err::read_to_string(out_dir.join("first.txt"))?, "alpha");
    assert_eq!(fs_err::read_to_string(out_dir.join("second.txt"))?, "beta");
    Ok(())
}

#[test]
fn append_preserves_entries_from_both_streams() -> TestResult {
    let (_guard, tmp) = temp_dir()?;
    let archive = multi_stream_tar_bz2(&tmp)?;
    let extra = tmp.join("third.txt");
    fs_err::write(&extra, "gamma")?;

    let out = Command::new(rz_bin())
        .args(["append", archive.as_str(), extra.as_str()])
        .output()?;
    assert!(
        out.status.success(),
        "append failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );

    let list = Command::new(rz_bin()).arg("list").arg(archive.as_str()).output()?;
    let stdout = String::from_utf8_lossy(&list.stdout);
    for name in ["first.txt", "second.txt", "third.txt"] {
        assert!(stdout.contains(name), "entry {name} missing after append: {stdout}");
    }
    Ok(())
}
