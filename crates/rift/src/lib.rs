//! Rift's native backend library.
//!
//! This crate is the old backend crate (package `rift`, formerly the previous
//! desktop shell's backend) with the shell framework removed. It owns everything
//! the desktop application needs but does not render:
//!
//! * the audio thread and playback queue ([`RiftBackend`] drives them),
//! * the persisted library (likes, playlists, recently played),
//! * offline downloads,
//! * settings,
//! * the `YouTube` Music innertube client ([`fetch`] + `rift-ytmusic`),
//! * native audio streaming: direct-URL resolution and ranged HTTP downloads
//!   ([`fetch`] and its `stream` module) — no external downloader binary,
//! * OS media keys and now-playing metadata (MPRIS / SMTC / `MPNowPlayingInfo`),
//! * Discord Rich Presence.
//!
//! The application (`crates/rift-desktop/`, package `rift-desktop`) constructs a
//! [`RiftBackend`] once, drains [`BackendEvents`] from the UI thread or a pump
//! thread, and calls the legacy IPC commands as methods on
//! [`BackendHandle`].
//!
//! # Contract
//!
//! The authoritative reference — constructor, every method signature, error and
//! blocking notes, the event payloads, the data-directory layout and the
//! command-mapping table — is `docs/backend-api.md` in the
//! repository. Read it before wiring the app crate.
//!
//! # Example
//!
//! ```text
//! use rift::{BackendConfig, BackendEvent, RiftBackend};
//!
//! let (backend, mut events) = RiftBackend::new(BackendConfig::default())?;
//!
//! // Startup snapshot (library, queue, playback state, settings, downloads).
//! let bootstrap = backend.bootstrap();
//!
//! // Per frame: drain the event bus.
//! while let Some(event) = events.try_recv() {
//!     match event {
//!         BackendEvent::Track(track) => { /* now playing */ }
//!         BackendEvent::Progress(p) => { /* seek bar */ }
//!         _ => {}
//!     }
//! }
//!
//! // Commands: sync ones directly, async ones inside a spawned task.
//! backend.set_volume(0.5);
//! let handle = backend.handle();
//! let task_handle = handle.clone();
//! task_handle.spawn(async move {
//!     let _tracks = handle.search("daft punk".into()).await;
//! });
//!
//! backend.shutdown();
//! ```
//!
//! No desktop-shell framework is involved: the crate has no windowing,
//! no webview and no IPC layer. The only platform coupling is inside the
//! dedicated media/Discord/audio threads, which already degrade gracefully when
//! the OS integration is unavailable.

// The backend is a safe-Rust library: platform/FFI details live in the
// dependencies (rodio, souvlaki) or the in-repo `rift-discord` crate, not here.
#![forbid(unsafe_code)]
// This crate is consumed by the desktop app and documented as the contract in
// `docs/backend-api.md`; a new public item must be documented.
#![warn(missing_docs)]

mod audio;
mod backend;
mod commands;
mod discord;
mod downloads;
mod event;
pub mod fetch;
mod library;
mod media;
mod paths;
mod player;
mod settings;
mod stream;
mod util;

pub use backend::{BackendConfig, BackendError, BackendHandle, RiftBackend};
pub use commands::images::MAX_IMAGE_BYTES;
pub use event::{BackendEvent, BackendEvents};
pub use paths::{native_data_dir, DataDirError, APP_IDENTIFIER, DATA_DIR_ENV};
pub use settings::MAX_CROSSFADE;

/// The crate's own version, as shown by "check for updates".
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether the process is a debug build launched with `RIFT_PREVIEW=1`.
///
/// In preview mode the backend never persists settings and skips commands that
/// would hit the network. The desktop app additionally renders placeholder data
/// instead of calling the backend, mirroring the legacy web app's preview mode.
#[must_use]
pub fn preview_mode() -> bool {
    settings::preview_mode()
}
