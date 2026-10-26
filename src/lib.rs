pub mod archive;
pub mod qt;

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid CopyQ data: {0}")]
    InvalidData(String),
    #[error("unsupported Qt QVariant type id: {0}")]
    UnsupportedVariant(u32),
    #[error("CopyQ archive version {0} is not supported yet")]
    UnsupportedVersion(u8),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn extract(_input: &Path, _output: &Path, _password: Option<&str>) -> Result<(), Error> {
    Err(Error::InvalidData(
        "archive decoder is not connected yet".into(),
    ))
}
