use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("CPQ decoder is not implemented yet")]
    NotImplemented,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn extract(_input: &Path, _output: &Path, _password: Option<&str>) -> Result<(), Error> {
    Err(Error::NotImplemented)
}
