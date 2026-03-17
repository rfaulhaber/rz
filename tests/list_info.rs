mod helpers;

use helpers::{build_file_tree, temp_utf8_dir, TestResult};

// ── tar.gz list ──────────────────────────────────────────────────────────────

#[test]
fn tar_gz_list_contains_expected_entries() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let archive = tmp.join("archive.tar.gz");
    rz::tar_gz::compress(&[tree], &archive, None)?;

    let entries = rz::tar_gz::list(&archive)?;
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();

    assert!(paths.contains(&"tree/hello.txt"), "missing hello.txt in {paths:?}");
    assert!(
        paths.contains(&"tree/subdir/nested.txt"),
        "missing nested.txt in {paths:?}",
    );

    // Verify sizes on the file entries
    for entry in &entries {
        if entry.path.as_str() == "tree/hello.txt" {
            assert_eq!(entry.size, 12); // b"hello world\n"
            assert!(!entry.is_dir);
        }
        if entry.path.as_str() == "tree/subdir/nested.txt" {
            assert_eq!(entry.size, 15); // b"nested content\n"
            assert!(!entry.is_dir);
        }
    }
    Ok(())
}

// ── tar.gz info ──────────────────────────────────────────────────────────────

#[test]
fn tar_gz_info_reports_correct_metadata() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let archive = tmp.join("archive.tar.gz");
    rz::tar_gz::compress(&[tree], &archive, None)?;

    let info = rz::tar_gz::info(&archive)?;

    assert_eq!(info.format, "tar.gz");
    // At least the two files, plus directory entries
    assert!(info.entry_count >= 2, "expected >= 2 entries, got {}", info.entry_count);
    // At least 27 bytes total (12 + 15 from the two files)
    assert!(
        info.total_uncompressed >= 27,
        "expected >= 27 bytes uncompressed, got {}",
        info.total_uncompressed,
    );
    assert!(info.compressed_size > 0);
    Ok(())
}

// ── 7z list ──────────────────────────────────────────────────────────────────

#[test]
fn seven_z_list_contains_expected_entries() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let archive = tmp.join("archive.7z");
    rz::seven_z::compress(&[tree], &archive, None)?;

    let entries = rz::seven_z::list(&archive)?;
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();

    // 7z implementations may use different path conventions; check suffixes
    let has_hello = paths.iter().any(|p| p.ends_with("hello.txt"));
    let has_nested = paths.iter().any(|p| p.ends_with("nested.txt"));
    assert!(has_hello, "missing hello.txt in {paths:?}");
    assert!(has_nested, "missing nested.txt in {paths:?}");
    Ok(())
}

// ── 7z info ──────────────────────────────────────────────────────────────────

#[test]
fn seven_z_info_reports_correct_metadata() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let archive = tmp.join("archive.7z");
    rz::seven_z::compress(&[tree], &archive, None)?;

    let info = rz::seven_z::info(&archive)?;

    assert_eq!(info.format, "7z");
    assert!(info.entry_count >= 2, "expected >= 2 entries, got {}", info.entry_count);
    assert!(info.compressed_size > 0);
    Ok(())
}
