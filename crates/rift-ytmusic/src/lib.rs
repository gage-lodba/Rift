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

/// HTTP/2 response header-list ceiling for the innertube clients.
///
/// hyper's h2 client defaults to a 16 KiB receive limit, and `YouTube` exceeds
/// it on a large fraction of `music.youtube.com` home-page responses: the site
/// intermittently adds a ~6.2 KiB `content-security-policy-report-only` header,
/// pushing the decoded header list from ~12 KiB to ~18 KiB. h2 then resets the
/// stream (`REQUEST_HEADER_FIELDS_TOO_LARGE`), which surfaces as an opaque
/// `http2 error: stream error detected` and fails the visitor scrape every cold
/// playback/search starts with (~50% of cold starts measured on 2026-10-30).
/// 64 KiB matches the value Google's own edge advertises in its SETTINGS; the
/// largest response observed was ~19.9 KiB. The
/// bound is per response and transient, and these clients only talk to
/// YouTube/Google hosts, so the headroom costs nothing else.
pub(crate) const MAX_H2_RESPONSE_HEADER_LIST_BYTES: u32 = 64 * 1024;

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
