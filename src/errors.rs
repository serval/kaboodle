//! Error types used internally to communicate details about errors specific to our implementation
//! details.
#![allow(missing_docs)]

use std::net::SocketAddr;

use thiserror::Error;

#[derive(Clone, Debug, Error)]
pub enum KaboodleError {
    #[error("IoError: {0}")]
    IoError(String),

    #[error("SocketAddrChannelSendError: {0}")]
    SocketAddrChannelSendError(SocketAddr),

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
}

// We can't just use #[from] to derive these because the errors in question don't implement Clone,
// but we do want KaboodleError to implement Clone. Instead, we manually marshall it into a clonable
// custom error variant.
impl From<std::io::Error> for KaboodleError {
    fn from(value: std::io::Error) -> Self {
        KaboodleError::IoError(value.to_string())
    }
}

impl From<tokio::sync::mpsc::error::SendError<SocketAddr>> for KaboodleError {
    fn from(addr: tokio::sync::mpsc::error::SendError<SocketAddr>) -> Self {
        KaboodleError::SocketAddrChannelSendError(addr.0)
    }
}
