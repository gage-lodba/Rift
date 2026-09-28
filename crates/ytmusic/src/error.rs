//! Error type for the innertube client.

/// Errors produced by the YouTube Music client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("innertube returned status {status}: {message}")]
    Api { status: u16, message: String },
    #[error("could not parse innertube response: {0}")]
    Parse(String),
    #[error("could not bootstrap client: {0}")]
    Bootstrap(String),
}

/// Convenience alias for the crate's result type.
pub type Result<T> = std::result::Result<T, Error>;
