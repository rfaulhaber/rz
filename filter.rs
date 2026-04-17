use std::io::BufRead;

use camino::{Utf8Path, Utf8PathBuf};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::error::{Error, Result};
use crate::progress::ProgressReport;
use crate::{CompressOpts, DecompressOpts};

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

// ── VCS-aware walking ───────────────────────────────────────────────────────

/// Build an `ignore::Walk` iterator that respects `.gitignore` rules.
///
/// This is the single source of truth for VCS-aware walking configuration.
/// Used by tar compress, zip compress, 7z compress, and dry-run collection.
pub fn vcs_walker(dir: &Utf8Path, follow_symlinks: bool) -> ignore::Walk {
    ignore::WalkBuilder::new(dir.as_std_path())
        .standard_filters(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(follow_symlinks)
        .sort_by_file_name(|a, b| a.cmp(b))
        .build()
}

// ── Shared directory walker ────────────────────────────────────────────────

/// An entry discovered during a directory walk.
pub struct WalkEntry {
    /// Path this entry will have inside the archive.
    pub archive_name: String,
    /// Absolute path on the filesystem.
    pub fs_path: Utf8PathBuf,
    /// Whether this entry is a directory.
    pub is_dir: bool,
}

/// Walk a directory tree and call `visit` for each entry, applying exclude
/// filters and honouring `follow_symlinks`.
///
/// When `opts.exclude_vcs_ignores` is set the walk respects `.gitignore`
/// rules via the `ignore` crate.  Entries are yielded in sorted order for
/// deterministic archive output.
///
/// This is the single source of walking logic used by tar compress, zip
/// compress, 7z compress, and dry-run collection.
pub fn walk_dir<F>(
    dir: &Utf8Path,
    prefix: &str,
    opts: &CompressOpts<'_>,
    visit: &mut F,
) -> Result<()>
where
    F: FnMut(WalkEntry) -> Result<()>,
{
    if opts.exclude_vcs_ignores {
        walk_dir_vcs(dir, prefix, &opts.excludes, opts.follow_symlinks, visit)
    } else {
        // Yield the root directory entry, then recurse.
        visit(WalkEntry {
            archive_name: prefix.to_owned(),
            fs_path: dir.to_owned(),
            is_dir: true,
        })?;
        walk_dir_simple(dir, prefix, &opts.excludes, opts.follow_symlinks, visit)
    }
}

/// Standard directory walk (no VCS-ignore awareness).
fn walk_dir_simple<F>(
    dir: &Utf8Path,
    prefix: &str,
    excludes: &GlobSet,
    follow_symlinks: bool,
    visit: &mut F,
) -> Result<()>
where
    F: FnMut(WalkEntry) -> Result<()>,
{
    let mut entries: Vec<_> = fs_err::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
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

        let is_dir = if follow_symlinks {
            fs_err::metadata(utf8_path)?.is_dir()
        } else {
            entry.file_type()?.is_dir()
        };

        visit(WalkEntry {
            archive_name: archive_name.clone(),
            fs_path: utf8_path.to_owned(),
            is_dir,
        })?;

        if is_dir {
            walk_dir_simple(utf8_path, &archive_name, excludes, follow_symlinks, visit)?;
        }
    }
    Ok(())
}

/// Walk a directory using the `ignore` crate to respect `.gitignore` rules.
fn walk_dir_vcs<F>(
    dir: &Utf8Path,
    prefix: &str,
    excludes: &GlobSet,
    follow_symlinks: bool,
    visit: &mut F,
) -> Result<()>
where
    F: FnMut(WalkEntry) -> Result<()>,
{
    for result in vcs_walker(dir, follow_symlinks) {
        let entry = result.map_err(|e| std::io::Error::other(e.to_string()))?;
        let fs_path = entry.path();

        let relative = fs_path
            .strip_prefix(dir.as_std_path())
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // Root directory entry.
        if relative.as_os_str().is_empty() {
            visit(WalkEntry {
                archive_name: prefix.to_owned(),
                fs_path: dir.to_owned(),
                is_dir: true,
            })?;
            continue;
        }

        let rel_str = relative
            .to_str()
            .ok_or_else(|| Error::InvalidUtf8Path(relative.display().to_string()))?;
        let archive_name = format!("{prefix}/{rel_str}");

        if excludes.is_match(&archive_name) {
            continue;
        }

        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        let utf8_str = fs_path
            .to_str()
            .ok_or_else(|| Error::InvalidUtf8Path(fs_path.display().to_string()))?;

        visit(WalkEntry {
            archive_name,
            fs_path: Utf8PathBuf::from(utf8_str),
            is_dir,
        })?;
    }
    Ok(())
}

// ── Path safety ────────────────────────────────────────────────────────────

