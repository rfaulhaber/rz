#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use globset::GlobSet;

use rz::{CompressOpts, DecompressOpts};

/// Convenience alias — every integration test returns this.
pub type TestResult = Result<(), Box<dyn std::error::Error>>;

// ── Format harness ───────────────────────────────────────────────────────────

/// Test harness that captures the four operations every format implements.
///
/// Create one `const` per format, then call the helper methods to run
/// standardised round-trip / list / info tests without duplicating the
/// boilerplate.
pub struct FormatHarness {
    pub compress: fn(&[Utf8PathBuf], &Utf8Path, &CompressOpts) -> rz::error::Result<()>,
    pub decompress: fn(&Utf8Path, &Utf8Path, &DecompressOpts) -> rz::error::Result<()>,
    pub list: fn(&Utf8Path) -> rz::error::Result<Vec<rz::Entry>>,
    pub info: fn(&Utf8Path) -> rz::error::Result<rz::ArchiveInfo>,
    pub ext: &'static str,
    pub format_name: &'static str,
    /// tar-style formats preserve the top-level directory name in the archive
    /// and require the output directory to already exist before decompressing.
    /// 7z-style formats extract contents flat and create the output directory
    /// themselves.
    pub preserves_top_dir: bool,
}

pub const ZIP: FormatHarness = FormatHarness {
    compress: rz::zip::compress,
    decompress: rz::zip::decompress,
    list: rz::zip::list,
    info: rz::zip::info,
    ext: ".zip",
    format_name: "zip",
    preserves_top_dir: true,
};

pub const TAR: FormatHarness = FormatHarness {
    compress: rz::tar::compress,
    decompress: rz::tar::decompress,
    list: rz::tar::list,
    info: rz::tar::info,
    ext: ".tar",
    format_name: "tar",
    preserves_top_dir: true,
};

pub const TAR_GZ: FormatHarness = FormatHarness {
    compress: rz::tar_gz::compress,
    decompress: rz::tar_gz::decompress,
    list: rz::tar_gz::list,
    info: rz::tar_gz::info,
    ext: ".tar.gz",
    format_name: "tar.gz",
    preserves_top_dir: true,
};

pub const TAR_XZ: FormatHarness = FormatHarness {
    compress: rz::tar_xz::compress,
    decompress: rz::tar_xz::decompress,
    list: rz::tar_xz::list,
    info: rz::tar_xz::info,
    ext: ".tar.xz",
    format_name: "tar.xz",
    preserves_top_dir: true,
};

pub const TAR_ZST: FormatHarness = FormatHarness {
    compress: rz::tar_zst::compress,
    decompress: rz::tar_zst::decompress,
    list: rz::tar_zst::list,
    info: rz::tar_zst::info,
    ext: ".tar.zst",
    format_name: "tar.zst",
    preserves_top_dir: true,
};

#[cfg(feature = "bzip2")]
pub const TAR_BZ2: FormatHarness = FormatHarness {
    compress: rz::tar_bz2::compress,
    decompress: rz::tar_bz2::decompress,
    list: rz::tar_bz2::list,
    info: rz::tar_bz2::info,
    ext: ".tar.bz2",
    format_name: "tar.bz2",
    preserves_top_dir: true,
};

pub const SEVEN_Z: FormatHarness = FormatHarness {
    compress: rz::seven_z::compress,
    decompress: rz::seven_z::decompress,
    list: rz::seven_z::list,
    info: rz::seven_z::info,
    ext: ".7z",
    format_name: "7z",
    preserves_top_dir: false,
};

/// Build default compress opts (no excludes).
pub fn default_compress_opts(level: Option<u32>) -> CompressOpts {
    CompressOpts {
        level,
        excludes: GlobSet::empty(),
    }
}

/// Build default decompress opts (no strip, no excludes, no overwrite).
pub fn default_decompress_opts() -> DecompressOpts {
    DecompressOpts {
        force: false,
        strip_components: 0,
        excludes: GlobSet::empty(),
    }
}

impl FormatHarness {
    /// Compress a directory tree, decompress it, and verify the contents match.
    pub fn round_trip_directory(&self, level: Option<u32>) -> TestResult {
        let (_guard, tmp) = temp_utf8_dir()?;

        let tree = tmp.join("tree");
        build_file_tree(&tree)?;

        let archive = tmp.join(format!("archive{}", self.ext));
        (self.compress)(std::slice::from_ref(&tree), &archive, &default_compress_opts(level))?;

        let out = tmp.join("out");
        if self.preserves_top_dir {
            fs_err::create_dir(&out)?;
        }
        (self.decompress)(&archive, &out, &default_decompress_opts())?;

        let extracted = if self.preserves_top_dir {
            out.join("tree")
        } else {
            out
        };
        assert_trees_match(&tree, &extracted)?;
        Ok(())
    }

    /// Compress a single file, decompress it, and verify the content matches.
    pub fn round_trip_single_file(&self) -> TestResult {
        let (_guard, tmp) = temp_utf8_dir()?;

        let file = tmp.join("single.txt");
        fs_err::write(&file, b"single file content\n")?;

        let archive = tmp.join(format!("archive{}", self.ext));
        (self.compress)(&[file], &archive, &default_compress_opts(None))?;

        let out = tmp.join("out");
        if self.preserves_top_dir {
            fs_err::create_dir(&out)?;
        }
        (self.decompress)(&archive, &out, &default_decompress_opts())?;

        let extracted = fs_err::read_to_string(out.join("single.txt"))?;
        assert_eq!(extracted, "single file content\n");
        Ok(())
    }

    /// List archive entries and verify the expected files are present with
    /// correct sizes.
    pub fn list_contains_expected_entries(&self) -> TestResult {
        let (_guard, tmp) = temp_utf8_dir()?;

        let tree = tmp.join("tree");
        build_file_tree(&tree)?;

        let archive = tmp.join(format!("archive{}", self.ext));
        (self.compress)(&[tree], &archive, &default_compress_opts(None))?;

        let entries = (self.list)(&archive)?;
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();

        let has_hello = paths.iter().any(|p| p.ends_with("hello.txt"));
        let has_nested = paths.iter().any(|p| p.ends_with("nested.txt"));
        assert!(has_hello, "missing hello.txt in {paths:?}");
        assert!(has_nested, "missing nested.txt in {paths:?}");

        // Verify sizes where available (some formats report 0 for sizes)
        for entry in &entries {
            if entry.path.as_str().ends_with("hello.txt") && entry.size > 0 {
                assert_eq!(entry.size, 12, "hello.txt size mismatch");
                assert!(!entry.is_dir);
            }
            if entry.path.as_str().ends_with("nested.txt") && entry.size > 0 {
                assert_eq!(entry.size, 15, "nested.txt size mismatch");
                assert!(!entry.is_dir);
            }
        }
        Ok(())
    }

    /// Verify archive metadata (format name, entry count, sizes).
    pub fn info_reports_correct_metadata(&self) -> TestResult {
        let (_guard, tmp) = temp_utf8_dir()?;

        let tree = tmp.join("tree");
        build_file_tree(&tree)?;

        let archive = tmp.join(format!("archive{}", self.ext));
        (self.compress)(&[tree], &archive, &default_compress_opts(None))?;

        let info = (self.info)(&archive)?;

        assert_eq!(info.format, self.format_name);
        assert!(
            info.entry_count >= 2,
            "expected >= 2 entries, got {}",
            info.entry_count,
        );
        assert!(info.compressed_size > 0);
        Ok(())
    }
}

// ── Utilities ────────────────────────────────────────────────────────────────

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
