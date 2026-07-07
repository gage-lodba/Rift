//! Queue management and track playback orchestration.
//!
//! Tracks are resolved to stream URLs with rustypipe, downloaded fully into
//! memory (a typical m4a is 3–5 MB) and handed to the audio thread. A
//! generation counter guards against races when the user skips while a
//! download is still in flight.
//!
//! The state types and broadcast helpers live in [`state`]; fetch/play/crossfade
//! in [`playback`]; next/previous/toggle/stop in [`nav`]; the pure queue
//! mutations in [`queue`]; and the audio-thread bridge in [`events`].

mod events;
mod nav;
mod playback;
mod queue;
mod state;

#[cfg(test)]
mod tests;

pub use events::event_loop;
pub use nav::{play_next, play_prev, stop, toggle_playback};
pub use playback::play_index;
pub use queue::{
    append_tracks, insert_next, move_in_queue, remove_from_queue, AddOutcome, RemoveOutcome,
};
pub use state::{emit_queue, load_snapshot, snapshot, PlayerCore, PlayerShared};
