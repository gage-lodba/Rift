//! Clean-room `YouTube` Music innertube client for Rift.
//!
//! Implements the anonymous (visitor-data) innertube flow for the `WEB_REMIX`
//! client and the metadata endpoints Rift needs, returning [`rift_types`]
//! models. The [`player`] module also resolves native audio streams through the
//! JS-less `VISIONOS` player client, so playback does not depend on an external
//! media process.

// The innertube client is pure Rust + HTTP; no `unsafe` is needed here.
#![forbid(unsafe_code)]
// The public surface is what `rift` (and any fixture/offline tooling) builds
// against, so new public items have to carry documentation.
#![warn(missing_docs)]

mod client;
mod context;
mod error;
mod identity;
mod params;
mod parse;

pub mod player;

pub use client::Client;
pub use error::{Error, Result};
pub use params::SearchFilter;
pub use player::AudioStream;

/// Raw response parsers, exposed for fixture tests and offline tooling.
#[doc(hidden)]
pub mod raw {
    pub use crate::parse::{
        album_page, albums, artist_page, artists, playlist_page, track_credits, tracks,
        PlaylistPage,
    };
    // The pure helpers behind the row parsers, so their rules can be pinned
    // without building a whole innertube response around each one.
    pub use crate::parse::{
        album_type_from_shelf, is_subtitle_noise, is_type_badge, parse_count, parse_duration,
        tracks_playlist_id, unlinked_artist,
    };
}
