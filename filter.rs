use camino::{Utf8Path, Utf8PathBuf};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::error::{Error, Result};
use crate::progress::ProgressReport;
use crate::DecompressOpts;

/// Returns `true` when an entry at `path` should be extracted, considering
/// both include and exclude filters.  When includes are non-empty, the path
/// must match at least one include pattern.  Excludes are always applied
/// afterward.
pub fn should_extract(path: &str, includes: &GlobSet, excludes: &GlobSet) -> bool {
    let clean = path.trim_end_matches('/');
    if !includes.is_empty() && !includes.is_match(clean) {
        return false;
    }
    if !excludes.is_empty() && excludes.is_match(clean) {
        return false;
    }
    true
}

/// Build a [`GlobSet`] from include patterns.
///
/// Uses the same `**/` prefix logic as [`build_exclude_set`] so bare patterns
/// match at any directory depth.
pub fn build_include_set(patterns: &[String]) -> Result<GlobSet> {
    // Reuse the same glob-building logic as excludes.
    build_glob_set(patterns)
}

/// Build a [`GlobSet`] from exclude patterns.
///
/// Patterns without a `/` are automatically prefixed with `**/` so they match
/// at any directory depth (matching `tar --exclude` behaviour).  Each pattern
/// also generates a `<pattern>/**` variant so that excluding a directory name
/// also excludes everything inside it.
pub fn build_exclude_set(patterns: &[String]) -> Result<GlobSet> {
    build_glob_set(patterns)
}

/// Build a [`GlobSet`] from glob patterns.
///
/// Bare patterns (without `/`) are prefixed with `**/` for recursive matching.
/// Each pattern also generates a `<pattern>/**` variant to match directory
/// contents.
fn build_glob_set(patterns: &[String]) -> Result<GlobSet> {
    if patterns.is_empty() {
        return Ok(GlobSet::empty());
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let effective = if pattern.contains('/') {
            pattern.clone()
        } else {
            format!("**/{pattern}")
        };
        let glob = GlobBuilder::new(&effective)
            .literal_separator(true)
            .build()
            .map_err(|e| Error::InvalidExcludePattern(e.to_string()))?;
        builder.add(glob);

        // Also match contents of matching directories.
        let dir_glob = GlobBuilder::new(&format!("{effective}/**"))
            .literal_separator(true)
            .build()
            .map_err(|e| Error::InvalidExcludePattern(e.to_string()))?;
        builder.add(dir_glob);
    }
    builder
        .build()
        .map_err(|e| Error::InvalidExcludePattern(e.to_string()))
}

/// Strip the first `n` path components from a UTF-8 path.
///
/// Returns [`None`] when the path has fewer components than `n`, or when
/// stripping leaves an empty remainder (e.g. `strip_components("dir/", 1)`).
pub fn strip_components(path: &Utf8Path, n: u32) -> Option<Utf8PathBuf> {
    if n == 0 {
        return Some(path.to_owned());
    }
    let mut components = path.components();
    for _ in 0..n {
        components.next()?;
    }
    let remaining = components.as_path();
    if remaining.as_str().is_empty() {
        None
    } else {
        Some(remaining.to_owned())
    }
}

// ── Stdout extraction ────────────────────────────────────────────────────────

/// Extract matching tar entries to a writer (typically stdout), skipping
/// directory entries.  Applies include/exclude filters and strip-components.
pub fn extract_tar_to_writer<R: std::io::Read, W: std::io::Write>(
    archive: &mut tar::Archive<R>,
    writer: &mut W,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    for entry in archive.entries()? {
        let mut entry = entry?;
        let orig_path = entry.path()?;
        let orig_path = Utf8PathBuf::try_from(orig_path.into_owned())
            .map_err(|e| Error::InvalidUtf8Path(e.into_path_buf().display().to_string()))?;

        if !should_extract(orig_path.as_str(), &opts.includes, &opts.excludes) {
            continue;
        }

        let stripped = match strip_components(&orig_path, opts.strip_components) {
            Some(p) => p,
            None => continue,
        };

        // Skip directory entries — only files have content.
        if entry.header().entry_type().is_dir() {
            continue;
        }

        opts.progress.set_entry(stripped.as_str());
        std::io::copy(&mut entry, writer)?;
    }
    Ok(())
}

// ── Tar helpers ──────────────────────────────────────────────────────────────

/// Fully decompress every entry in a tar archive to [`io::sink`], verifying
/// data integrity beyond what header-only iteration (`list`) provides.
pub fn verify_tar_entries<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
    progress: &dyn ProgressReport,
) -> Result<()> {
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let path = Utf8PathBuf::try_from(path.into_owned())
            .map_err(|e| Error::InvalidUtf8Path(e.into_path_buf().display().to_string()))?;
        progress.set_entry(path.as_str());
        let written = std::io::copy(&mut entry, &mut std::io::sink())?;
        progress.inc(written);
    }
    Ok(())
}

