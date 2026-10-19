//! Error type for the innertube client.

/// Errors produced by the `YouTube` Music client.
#[derive(Debug, thiserror::Error)]
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
    /// The anonymous session could not be established (visitor data etc.).
    #[error("could not bootstrap client: {0}")]
    Bootstrap(String),
}

/// Convenience alias for the crate's result type.
pub type Result<T> = std::result::Result<T, Error>;
