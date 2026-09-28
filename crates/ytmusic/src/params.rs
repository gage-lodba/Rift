//! YouTube Music search filters.
//!
//! Each variant is a precomputed base64 protobuf blob sent as the `params`
//! field of the `/search` request. These are protocol constants, not code.

/// The kind of item a YouTube Music search returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFilter {
    /// Audio tracks.
    Tracks,
    /// Artists.
    Artists,
    /// Albums.
    Albums,
}

impl SearchFilter {
    /// The protobuf `params` string for this filter.
    ///
    /// These are the current `searchEndpoint.params` values YouTube itself
    /// emits in the search chip cloud (observed from a live response). They
    /// change over time, so they are kept in one place.
    pub fn params(self) -> &'static str {
        match self {
            SearchFilter::Tracks => "EgWKAQIIAWoSEAUQCRADEAQQChAQEA4QFRAR",
            SearchFilter::Artists => "EgWKAQIgAWoSEAUQCRADEAQQChAQEA4QFRAR",
            SearchFilter::Albums => "EgWKAQIYAWoSEAUQCRADEAQQChAQEA4QFRAR",
        }
    }
}
