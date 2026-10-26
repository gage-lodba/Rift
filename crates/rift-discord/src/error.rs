//! Errors from the Discord IPC transport.

use std::fmt;

/// Why a Discord IPC operation failed.
#[derive(Debug)]
pub enum Error {
    /// No Discord IPC socket could be opened.
    ///
    /// The string is the last OS error from the candidate paths (or a note that
    /// no candidate path existed); Discord is usually not running when this
    /// happens.
    IpcUnavailable(String),
    /// A command was sent before [`Client::connect`](crate::Client::connect)
    /// completed.
    NotConnected,
    /// Reading from or writing to the local socket failed.
    ///
    /// A failure here means the socket should be considered dead; the caller
    /// drops the client and reconnects on the next attempt.
    Io(std::io::Error),
    /// A JSON body could not be encoded or decoded.
    Json(serde_json::Error),
    /// Discord's reply did not have the shape the protocol documents.
    Protocol(String),
    /// The handshake was rejected or not acknowledged with a `READY` dispatch.
    Handshake(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IpcUnavailable(detail) => {
                write!(f, "no Discord IPC socket is available: {detail}")
            }
            Self::NotConnected => write!(f, "the Discord IPC client is not connected"),
            Self::Io(error) => write!(f, "Discord IPC I/O failed: {error}"),
            Self::Json(error) => write!(f, "Discord IPC JSON failed: {error}"),
            Self::Protocol(detail) => write!(f, "unexpected Discord IPC protocol: {detail}"),
            Self::Handshake(detail) => write!(f, "Discord RPC handshake failed: {detail}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::IpcUnavailable(_)
            | Self::NotConnected
            | Self::Protocol(_)
            | Self::Handshake(_) => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
