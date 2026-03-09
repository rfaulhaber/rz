use camino::{Utf8Path, Utf8PathBuf};

use crate::cmd::Format;
use crate::error::{Error, Result};

impl Format {
    /// Infer format from a file path's extension(s).
    ///
    /// Uses string suffix matching because `Path::extension()` only returns
    /// the last segment (e.g. "gz" for "foo.tar.gz"), which can't distinguish
    /// compound extensions like `.tar.gz` vs `.tar.zst`.
    pub fn from_path(path: &Utf8Path) -> Option<Self> {
        let s = path.as_str().to_ascii_lowercase();

        // Check longer suffixes first to avoid false matches
        if s.ends_with(".tar.gz") || s.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if s.ends_with(".tar.zst") || s.ends_with(".tzst") {
            Some(Self::TarZst)
        } else if s.ends_with(".tar.xz") || s.ends_with(".txz") {
            Some(Self::TarXz)
        } else if s.ends_with(".tar.bz2") || s.ends_with(".tbz2") {
            Some(Self::TarBz2)
        } else if s.ends_with(".tar") {
            Some(Self::Tar)
        } else if s.ends_with(".zip") {
            Some(Self::Zip)
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
        }
    }

    /// Derive a default output path from the first input and the format's extension.
    pub fn default_output(&self, first_input: &Utf8Path) -> Utf8PathBuf {
        let stem = first_input
            .file_name()
            .unwrap_or(first_input.as_str());
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
pub fn resolve_input_format(
    explicit: Option<Format>,
    input: &Utf8Path,
) -> Result<Format> {
    if let Some(f) = explicit {
        return Ok(f);
    }
    if let Some(f) = Format::from_magic(input) {
        return Ok(f);
    }
    Format::from_path(input)
        .ok_or_else(|| Error::CannotInferFormat(input.to_owned()))
}
