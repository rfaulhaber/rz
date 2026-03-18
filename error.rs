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
}
