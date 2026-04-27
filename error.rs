use std::io;

use camino::Utf8PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),

    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("cannot infer format from path: {0}")]
    CannotInferFormat(Utf8PathBuf),

    #[error("cannot infer output path; specify --output")]
    CannotInferOutput,

    #[error("path is not valid UTF-8: {0}")]
    InvalidUtf8Path(String),

    #[error("7z: {0}")]
    SevenZ(#[from] sevenz_rust2::Error),

    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("invalid exclude pattern: {0}")]
    InvalidExcludePattern(String),

    #[error("file already exists: {0} (use --force to overwrite)")]
    FileExists(Utf8PathBuf),

    #[error("--strip-components is not supported for {0} format")]
    StripComponentsUnsupported(String),

    #[error(
        "--keep-newer is not supported for {0} format (entries lack reliable \
         mtime metadata); use --no-overwrite if you want to skip existing files"
    )]
    KeepNewerUnsupported(String),

    #[error(
        "{flag} is not supported for {format} format \
         (supported on tar-family formats: tar, tar-gz, tar-zst, tar-xz, tar-bz2)"
    )]
    ReproducibilityFlagUnsupported { flag: &'static str, format: String },

    #[error("{0} format does not support reading from stdin (requires seekable input)")]
    StdinNotSupported(String),

    #[error("{0} format does not support writing to stdout (requires seekable output)")]
    StdoutNotSupported(String),

    #[error("cannot infer format from stdin; specify --format")]
    CannotInferFormatStdin,

    #[error("path traversal detected in archive entry: {0}")]
    PathTraversal(String),

    #[error(
        "zstd: the pure-Rust encoder only accepts --level 0 (store, no \
         compression); omit --level to use the default fast-compression mode"
    )]
    ZstdLevelUnsupported,

    #[error(
        "{operation} is not supported for {format} format \
         (supported on tar-family and zip)"
    )]
    ModifyUnsupported {
        operation: &'static str,
        format: String,
    },
}
