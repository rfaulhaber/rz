//! Regression coverage for two defects in `push_dir_vcs`'s `--exclude-vcs-ignores`
//! walk that made it disagree with the non-flag directory walk it is supposed
//! to be a drop-in replacement for:
//!
//! 1. Excludes were matched against the archive-relative name inside
//!    `push_dir_vcs`, while the non-flag path (`push_source_path`) matches its
//!    filter closure against the raw walked filesystem path. A
//!    slash-containing pattern is anchored (see `filter::build_glob_set`), so
//!    the two branches disagreed on what a pattern like `dir/sub/*` excluded.
//! 2. The flagged walk archived symlinks that the built-in non-flag walker
//!    always drops (it decides whether to recurse from a non-following
//!    `DirEntry::file_type`, so it never dereferences or surfaces a symlink,
//!    regardless of `--follow-symlinks`). A symlink-to-file got silently
//!    dereferenced, a symlink-to-dir became an empty directory entry with its
//!    contents dropped, and a dangling symlink hard-failed the whole compress.

mod helpers;

use std::os::unix::fs::symlink;
use std::process::Command;

use camino::Utf8Path;
use helpers::{TestResult, temp_utf8_dir};

fn rz_archive_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rz")
}

/// List an archive's entry names as a sorted `Vec`, shelling out to the
/// compiled binary so the process working directory can be set without
/// racing the other tests in this process.
fn list_names(dir: &Utf8Path, archive: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let output = Command::new(rz_archive_bin())
        .current_dir(dir.as_std_path())
        .args(["list", archive])
        .output()?;
    assert!(
        output.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let mut names: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    names.sort();
    Ok(names)
}

/// A labelled entry-name listing produced by one `compress_both_ways` run.
type Listing = (&'static str, Vec<String>);

/// Compress `vcs` (a relative path, run from `dir`) once without and once
/// with `--exclude-vcs-ignores`, plus whatever extra args the caller passes,
/// returning the two resulting name listings in that order.
fn compress_both_ways(
    dir: &Utf8Path,
    extra_args: &[&str],
) -> Result<[Listing; 2], Box<dyn std::error::Error>> {
    let mut listings = Vec::new();
    for (label, flag) in [("without", None), ("with", Some("--exclude-vcs-ignores"))] {
        let archive = format!("out-{label}.7z");
        let mut args = vec!["compress", "-o", archive.as_str()];
        args.extend_from_slice(extra_args);
        if let Some(f) = flag {
            args.push(f);
        }
        args.push("vcs");
        let status = Command::new(rz_archive_bin())
            .current_dir(dir.as_std_path())
            .args(&args)
            .status()?;
        assert!(status.success(), "compress ({label} flag) failed: {status}");
        listings.push((label, list_names(dir, &archive)?));
    }
    Ok([listings[0].clone(), listings[1].clone()])
}

/// Pre-fix, `--exclude 'vcs/sub1/*'` excluded `sub1` in both modes. The
/// regression matched the flagged walk's excludes against the
/// archive-relative name (no `vcs/` prefix) while the non-flag walk matches
/// against the raw path (`vcs/sub1/...`), so the anchored, slash-containing
/// pattern silently stopped excluding anything once `--exclude-vcs-ignores`
/// was added.
#[test]
fn exclude_pattern_with_slash_matches_the_same_entries_with_and_without_the_flag() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("vcs");
    fs_err::create_dir_all(tree.join("sub1"))?;
    fs_err::create_dir_all(tree.join("sub2"))?;
    fs_err::write(tree.join("top.txt"), b"top\n")?;
    fs_err::write(tree.join("sub1/dup.txt"), b"one\n")?;
    fs_err::write(tree.join("sub2/dup.txt"), b"two\n")?;

    let [(without_label, without_names), (with_label, with_names)] =
        compress_both_ways(&tmp, &["--exclude", "vcs/sub1/*"])?;

    assert_eq!(
        without_names, with_names,
        "`--exclude 'vcs/sub1/*'` produced different name sets depending on \
         --exclude-vcs-ignores: {without_label}={without_names:?} vs \
         {with_label}={with_names:?}",
    );
    assert!(
        !with_names.iter().any(|n| n.contains("sub1")),
        "sub1 should have been excluded by `vcs/sub1/*` in both modes, got {with_names:?}",
    );
    assert!(
        with_names.iter().any(|n| n == "top.txt") && with_names.iter().any(|n| n == "sub2/dup.txt"),
        "unrelated entries should survive the exclude, got {with_names:?}",
    );

    Ok(())
}

/// Pre-fix, the flagged walk (`push_dir_vcs`) dereferenced a symlink-to-file,
/// turned a symlink-to-dir into an empty directory entry whose contents were
/// silently dropped, and hard-failed the whole compress on a dangling
/// symlink — none of which match the non-flag walk, which drops every
/// symlink outright.
#[test]
fn symlinks_match_the_non_flag_walk_and_a_dangling_symlink_does_not_fail_compress() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("vcs");
    fs_err::create_dir_all(tree.join("realdir"))?;
    fs_err::write(tree.join("real.txt"), b"hello\n")?;
    fs_err::write(tree.join("realdir/inside.txt"), b"nested\n")?;
    symlink("real.txt", tree.join("link_file.txt").as_std_path())?;
    symlink("realdir", tree.join("link_dir").as_std_path())?;
    symlink("missing_target", tree.join("dangling").as_std_path())?;

    let [(without_label, without_names), (with_label, with_names)] = compress_both_ways(&tmp, &[])?;

    assert_eq!(
        without_names, with_names,
        "flagged and non-flagged walks archived a different set of entries: \
         {without_label}={without_names:?} vs {with_label}={with_names:?}",
    );
    assert_eq!(
        with_names,
        vec!["real.txt".to_owned(), "realdir/inside.txt".to_owned()],
        "expected only the two real files (every symlink dropped), got {with_names:?}",
    );

    Ok(())
}
