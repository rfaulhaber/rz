use std::fmt;

use camino::{Utf8Path, Utf8PathBuf};

use crate::cmd::Format;
use crate::error::{Error, Result};

impl fmt::Display for Format {
    /// User-facing name matching the kebab-case `--format` clap value, so
    /// error messages are actionable — if the error says `tar-gz`, the user
    /// can pass `--format tar-gz` verbatim.  (Use `extension()` when you want
    /// the dotted filename suffix instead.)
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar-gz",
            Self::TarZst => "tar-zst",
            Self::TarXz => "tar-xz",
            Self::TarBz2 => "tar-bz2",
            Self::SevenZ => "seven-z",
        };
        f.write_str(s)
    }
}

/// Case-insensitive suffix check on ASCII bytes.  Equivalent to
/// `s.to_ascii_lowercase().ends_with(suffix)` but without the allocation.
fn ends_with_ci(s: &str, suffix: &str) -> bool {
    let sb = s.as_bytes();
    let tb = suffix.as_bytes();
    sb.len() >= tb.len() && sb[sb.len() - tb.len()..].eq_ignore_ascii_case(tb)
}

impl Format {
    /// Infer format from a file path's extension(s).
    ///
    /// Uses string suffix matching because `Path::extension()` only returns
    /// the last segment (e.g. "gz" for "foo.tar.gz"), which can't distinguish
    /// compound extensions like `.tar.gz` vs `.tar.zst`.
    pub fn from_path(path: &Utf8Path) -> Option<Self> {
        let s = path.as_str();

        // Check longer suffixes first to avoid false matches.
        // Uses byte-level case-insensitive comparison so we don't allocate a
        // lowercased copy of the whole path just to test extensions.
        if ends_with_ci(s, ".tar.gz") || ends_with_ci(s, ".tgz") {
            Some(Self::TarGz)
        } else if ends_with_ci(s, ".tar.zst") || ends_with_ci(s, ".tzst") {
            Some(Self::TarZst)
        } else if ends_with_ci(s, ".tar.xz") || ends_with_ci(s, ".txz") {
            Some(Self::TarXz)
        } else if ends_with_ci(s, ".tar.bz2") || ends_with_ci(s, ".tbz2") {
            Some(Self::TarBz2)
        } else if ends_with_ci(s, ".tar") {
            Some(Self::Tar)
        } else if ends_with_ci(s, ".zip") {
            Some(Self::Zip)
        } else if ends_with_ci(s, ".7z") {
            Some(Self::SevenZ)
        } else {
            None
        }
    }

    /// Infer format from the first bytes of a file (magic bytes).
    pub fn from_magic(path: &Utf8Path) -> Option<Self> {
        let kind = infer::get_from_path(path.as_std_path()).ok()??;
        match kind.mime_type() {
            "application/gzip" => Some(Self::TarGz),
            "application/zstd" => Some(Self::TarZst),
            "application/x-xz" => Some(Self::TarXz),
            "application/x-bzip2" => Some(Self::TarBz2),
            "application/zip" => Some(Self::Zip),
            "application/x-7z-compressed" => Some(Self::SevenZ),
            _ => None,
        }
    }

    /// Canonical file extension for this format (including leading dot).
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Zip => ".zip",
            Self::Tar => ".tar",
            Self::TarGz => ".tar.gz",
            Self::TarZst => ".tar.zst",
            Self::TarXz => ".tar.xz",
            Self::TarBz2 => ".tar.bz2",
            Self::SevenZ => ".7z",
        }
    }

    /// Derive a default output path from the first input and the format's extension.
    pub fn default_output(&self, first_input: &Utf8Path) -> Utf8PathBuf {
        let stem = first_input.file_name().unwrap_or(first_input.as_str());
        let mut out = Utf8PathBuf::from(stem);
        let ext = self.extension();
        out.set_file_name(format!("{stem}{ext}"));
        out
    }
}

/// Resolve format for `compress`: explicit flag → output extension.
pub fn resolve_compress_format(
    explicit: Option<Format>,
    output: Option<&Utf8Path>,
) -> Result<Format> {
    if let Some(f) = explicit {
        return Ok(f);
    }
    if let Some(out) = output {
        if let Some(f) = Format::from_path(out) {
            return Ok(f);
        }
        return Err(Error::CannotInferFormat(out.to_owned()));
    }
    Err(Error::CannotInferOutput)
}

/// Resolve format for `decompress`/`list`/`info`: explicit flag → magic bytes → extension.
pub fn resolve_input_format(explicit: Option<Format>, input: &Utf8Path) -> Result<Format> {
    if let Some(f) = explicit {
        return Ok(f);
    }
    if let Some(f) = Format::from_magic(input) {
        return Ok(f);
    }
    Format::from_path(input).ok_or_else(|| Error::CannotInferFormat(input.to_owned()))
}

#[cfg(test)]
mod tests {
    use camino::{Utf8Path, Utf8PathBuf};

    use crate::cmd::Format;
    use crate::format::{resolve_compress_format, resolve_input_format};

    // ── from_path ────────────────────────────────────────────────────────

    #[test]
    fn from_path_tar_gz() {
        assert_eq!(
            Format::from_path(Utf8Path::new("a.tar.gz")),
            Some(Format::TarGz)
        );
    }

    #[test]
    fn from_path_tgz() {
        assert_eq!(
            Format::from_path(Utf8Path::new("a.tgz")),
            Some(Format::TarGz)
        );
    }

