//! Clean-room YouTube Music innertube client for Rift.
//!
//! Implements the anonymous (visitor-data) innertube flow for the `WEB_REMIX`
//! client and the metadata endpoints Rift needs, returning [`rift_types`]
//! models. Audio stream resolution is deliberately out of scope — Rift shells
//! out to yt-dlp for that.

mod client;
mod context;
mod error;
mod identity;
mod params;
mod parse;

pub use client::Client;
pub use error::{Error, Result};
pub use params::SearchFilter;

/// Raw response parsers, exposed for fixture tests and offline tooling.
#[doc(hidden)]
pub mod raw {
    pub use crate::parse::{
        album_page, albums, artist_page, artists, playlist_page, track_credits, tracks,
    };
}
