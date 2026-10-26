pub mod archive;
pub mod crypto;
pub mod extract;
pub mod qt;

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid CopyQ data: {0}")]
    InvalidData(String),
    #[error("unsupported Qt QVariant type id: {0}")]
    UnsupportedVariant(u32),
    #[error("output path already exists: {0}")]
    OutputExists(PathBuf),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn extract(
    input: &Path,
    output: &Path,
    password: Option<&str>,
) -> Result<extract::ExtractionSummary, Error> {
    extract::extract(input, output, password)
}