/// Walk a directory tree and append entries to a tar builder, skipping paths
/// that match the exclude set.  Reports progress per file via `progress`.
pub fn append_dir_filtered<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    dir: &Utf8Path,
    prefix: &str,
    excludes: &GlobSet,
    progress: &dyn ProgressReport,
) -> Result<()> {
    builder.append_dir(prefix, dir)?;

    let mut entries: Vec<_> =
        fs_err::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let entry_path = entry.path();
        let file_name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::InvalidUtf8Path(entry_path.display().to_string()))?;
        let archive_name = format!("{prefix}/{file_name}");

        if excludes.is_match(&archive_name) {
            continue;
        }

        let entry_str = entry_path
            .to_str()
            .ok_or_else(|| Error::InvalidUtf8Path(entry_path.display().to_string()))?;
        let utf8_path = Utf8Path::new(entry_str);

        if entry.file_type()?.is_dir() {
            append_dir_filtered(builder, utf8_path, &archive_name, excludes, progress)?;
        } else {
            let meta = fs_err::metadata(utf8_path)?;
            builder.append_path_with_name(utf8_path, &archive_name)?;
            progress.set_entry(&archive_name);
            progress.inc(meta.len());
        }
    }
    Ok(())
}

/// Extract entries from a tar archive, honouring exclude patterns and
/// path-component stripping.  Reports progress per entry.
pub fn unpack_tar_filtered<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
    output: &Utf8Path,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    for entry in archive.entries()? {
        let mut entry = entry?;
        let orig_path = entry.path()?;
        let orig_path = Utf8PathBuf::try_from(orig_path.into_owned())
            .map_err(|e| Error::InvalidUtf8Path(e.into_path_buf().display().to_string()))?;

        // Include/exclude check against the original (pre-strip) path.
        if !should_extract(orig_path.as_str(), &opts.includes, &opts.excludes) {
            continue;
        }

        let stripped = match strip_components(&orig_path, opts.strip_components) {
            Some(p) => p,
            None => continue,
        };

        let dest = output.join(&stripped);

        // Ensure parent directories exist.
        if let Some(parent) = dest.parent()
            && !parent.as_str().is_empty()
        {
            fs_err::create_dir_all(parent)?;
        }

        // Overwrite guard for non-directory entries.
        if !opts.force
            && !entry.header().entry_type().is_dir()
            && fs_err::symlink_metadata(&dest).is_ok()
        {
            return Err(Error::FileExists(dest));
        }

        opts.progress.set_entry(stripped.as_str());
        entry.unpack(&dest)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use super::*;

    // ── strip_components ─────────────────────────────────────────────────

    #[test]
    fn strip_zero_is_identity() {
        let p = Utf8Path::new("a/b/c");
        assert_eq!(strip_components(p, 0), Some(p.to_owned()));
    }

    #[test]
    fn strip_one() {
        assert_eq!(
            strip_components(Utf8Path::new("project/src/main.rs"), 1),
            Some(Utf8PathBuf::from("src/main.rs")),
        );
    }

    #[test]
    fn strip_all_returns_none() {
        assert_eq!(strip_components(Utf8Path::new("a/b"), 2), None);
    }

    #[test]
    fn strip_more_than_depth_returns_none() {
        assert_eq!(strip_components(Utf8Path::new("a"), 2), None);
    }

    #[test]
    fn strip_dir_entry_with_trailing_slash() {
        // "a/b/" → strip 1 → "b/" which camino normalises to "b"
        assert_eq!(
            strip_components(Utf8Path::new("a/b/"), 1),
            Some(Utf8PathBuf::from("b")),
        );
    }

    #[test]
    fn strip_dot_prefix() {
        // "./dir/file" → strip 1 → "dir/file"
        assert_eq!(
            strip_components(Utf8Path::new("./dir/file"), 1),
            Some(Utf8PathBuf::from("dir/file")),
        );
    }

    // ── build_exclude_set ────────────────────────────────────────────────

    #[test]
    fn empty_patterns_never_match() {
        let set = build_exclude_set(&[]).ok();
        assert!(set.is_some());
        let set = set.map(|s| s.is_match("anything"));
        assert_eq!(set, Some(false));
    }

    #[test]
    fn star_pattern_matches_at_any_depth() {
        let set = build_exclude_set(&["*.log".to_owned()]).ok();
        assert!(set.is_some());
        let set = set.as_ref().map(|s| s.is_match("foo.log"));
        assert_eq!(set, Some(true));
        let set2 = build_exclude_set(&["*.log".to_owned()]).ok();
        let set2 = set2.as_ref().map(|s| s.is_match("dir/foo.log"));
        assert_eq!(set2, Some(true));
    }

    #[test]
    fn directory_name_excludes_children() {
        let set = build_exclude_set(&["node_modules".to_owned()]).ok();
        assert!(set.is_some());
        let s = set.as_ref();
        assert_eq!(s.map(|s| s.is_match("node_modules")), Some(true));
        assert_eq!(
            s.map(|s| s.is_match("node_modules/package.json")),
            Some(true),
        );
        assert_eq!(
            s.map(|s| s.is_match("src/node_modules/foo")),
            Some(true),
        );
        assert_eq!(s.map(|s| s.is_match("src/other")), Some(false));
    }
}
