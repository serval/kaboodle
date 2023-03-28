//! Error types used internally to communicate details about errors specific to our implementation
//! details.
#![allow(missing_docs)]

use thiserror::Error;

#[derive(Error, Debug)]
pub enum KaboodleError {
    /// A conversion for std:io:Error
    #[error("std::io::Error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("No available interfaces meet our requirements")]
    NoAvailableInterfaces,

    #[error("Unable to find interface number for the given networking interface")]
    UnableToFindInterfaceNumber,

    #[error("Unable to stop: {0}")]
    StoppingFailed(String),

    #[error("Functionality unavailable because the mesh is not currently running")]
    NotRunning,
}
