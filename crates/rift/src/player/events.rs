//! Bridge from the audio thread to the application: forward progress/duration,
//! arm crossfades, and advance the queue when a track ends.
//!
//! This loop runs as a task on the backend runtime. It used to take the shell's
//! `AppHandle`; it now takes the backend's shared state, and every former
//! `app.emit(...)` call is an event-bus send.

use std::sync::Arc;

use rift_types::{PlaybackState, Progress};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::debug;

use super::nav::{play_next, stop};
use super::playback::{begin_crossfade, should_arm_crossfade};
use crate::audio::AudioEvent;
use crate::backend::BackendInner;
use crate::event::BackendEvent;
use crate::util::LockExt;

/// Whether a position tick is meaningful for the current playback state.
///
/// A tick that raced a track change can be delivered after `start_playback` has
/// reset the position and put the player into `Loading` — the audio thread may
/// still have a tick armed from the outgoing track. It carries the old track's
/// end position; applying it would make the next `Playing` state anchor Discord's
/// time bar at the end of the new song (and jump the UI seek bar). Ticks are
/// therefore only accepted while the current generation is actually `Playing`;
/// seeks while paused set the position directly, so nothing legitimate is lost.
fn position_tick_applies(state: PlaybackState) -> bool {
    matches!(state, PlaybackState::Playing)
}

/// Forwards events from the audio thread to the application and advances the
/// queue when a track ends.
///
/// The audio thread is also the progress ticker: while a track plays it sends
/// [`AudioEvent::Position`] about five times a second, independent of anything
/// else happening, which is what keeps the seek bar moving during quiet periods.
pub(crate) async fn event_loop(backend: Arc<BackendInner>, mut rx: UnboundedReceiver<AudioEvent>) {
    while let Some(ev) = rx.recv().await {
        match ev {
            AudioEvent::Duration(duration) => {
                // The audio thread only reports decoder lengths, so a non-finite
                // value means something upstream is broken. Rejecting it keeps
                // NaN/∞ out of `core.duration`, where it would poison the seek
                // bar and the crossfade window from then on.
                if !duration.is_finite() {
                    debug!("ignoring non-finite duration from the audio thread: {duration}");
                    continue;
                }
                let (position, changed) = {
                    let mut core = backend.player.core.lock_safe();
                    // Only a real correction is worth a Discord push: if the
                    // fetch already supplied this length the bar is anchored, and
                    // a second identical push just burns Discord's rate budget.
                    let changed = (core.duration - duration).abs() > 0.5;
                    core.duration = duration;
                    (core.position, changed)
                };
                backend.emit(BackendEvent::Progress(Progress { position, duration }));
                // The decoder's length is authoritative and may correct a
                // missing/zero metadata duration; hand a genuine correction to
                // Discord so its time bar shows (and is accurate) for those tracks.
                if changed {
                    backend.discord.set_duration(duration, position);
                }
            }
            AudioEvent::Position(position) => {
                if !position.is_finite() {
                    debug!("ignoring non-finite position from the audio thread: {position}");
                    continue;
                }
                let (duration, crossfade) = {
                    let mut core = backend.player.core.lock_safe();
                    if !position_tick_applies(core.state) {
                        continue;
                    }
                    core.position = position;
                    // Arm a crossfade once, when the track is within the overlap
                    // window of its end. The rule (repeat-one excluded, keyed by
                    // generation so it fires at most once per track, never while a
                    // prefetch is in flight) is `should_arm_crossfade`, where it
                    // is unit-tested.
                    let arm = should_arm_crossfade(&mut core, position);
                    (core.duration, arm)
                };
                backend.emit(BackendEvent::Progress(Progress { position, duration }));
                if crossfade {
                    debug!("nearing track end, starting crossfade");
                    begin_crossfade(&backend);
                }
            }
            AudioEvent::Ended => {
                debug!("track ended, advancing queue");
                play_next(&backend, false);
            }
            AudioEvent::DecodeFailed(msg) => {
                // An unplayable track (fetched fine, won't decode): skip it like
                // a failed fetch, with the same budget so a queue of all-broken
                // tracks stops instead of spinning forever.
                backend.emit(BackendEvent::Error(msg));
                let skip = {
                    let mut core = backend.player.core.lock_safe();
                    core.failures += 1;
                    core.failures < core.queue.len().max(1) as u32
                };
                if skip {
                    debug!("auto-skipping undecodable track");
                    play_next(&backend, false);
                } else {
                    stop(&backend);
                }
            }
            AudioEvent::Failed(msg) => {
                backend.emit(BackendEvent::Error(msg));
                stop(&backend);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::position_tick_applies;
    use rift_types::PlaybackState;

    /// A stale tick from the outgoing track can arrive while the new one is
    /// still `Loading`; it must not become the new track's position (it would
    /// anchor Discord's time bar at the previous track's end). Seeks while paused
    /// update the position directly, so only `Playing` ticks are accepted.
    #[test]
    fn a_position_tick_only_applies_while_playing() {
        assert!(position_tick_applies(PlaybackState::Playing));
        for state in [
            PlaybackState::Loading,
            PlaybackState::Paused,
            PlaybackState::Stopped,
        ] {
            assert!(
                !position_tick_applies(state),
                "a position tick while {state:?} must be ignored"
            );
        }
    }
}