    #[test]
    fn from_path_tar_zst() {
        assert_eq!(
            Format::from_path(Utf8Path::new("a.tar.zst")),
            Some(Format::TarZst)
        );
    }

    #[test]
    fn from_path_tzst() {
        assert_eq!(
            Format::from_path(Utf8Path::new("a.tzst")),
            Some(Format::TarZst)
        );
    }

    #[test]
    fn from_path_tar_xz() {
        assert_eq!(
            Format::from_path(Utf8Path::new("a.tar.xz")),
            Some(Format::TarXz)
        );
    }

    #[test]
    fn from_path_txz() {
        assert_eq!(
            Format::from_path(Utf8Path::new("a.txz")),
            Some(Format::TarXz)
        );
    }

    #[test]
    fn from_path_tar_bz2() {
        assert_eq!(
            Format::from_path(Utf8Path::new("a.tar.bz2")),
            Some(Format::TarBz2)
        );
    }

    #[test]
    fn from_path_tbz2() {
        assert_eq!(
            Format::from_path(Utf8Path::new("a.tbz2")),
            Some(Format::TarBz2)
        );
    }

    #[test]
    fn from_path_tar() {
        assert_eq!(Format::from_path(Utf8Path::new("a.tar")), Some(Format::Tar));
    }

    #[test]
    fn from_path_zip() {
        assert_eq!(Format::from_path(Utf8Path::new("a.zip")), Some(Format::Zip));
    }

    #[test]
    fn from_path_seven_z() {
        assert_eq!(
            Format::from_path(Utf8Path::new("a.7z")),
            Some(Format::SevenZ)
        );
    }

    #[test]
    fn from_path_unknown_returns_none() {
        assert_eq!(Format::from_path(Utf8Path::new("a.rar")), None);
    }

    #[test]
    fn from_path_no_extension_returns_none() {
        assert_eq!(Format::from_path(Utf8Path::new("noext")), None);
    }

    #[test]
    fn from_path_is_case_insensitive() {
        assert_eq!(
            Format::from_path(Utf8Path::new("A.TAR.GZ")),
            Some(Format::TarGz)
        );
        assert_eq!(
            Format::from_path(Utf8Path::new("B.Tar.Bz2")),
            Some(Format::TarBz2)
        );
        assert_eq!(Format::from_path(Utf8Path::new("C.ZIP")), Some(Format::Zip));
    }

    #[test]
    fn from_path_with_directory_prefix() {
        assert_eq!(
            Format::from_path(Utf8Path::new("/some/dir/archive.tar.gz")),
            Some(Format::TarGz),
        );
    }

    // ── extension ────────────────────────────────────────────────────────

    #[test]
    fn extension_round_trips_with_from_path() {
        let formats = [
            Format::Zip,
            Format::Tar,
            Format::TarGz,
            Format::TarZst,
            Format::TarXz,
            Format::TarBz2,
            Format::SevenZ,
        ];
        for fmt in &formats {
            let name = format!("test{}", fmt.extension());
            let detected = Format::from_path(Utf8Path::new(&name));
            assert_eq!(
                detected.as_ref(),
                Some(fmt),
                "round-trip failed for {}",
                fmt.extension()
            );
        }
    }

    // ── default_output ───────────────────────────────────────────────────

    #[test]
    fn default_output_appends_extension() {
        let out = Format::TarGz.default_output(Utf8Path::new("mydir"));
        assert_eq!(out, Utf8PathBuf::from("mydir.tar.gz"));
    }

    #[test]
    fn default_output_strips_parent_directory() {
        let out = Format::Zip.default_output(Utf8Path::new("/home/user/mydir"));
        assert_eq!(out, Utf8PathBuf::from("mydir.zip"));
    }

    // ── resolve_compress_format ──────────────────────────────────────────

    #[test]
    fn resolve_compress_explicit_flag_wins() {
        let result = resolve_compress_format(Some(Format::TarGz), Some(Utf8Path::new("out.zip")));
        assert_eq!(result.ok(), Some(Format::TarGz));
    }

    #[test]
    fn resolve_compress_infers_from_output_extension() {
        let result = resolve_compress_format(None, Some(Utf8Path::new("out.tar.zst")));
        assert_eq!(result.ok(), Some(Format::TarZst));
    }

    #[test]
    fn resolve_compress_unknown_output_extension_errors() {
        assert!(resolve_compress_format(None, Some(Utf8Path::new("out.rar"))).is_err());
    }

    #[test]
    fn resolve_compress_no_format_no_output_errors() {
        assert!(resolve_compress_format(None, None).is_err());
    }

    // ── resolve_input_format ─────────────────────────────────────────────

    #[test]
    fn resolve_input_explicit_flag_wins() {
        let result = resolve_input_format(Some(Format::SevenZ), Utf8Path::new("a.tar.gz"));
        assert_eq!(result.ok(), Some(Format::SevenZ));
    }

    #[test]
    fn resolve_input_falls_back_to_extension() {
        // File doesn't exist → magic-byte detection returns None → falls to extension
        let result = resolve_input_format(None, Utf8Path::new("nonexistent.tar.bz2"));
        assert_eq!(result.ok(), Some(Format::TarBz2));
    }

    #[test]
    fn resolve_input_unknown_extension_errors() {
        assert!(resolve_input_format(None, Utf8Path::new("nonexistent.rar")).is_err());
    }
}
