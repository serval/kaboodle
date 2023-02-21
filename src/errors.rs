use thiserror::Error;

#[derive(Error, Debug)]
pub enum KaboodleError {
    /// A conversion for std:io:Error
    #[error("std::io::Error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Unable to stop: {0}")]
    StoppingFailed(String),
}