/// Validate that an archive entry path does not escape the output directory.
///
/// Rejects paths containing `..` components to prevent "zip-slip" style
/// path-traversal attacks (CVE-2018-1002200).  The check is purely lexical —
/// it does not touch the filesystem — so it works before any directories are
/// created.
pub fn safe_entry_path(name: &str) -> Result<()> {
    // Check for ".." components that could escape the output directory.
    for component in Utf8Path::new(name).components() {
        if matches!(component, camino::Utf8Component::ParentDir) {
            return Err(Error::PathTraversal(name.to_owned()));
        }
    }
    // Also reject absolute paths.
    if Utf8Path::new(name).is_absolute() {
        return Err(Error::PathTraversal(name.to_owned()));
    }
    Ok(())
}

// ── Symlink helper ──────────────────────────────────────────────────────────

/// Read file metadata, optionally following symlinks.
pub fn input_metadata(path: &Utf8Path, follow_symlinks: bool) -> Result<std::fs::Metadata> {
    if follow_symlinks {
        Ok(fs_err::metadata(path)?)
    } else {
        Ok(fs_err::symlink_metadata(path)?)
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

        // Reject entries that attempt path traversal.
        safe_entry_path(orig_path.as_str())?;

        if !should_extract(orig_path.as_str(), &opts.includes, &opts.excludes) {
            continue;
        }

        // Skip directory entries — only files have content.
        if entry.header().entry_type().is_dir() {
            continue;
        }

        let stripped = match strip_components(&orig_path, opts.strip_components) {
            Some(p) => p,
            None => continue,
        };

        let display_name = if opts.no_directory {
            match stripped.file_name() {
                Some(name) => name.to_owned(),
                None => continue,
            }
        } else {
            stripped.to_string()
        };

        opts.progress.set_entry(&display_name);
        let written = std::io::copy(&mut entry, writer)?;
        opts.progress.inc(written);
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
///
/// Delegates to [`walk_dir`] for the actual directory traversal.
pub fn append_dir_filtered<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    dir: &Utf8Path,
    prefix: &str,
    opts: &CompressOpts<'_>,
) -> Result<()> {
    if opts.no_recursion {
        builder.append_dir(prefix, dir)?;
        return Ok(());
    }

    walk_dir(dir, prefix, opts, &mut |entry| {
        if entry.is_dir {
            builder.append_dir(&entry.archive_name, &entry.fs_path)?;
        } else {
            let meta = input_metadata(&entry.fs_path, opts.follow_symlinks)?;
            builder.append_path_with_name(&entry.fs_path, &entry.archive_name)?;
            opts.progress.set_entry(&entry.archive_name);
            opts.progress.inc(meta.len());
        }
        Ok(())
    })
}

/// Append each input (file or directory) to a tar builder, respecting
/// excludes.  This is the shared input-iteration loop used by every
/// tar-based compress function.
pub fn append_inputs<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    inputs: &[Utf8PathBuf],
    opts: &CompressOpts<'_>,
) -> Result<()> {
    for input in inputs {
        let meta = input_metadata(input, opts.follow_symlinks)?;
        let name = input.file_name().unwrap_or(input.as_str());
        if opts.excludes.is_match(name) {
            continue;
        }
        if meta.is_dir() {
            append_dir_filtered(builder, input, name, opts)?;
        } else {
            let size = meta.len();
            builder.append_path_with_name(input, name)?;
            opts.progress.set_entry(name);
            opts.progress.inc(size);
        }
    }
    Ok(())
}

/// Collect entry metadata from a tar archive into a `Vec<Entry>`.
/// Shared by every tar-based `list` function.
pub fn list_tar_entries<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
) -> Result<Vec<crate::Entry>> {
    let mut entries = Vec::new();
    for entry in archive.entries()? {
        let entry = entry?;
        let header = entry.header();
        let path = entry.path()?;
        let path = Utf8PathBuf::try_from(path.into_owned())
            .map_err(|e| Error::InvalidUtf8Path(e.into_path_buf().display().to_string()))?;
        entries.push(crate::Entry {
            path,
            size: header.size()?,
            mtime: header.mtime()?,
            mode: header.mode()?,
            is_dir: header.entry_type().is_dir(),
        });
    }
    Ok(entries)
}

/// Count entries and sum uncompressed sizes in a tar archive.
/// Shared by every tar-based `info` function.
pub fn count_tar_entries<R: std::io::Read>(archive: &mut tar::Archive<R>) -> Result<(usize, u64)> {
    let mut entry_count: usize = 0;
    let mut total_uncompressed: u64 = 0;
    for entry in archive.entries()? {
        let entry = entry?;
        total_uncompressed += entry.header().size()?;
        entry_count += 1;
    }
    Ok((entry_count, total_uncompressed))
}

