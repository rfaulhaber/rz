#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io;

use camino::{Utf8Path, Utf8PathBuf};

/// Convenience alias — every integration test returns this.
pub type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Create a temporary directory and return the RAII guard + its UTF-8 path.
///
/// The guard must be held for the lifetime of the test; dropping it removes the
/// directory.
pub fn temp_utf8_dir() -> io::Result<(tempfile::TempDir, Utf8PathBuf)> {
    let dir = tempfile::TempDir::new()?;
    let path = dir
        .path()
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "non-UTF8 temp dir"))
        .map(Utf8PathBuf::from)?;
    Ok((dir, path))
}

/// Build a small, deterministic file tree for round-trip testing:
///
/// ```text
/// root/
///   hello.txt          — "hello world\n"  (12 bytes)
///   subdir/
///     nested.txt       — "nested content\n"  (15 bytes)
/// ```
pub fn build_file_tree(root: &Utf8Path) -> io::Result<()> {
    fs_err::create_dir_all(root.join("subdir"))?;
    fs_err::write(root.join("hello.txt"), b"hello world\n")?;
    fs_err::write(root.join("subdir/nested.txt"), b"nested content\n")?;
    Ok(())
}

/// Assert that two directory trees contain the same files with the same content.
///
/// Compares only regular-file contents keyed by relative path. Directory entries
/// and metadata (permissions, timestamps) are intentionally ignored — the point
/// is to verify that data survives a compress → decompress round trip.
pub fn assert_trees_match(expected: &Utf8Path, actual: &Utf8Path) -> TestResult {
    let expected_files = collect_files(expected)?;
    let actual_files = collect_files(actual)?;

    let expected_keys: Vec<_> = expected_files.keys().collect();
    let actual_keys: Vec<_> = actual_files.keys().collect();
    assert_eq!(expected_keys, actual_keys, "file set mismatch");

    for (rel, expected_content) in &expected_files {
        let actual_content = actual_files.get(rel);
        assert_eq!(
            Some(expected_content),
            actual_content,
            "content mismatch for {rel}",
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Recursively collect every regular file under `root` into a sorted map of
/// (relative-path → contents).
fn collect_files(root: &Utf8Path) -> io::Result<BTreeMap<String, Vec<u8>>> {
    let mut map = BTreeMap::new();
    visit_dir(root, root, &mut map)?;
    Ok(map)
}

fn visit_dir(
    base: &Utf8Path,
    dir: &Utf8Path,
    map: &mut BTreeMap<String, Vec<u8>>,
) -> io::Result<()> {
    for entry in fs_err::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let entry_path = entry.path();
        let s = entry_path
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "non-UTF8 path in tree"))?;
        let path = Utf8Path::new(s);

        if ft.is_dir() {
            visit_dir(base, path, map)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .map_err(|e| io::Error::other(e.to_string()))?;
            let content = fs_err::read(path)?;
            map.insert(rel.to_string(), content);
        }
    }
    Ok(())
}
