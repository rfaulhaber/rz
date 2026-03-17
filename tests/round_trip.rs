mod helpers;

use helpers::{assert_trees_match, build_file_tree, temp_utf8_dir, TestResult};

// ── tar.gz ───────────────────────────────────────────────────────────────────

#[test]
fn tar_gz_round_trip_directory() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let archive = tmp.join("archive.tar.gz");
    rz::tar_gz::compress(std::slice::from_ref(&tree), &archive, None)?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz::tar_gz::decompress(&archive, &out, false)?;

    assert_trees_match(&tree, &out.join("tree"))?;
    Ok(())
}

#[test]
fn tar_gz_round_trip_single_file() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let file = tmp.join("single.txt");
    fs_err::write(&file, b"single file content\n")?;

    let archive = tmp.join("archive.tar.gz");
    rz::tar_gz::compress(&[file], &archive, None)?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz::tar_gz::decompress(&archive, &out, false)?;

    let extracted = fs_err::read_to_string(out.join("single.txt"))?;
    assert_eq!(extracted, "single file content\n");
    Ok(())
}

#[test]
fn tar_gz_round_trip_custom_level() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let archive = tmp.join("archive.tar.gz");
    rz::tar_gz::compress(std::slice::from_ref(&tree), &archive, Some(9))?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz::tar_gz::decompress(&archive, &out, false)?;

    assert_trees_match(&tree, &out.join("tree"))?;
    Ok(())
}

// ── 7z ───────────────────────────────────────────────────────────────────────

#[test]
fn seven_z_round_trip_directory() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;

    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let archive = tmp.join("archive.7z");
    rz::seven_z::compress(std::slice::from_ref(&tree), &archive, None)?;

    let out = tmp.join("out");
    rz::seven_z::decompress(&archive, &out, false)?;

    // sevenz_rust2 stores directory contents directly (no wrapping directory),
    // so files extract into `out/` rather than `out/tree/`.
    assert_trees_match(&tree, &out)?;
    Ok(())
}
