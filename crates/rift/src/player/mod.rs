//! Queue management and track playback orchestration.
//!
//! Tracks are resolved to audio natively and downloaded fully into memory (a
//! typical m4a is 3–5 MB) before being handed to the audio thread. A generation
//! counter guards against races when the user skips while a download is still in
//! flight.
//!
//! Everything here is crate-internal: the public surface is the command methods
//! on [`crate::BackendHandle`]. The state types and broadcast helpers live in
//! [`state`]; fetch/play/crossfade in [`playback`]; next/previous/toggle/stop in
//! [`nav`]; the pure queue mutations in [`queue`]; and the audio-thread bridge in
//! [`events`].

mod events;
mod nav;
mod playback;
mod queue;
mod state;

#[cfg(test)]
mod tests;

pub(crate) use events::event_loop;
pub(crate) use nav::{cycle_repeat, play_next, play_prev, stop, toggle_playback, toggle_shuffle};
pub(crate) use playback::play_index;
pub(crate) use queue::{
    append_tracks, insert_next, move_in_queue, remove_from_queue, AddOutcome, RemoveOutcome,
};
pub(crate) use state::{emit_queue, load_snapshot, snapshot, PlayerCore, PlayerShared};
