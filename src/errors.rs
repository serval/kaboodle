//! Error types used internally to communicate details about errors specific to our implementation
//! details.
#![allow(missing_docs)]

use std::net::SocketAddr;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum KaboodleError {
    /// A conversion for std:io:Error
    #[error("std::io::Error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("tokio::sync::mpsc::error::SendError: {0}")]
    SendError(#[from] tokio::sync::mpsc::error::SendError<SocketAddr>),

    #[error("No available interfaces meet our requirements")]
    NoAvailableInterfaces,

    #[error("Unable to find interface number for the given networking interface")]
    UnableToFindInterfaceNumber,

    #[error("Unable to stop: {0}")]
    StoppingFailed(String),

    #[error("Can't request payloads when stopped")]
    PayloadsUnavailableNotRunning,

    #[error("Failed to request payload from peer")]
    FailedToRequestPayload,

    #[error("Payload for a peer is not available because the peer has failed")]
    FailedPeerPayloadUnavailable,

    #[error("Communication channel closed; result unavailable")]
    ChannelClosed,

    #[error("The original wrapped error was lost when this error was cloned")]
    UncloneableError(String),
}

// We can't derive Clone for KaboodleError because some of the errors we marshall #[from] don't
// implement Clone themselves, hence this little bit of gross logic:
impl Clone for KaboodleError {
    fn clone(&self) -> Self {
        match self {
            Self::IoError(err) => KaboodleError::UncloneableError(err.to_string()),
            Self::SendError(err) => tokio::sync::mpsc::error::SendError::<SocketAddr>(err.0).into(),
            Self::NoAvailableInterfaces => Self::NoAvailableInterfaces,
            Self::UnableToFindInterfaceNumber => Self::UnableToFindInterfaceNumber,
            Self::StoppingFailed(reason) => Self::StoppingFailed(reason.to_owned()),
            Self::PayloadsUnavailableNotRunning => Self::PayloadsUnavailableNotRunning,
            Self::FailedToRequestPayload => Self::FailedToRequestPayload,
            Self::FailedPeerPayloadUnavailable => Self::FailedPeerPayloadUnavailable,
            Self::ChannelClosed => Self::ChannelClosed,
            Self::UncloneableError(msg) => Self::UncloneableError(msg.to_owned()),
        }
    }
}
