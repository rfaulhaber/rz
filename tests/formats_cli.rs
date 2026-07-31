//! `rz formats` must advertise exactly the ids and extensions the rest of the
//! CLI accepts — the table once drifted (`tar-cz` for `.tar.xz`, `7z` for the
//! clap id `seven-z`) and fed users format ids `--format` rejects.

use std::collections::BTreeSet;
use std::process::Command;

use clap::ValueEnum;
use rz_archive::cmd::Format;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rz_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz")
}

#[test]
fn formats_json_ids_and_extensions_match_the_parser() -> TestResult {
    let out = Command::new(rz_bin()).args(["formats", "--json"]).output()?;
    assert!(
        out.status.success(),
        "formats --json failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );

    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout)?;

    let listed: BTreeSet<(String, String)> = rows
        .iter()
        .map(|row| {
            let fmt = row["format"].as_str().unwrap_or_default().to_owned();
            let ext = row["extension"].as_str().unwrap_or_default().to_owned();
            (fmt, ext)
        })
        .collect();
    let expected: BTreeSet<(String, String)> = Format::value_variants()
        .iter()
        .map(|f| (f.to_string(), f.extension().to_owned()))
        .collect();
    assert_eq!(
        listed, expected,
        "formats output does not match the Format enum",
    );

    // Every advertised id must be accepted by the `--format` value parser.
    for (id, _) in &listed {
        assert!(
            Format::from_str(id, true).is_ok(),
            "formats advertises `{id}`, which --format rejects",
        );
    }
    Ok(())
}

/// `rz info` must report format ids `--format` accepts, so its output can be
/// fed straight back into another rz invocation; `tar.gz`-style ids and the
/// derived `seven-z` spelling both used to break that round-trip.
#[test]
fn info_format_ids_round_trip_into_format_flag() -> TestResult {
    let guard = tempfile::tempdir()?;
    let tmp = camino::Utf8PathBuf::try_from(guard.path().to_path_buf())
        .map_err(|e| format!("non-UTF-8 tempdir: {e}"))?;
    fs_err::write(tmp.join("a.txt"), "hello")?;

    for archive in ["out.tar.gz", "out.7z", "out.zip"] {
        let ok = Command::new(rz_bin())
            .current_dir(tmp.as_std_path())
            .args(["compress", "a.txt", "-o", archive])
            .output()?;
        assert!(ok.status.success());

        let info = Command::new(rz_bin())
            .current_dir(tmp.as_std_path())
            .args(["info", archive, "--json"])
            .output()?;
        assert!(info.status.success());
        let parsed: serde_json::Value = serde_json::from_slice(&info.stdout)?;
        let id = parsed["format"].as_str().ok_or("no format field")?;

        assert!(
            Format::from_str(id, true).is_ok(),
            "info reports `{id}`, which --format rejects",
        );
        let relist = Command::new(rz_bin())
            .current_dir(tmp.as_std_path())
            .args(["list", archive, "--format", id])
            .output()?;
        assert!(
            relist.status.success(),
            "list --format {id} failed: {}",
            String::from_utf8_lossy(&relist.stderr),
        );
    }
    Ok(())
}

#[test]
fn formats_table_lists_every_variant() -> TestResult {
    let out = Command::new(rz_bin()).arg("formats").output()?;
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for fmt in Format::value_variants() {
        assert!(
            stdout.contains(&fmt.to_string()),
            "plain table is missing `{fmt}`",
        );
    }
    Ok(())
}
