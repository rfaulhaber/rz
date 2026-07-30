//! Regression coverage for `7z --exclude-vcs-ignores`.
//!
//! `push_dir_vcs` used to hand each file to `push_source_path` one at a time,
//! passing the file itself as the source root — the library then derived the
//! archive name from that root, collapsing every entry to its bare file name.
//! Same-named files in different subdirectories collided in the archive and
//! only the last one written survived extraction.

mod helpers;

use std::process::Command;

use helpers::{TestResult, temp_utf8_dir};

fn rz_archive_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz")
}

/// Build:
///
/// ```text
/// vcs/
///   .git/                (empty — just enough for the `ignore` crate to
///                          treat `vcs/` as a repo root and honour .gitignore)
///   .gitignore            "ignored.txt"
///   ignored.txt
///   top.txt
///   sub1/dup.txt          "one"
///   sub2/dup.txt          "two"
/// ```
fn build_vcs_tree(root: &camino::Utf8Path) -> std::io::Result<()> {
    fs_err::create_dir_all(root.join(".git"))?;
    fs_err::create_dir_all(root.join("sub1"))?;
    fs_err::create_dir_all(root.join("sub2"))?;
    fs_err::write(root.join(".gitignore"), b"ignored.txt\n")?;
    fs_err::write(root.join("ignored.txt"), b"should not be archived\n")?;
    fs_err::write(root.join("top.txt"), b"top\n")?;
    fs_err::write(root.join("sub1/dup.txt"), b"one\n")?;
    fs_err::write(root.join("sub2/dup.txt"), b"two\n")?;
    Ok(())
}

#[test]
fn exclude_vcs_ignores_preserves_subdirectory_paths() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("vcs");
    build_vcs_tree(&tree)?;

    // Baseline: compress the same tree with no VCS awareness at all, so
    // nothing is excluded and every file is named the same way 7z always
    // names entries for a whole-directory input (no leading "vcs/").
    let plain_archive = tmp.join("plain.7z");
    let status = Command::new(rz_archive_bin())
        .args(["compress", "-o", plain_archive.as_str(), tree.as_str()])
        .status()?;
    assert!(status.success(), "plain compress failed: {status}");

    let plain_list = Command::new(rz_archive_bin())
        .args(["list", plain_archive.as_str()])
        .output()?;
    assert!(plain_list.status.success());
    let plain_stdout = String::from_utf8_lossy(&plain_list.stdout).into_owned();

    // With the flag: ignored.txt must be dropped, but sub1/dup.txt and
    // sub2/dup.txt must both survive with their full, distinct paths.
    let flagged_archive = tmp.join("flagged.7z");
    let status = Command::new(rz_archive_bin())
        .args([
            "compress",
            "--exclude-vcs-ignores",
            "-o",
            flagged_archive.as_str(),
            tree.as_str(),
        ])
        .status()?;
    assert!(status.success(), "flagged compress failed: {status}");

    let flagged_list = Command::new(rz_archive_bin())
        .args(["list", flagged_archive.as_str()])
        .output()?;
    assert!(flagged_list.status.success());
    let flagged_stdout = String::from_utf8_lossy(&flagged_list.stdout).into_owned();

    // Name parity: the flag must only change which files are included, never
    // how the surviving ones are named.
    for name in ["top.txt", "sub1/dup.txt", "sub2/dup.txt"] {
        assert!(
            plain_stdout.lines().any(|l| l == name),
            "baseline listing missing {name}: {plain_stdout:?}",
        );
        assert!(
            flagged_stdout.lines().any(|l| l == name),
            "flagged listing missing {name} (bug: names diverged or entries \
             were dropped): {flagged_stdout:?}",
        );
    }

    // The gitignore rule only takes effect with the flag.
    assert!(
        plain_stdout.lines().any(|l| l == "ignored.txt"),
        "baseline should still include ignored.txt: {plain_stdout:?}",
    );
    assert!(
        !flagged_stdout.lines().any(|l| l == "ignored.txt"),
        "flagged listing should have excluded ignored.txt: {flagged_stdout:?}",
    );

    // Decompress and verify both same-named files kept their own content.
    let out = tmp.join("out");
    let status = Command::new(rz_archive_bin())
        .args(["decompress", "-o", out.as_str(), flagged_archive.as_str()])
        .status()?;
    assert!(status.success(), "decompress failed: {status}");

    assert_eq!(fs_err::read_to_string(out.join("sub1/dup.txt"))?, "one\n");
    assert_eq!(fs_err::read_to_string(out.join("sub2/dup.txt"))?, "two\n");
    assert_eq!(fs_err::read_to_string(out.join("top.txt"))?, "top\n");
    assert!(
        !out.join("ignored.txt").exists(),
        "ignored.txt should not be extracted"
    );

    Ok(())
}
