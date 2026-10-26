//! Error type for the innertube client.

/// Errors produced by the `YouTube` Music client.
///
/// Non-exhaustive: the transport can grow failure modes (new rejection statuses,
/// new size guards) without that being a breaking change for callers, all of
/// which currently just format the error for a user-facing message.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The HTTP request itself failed (DNS, TLS, timeout, connection reset).
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    /// Innertube answered with a non-success result code.
    #[error("innertube returned status {status}: {message}")]
    Api {
        /// HTTP status or innertube result code that rejected the request.
        status: u16,
        /// Server-provided explanation, when the response carried one.
        message: String,
    },
    /// The response body could not be parsed into the expected shape.
    #[error("could not parse innertube response: {0}")]
    Parse(String),
    /// The response body was larger than the client's safety ceiling, so it was
    /// not buffered (or was abandoned part-way through). See
    /// `client::MAX_RESPONSE_BYTES` for the ceiling and why it exists.
    #[error("innertube response too large: {size} bytes")]
    ResponseTooLarge {
        /// Body size the server declared, or the byte count reached in the body
        /// that was being read when the ceiling was hit.
        size: u64,
    },
    /// The anonymous session could not be established (visitor data etc.).
    #[error("could not bootstrap client: {0}")]
    Bootstrap(String),
    /// The player endpoint answered that the video is not playable.
    ///
    /// This is the server's verdict on the *video*, not on the request: it is
    /// what a removed, private, age-gated or region-blocked video looks like, and
    /// it is distinct from [`Error::Api`] so the caller can surface a useful
    /// reason to the user while treating it as a fetch failure.
    #[error("video unavailable: {reason}")]
    Unavailable {
        /// Server-provided explanation when the response carried one, otherwise
        /// the playability status itself (`LOGIN_REQUIRED`, `ERROR`, ...).
        reason: String,
    },
    /// The player response carried no directly downloadable audio format.
    ///
    /// The response either had no `streamingData`, no audio-only entries, or
    /// only entries that would need JavaScript signature/`n` solving — which
    /// this clean-room resolver deliberately does not implement. The fetch
    /// surfaces this as an actionable error rather than retrying another backend.
    #[error("player response offered no downloadable audio format")]
    NoAudioFormat,
}

/// Convenience alias for the crate's result type.
pub type Result<T> = std::result::Result<T, Error>;