/// Extract entries from a tar archive, honouring exclude patterns and
/// path-component stripping.  Reports progress per entry.
pub fn unpack_tar_filtered<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
    output: &Utf8Path,
    opts: &DecompressOpts<'_>,
) -> Result<()> {
    archive.set_preserve_permissions(opts.preserve_permissions);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let orig_path = entry.path()?;
        let orig_path = Utf8PathBuf::try_from(orig_path.into_owned())
            .map_err(|e| Error::InvalidUtf8Path(e.into_path_buf().display().to_string()))?;

        // Reject entries that attempt path traversal.
        safe_entry_path(orig_path.as_str())?;

        // Include/exclude check against the original (pre-strip) path.
        if !should_extract(orig_path.as_str(), &opts.includes, &opts.excludes) {
            continue;
        }

        let is_dir = entry.header().entry_type().is_dir();

        // --no-directory: skip directory entries, flatten file paths.
        if opts.no_directory && is_dir {
            continue;
        }

        let stripped = match strip_components(&orig_path, opts.strip_components) {
            Some(p) => p,
            None => continue,
        };

        let dest_path = if opts.no_directory {
            match stripped.file_name() {
                Some(name) => Utf8PathBuf::from(name),
                None => continue,
            }
        } else {
            stripped
        };

        let dest = output.join(&dest_path);

        // Ensure parent directories exist.
        if let Some(parent) = dest.parent()
            && !parent.as_str().is_empty()
        {
            fs_err::create_dir_all(parent)?;
        }

        // Overwrite guard for non-directory entries.
        if !is_dir && fs_err::symlink_metadata(&dest).is_ok() {
            if let Some(ref suffix) = opts.backup_suffix {
                let backup = Utf8PathBuf::from(format!("{dest}{suffix}"));
                fs_err::rename(&dest, &backup)?;
            } else if opts.keep_newer {
                let entry_mtime = entry.header().mtime().unwrap_or(0);
                if is_existing_newer(&dest, entry_mtime)? {
                    continue;
                }
            } else if opts.no_overwrite {
                continue;
            } else if !opts.force {
                return Err(Error::FileExists(dest));
            }
        }

        opts.progress.set_entry(dest_path.as_str());
        let size = entry.header().size().unwrap_or(0);
        entry.unpack(&dest)?;
        opts.progress.inc(size);
    }
    Ok(())
}

/// Returns `true` when the file at `path` has an mtime >= the given unix
/// timestamp, meaning the existing file is at least as new as the entry.
pub fn is_existing_newer(path: &Utf8Path, entry_mtime: u64) -> Result<bool> {
    let meta = fs_err::metadata(path)?;
    let file_mtime = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(file_mtime >= entry_mtime)
}

// ── Exclude-set building ────────────────────────────────────────────────────

/// Merge inline exclude patterns with patterns read from `--exclude-from`
/// files, then build a [`GlobSet`].
///
/// Used by Compress, Decompress, and List commands.
pub fn build_excludes(patterns: Vec<String>, pattern_files: &[Utf8PathBuf]) -> Result<GlobSet> {
    let mut all = patterns;
    for path in pattern_files {
        all.extend(read_patterns_from_file(path)?);
    }
    build_exclude_set(&all)
}

// ── Pattern / path file readers ─────────────────────────────────────────────

/// Read non-empty, non-comment lines from a file (one per line).
/// Blank lines and lines starting with `#` are ignored.
fn read_lines_from_file(path: &Utf8Path) -> Result<Vec<String>> {
    let file = fs_err::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut lines = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        lines.push(trimmed.to_owned());
    }
    Ok(lines)
}

/// Read glob patterns from a file, one per line.
/// Blank lines and lines starting with `#` are ignored.
pub fn read_patterns_from_file(path: &Utf8Path) -> Result<Vec<String>> {
    read_lines_from_file(path)
}

/// Read file paths from a file, one per line.
/// Blank lines and lines starting with `#` are ignored.
pub fn read_paths_from_file(path: &Utf8Path) -> Result<Vec<Utf8PathBuf>> {
    Ok(read_lines_from_file(path)?
        .into_iter()
        .map(Utf8PathBuf::from)
        .collect())
}

// ── Dry-run helpers ─────────────────────────────────────────────────────────

/// Collect all file paths that would be added to an archive from the given
/// inputs, honouring exclude patterns.  Used by `--dry-run` on compress.
///
/// Delegates to [`walk_dir`] for directory traversal.
pub fn collect_compress_paths(
    inputs: &[Utf8PathBuf],
    opts: &CompressOpts<'_>,
) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for input in inputs {
        let meta = input_metadata(input, opts.follow_symlinks)?;
        let name = input.file_name().unwrap_or(input.as_str());
        if opts.excludes.is_match(name) {
            continue;
        }
        if meta.is_dir() {
            if opts.no_recursion {
                paths.push(format!("{name}/"));
            } else {
                walk_dir(input, name, opts, &mut |entry| {
                    if entry.is_dir {
                        paths.push(format!("{}/", entry.archive_name));
                    } else {
                        paths.push(entry.archive_name);
                    }
                    Ok(())
                })?;
            }
        } else {
            paths.push(name.to_owned());
        }
    }
    Ok(paths)
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
        assert_eq!(s.map(|s| s.is_match("src/node_modules/foo")), Some(true),);
        assert_eq!(s.map(|s| s.is_match("src/other")), Some(false));
    }
}
