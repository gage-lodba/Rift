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

use super::nav::{play_next, play_next_after_ended, stop};
use super::playback::{begin_crossfade, should_arm_crossfade};
use super::state::PlayerCore;
use crate::audio::AudioEvent;
use crate::backend::BackendInner;
use crate::event::BackendEvent;
use crate::util::LockExt;

/// Which audio-thread event is being checked against the player state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioEventKind {
    Duration,
    Position,
}

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

/// Whether an audio-thread event tagged with `play_id` may act on `core`.
///
/// The audio thread echoes the play identity it is currently playing on every
/// [`AudioEvent::Position`], [`AudioEvent::Duration`] and [`AudioEvent::Ended`].
/// That identity is the manager's player generation: a hard play hands it
/// `core.generation`, a crossfade hands it the generation `commit_crossfade`
/// will assign. An event whose id no longer matches was queued for playback the
/// user has already left (Stop, a skip, a new play), so it must not touch the new
/// track: applying it used to let the outgoing track's `Ended` run `play_next`
/// against the new state (undoing the command or skipping a track) and let a
/// stale position overwrite the new track's bar or arm its crossfade.
///
/// `Ended` is deliberately not routed through this helper: `nav::handle_ended`
/// applies the stricter transport rule (advance only while `Playing`, stop at the
/// end when a pause raced it) because a `Pause` does not bump the generation.
/// `Duration` only needs the generation: it is emitted as a track starts, before
/// the manager commits `Playing`.
pub(super) fn audio_event_applies(core: &PlayerCore, play_id: u64, kind: AudioEventKind) -> bool {
    if core.generation != play_id {
        return false;
    }
    match kind {
        AudioEventKind::Duration => true,
        AudioEventKind::Position => position_tick_applies(core.state),
    }
}

/// Apply a decoder length correction from `play_id` to `core`.
///
/// Returns the `(position, changed)` the caller broadcasts (and, when `changed`,
/// re-pushes to Discord), or `None` when the event is stale. A stale correction
/// must relabel neither the new track's length nor Discord's time bar. Non-finite
/// lengths are rejected by the caller before this runs.
fn apply_duration_correction(
    core: &mut PlayerCore,
    play_id: u64,
    duration: f64,
) -> Option<(f64, bool)> {
    if !audio_event_applies(core, play_id, AudioEventKind::Duration) {
        return None;
    }
    // Only a real correction is worth a Discord push: if the fetch already
    // supplied this length the bar is anchored, and a second identical push just
    // burns Discord's rate budget.
    let changed = (core.duration - duration).abs() > 0.5;
    core.duration = duration;
    Some((core.position, changed))
}

/// Apply a position tick from `play_id` to `core` and decide whether it arms a
/// crossfade.
///
/// Returns the `(duration, armed)` the caller broadcasts and acts on, or `None`
/// when the tick is stale or the player is not `Playing`. A stale tick must not
/// move the new track's bar or arm its crossfade. Non-finite positions are
/// rejected by the caller before this runs.
fn apply_position_tick(core: &mut PlayerCore, play_id: u64, position: f64) -> Option<(f64, bool)> {
    if !audio_event_applies(core, play_id, AudioEventKind::Position) {
        return None;
    }
    core.position = position;
    // Arm a crossfade once, when the track is within the overlap window of its
    // end. The rule (repeat-one excluded, keyed by generation so it fires at most
    // once per track, never while a prefetch is in flight) is
    // `should_arm_crossfade`, where it is unit-tested.
    let armed = should_arm_crossfade(core, position);
    Some((core.duration, armed))
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
            AudioEvent::Duration(duration, play_id) => {
                // The audio thread only reports decoder lengths, so a non-finite
                // value means something upstream is broken. Rejecting it keeps
                // NaN/∞ out of `core.duration`, where it would poison the seek
                // bar and the crossfade window from then on.
                if !duration.is_finite() {
                    debug!("ignoring non-finite duration from the audio thread: {duration}");
                    continue;
                }
                let applied = {
                    let mut core = backend.player.core.lock_safe();
                    let applied = apply_duration_correction(&mut core, play_id, duration);
                    if applied.is_none() {
                        debug!(
                            "dropping a duration event from play {play_id}: the current play is {}",
                            core.generation
                        );
                    }
                    applied
                };
                let Some((position, changed)) = applied else {
                    continue;
                };
                backend.emit(BackendEvent::Progress(Progress { position, duration }));
                // The decoder's length is authoritative and may correct a
                // missing/zero metadata duration; hand a genuine correction to
                // Discord so its time bar shows (and is accurate) for those tracks.
                if changed {
                    backend.discord.set_duration(duration, position);
                }
            }
            AudioEvent::Position(position, play_id) => {
                if !position.is_finite() {
                    debug!("ignoring non-finite position from the audio thread: {position}");
                    continue;
                }
                let applied = {
                    let mut core = backend.player.core.lock_safe();
                    // Stale ticks from the outgoing track must neither overwrite
                    // the new track's position nor arm its crossfade.
                    let applied = apply_position_tick(&mut core, play_id, position);
                    if applied.is_none() {
                        debug!(
                            "dropping a position event from play {play_id}: the current play is {} ({:?})",
                            core.generation, core.state
                        );
                    }
                    applied
                };
                let Some((duration, crossfade)) = applied else {
                    continue;
                };
                backend.emit(BackendEvent::Progress(Progress { position, duration }));
                if crossfade {
                    debug!("nearing track end, starting crossfade");
                    begin_crossfade(&backend);
                }
            }
            AudioEvent::Ended(play_id) => {
                // The identity/state check and the pick share one core lock in
                // `play_next_after_ended`, so a queued end from the previous track
                // cannot advance the playback that replaced it.
                play_next_after_ended(&backend, play_id);
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
    use super::{
        apply_duration_correction, apply_position_tick, audio_event_applies, position_tick_applies,
        AudioEventKind,
    };
    use crate::player::state::PlayerCore;
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

    /// A queued event from the track the user already left must be dropped before
    /// it can touch the new playback: its identity is the old generation. This is
    /// the rule that stops an outgoing `Ended` from running `play_next` against
    /// the state a Stop/Next installed, a stale `Duration` from relabelling the
    /// new track, and a stale `Position` from overwriting its bar.
    #[test]
    fn an_event_from_a_superseded_play_is_dropped() {
        let core = PlayerCore {
            generation: 7,
            state: PlaybackState::Playing,
            position: 12.5,
            duration: 200.0,
            ..PlayerCore::default()
        };
        for kind in [AudioEventKind::Duration, AudioEventKind::Position] {
            assert!(
                !audio_event_applies(&core, 6, kind),
                "an event from the previous play must not act on the current one"
            );
            assert!(
                audio_event_applies(&core, 7, kind),
                "the current play's own {kind:?} event applies"
            );
        }
    }

    /// A tick only applies while playing/paused, and a length correction in any
    /// state (it only labels the current track). The `Ended` transport rule lives
    /// in `nav::handle_ended` and is tested there.
    #[test]
    fn ticks_only_apply_while_playing_or_paused() {
        let mut core = PlayerCore {
            generation: 4,
            state: PlaybackState::Playing,
            ..PlayerCore::default()
        };
        for state in [
            PlaybackState::Paused,
            PlaybackState::Loading,
            PlaybackState::Stopped,
        ] {
            core.state = state;
            assert!(
                !audio_event_applies(&core, 4, AudioEventKind::Position),
                "a tick while {state:?} must not move the bar"
            );
            assert!(
                audio_event_applies(&core, 4, AudioEventKind::Duration),
                "a decoder length correction still applies while {state:?}"
            );
        }
    }

    /// A queued duration correction from the outgoing track must not relabel the
    /// new one: it is dropped before `core.duration` changes, and `None` is what
    /// keeps the caller from pushing a stale length to Discord. The current
    /// play's correction still applies and reports whether it was big enough to
    /// re-push.
    #[test]
    fn a_stale_duration_correction_does_not_relabel_the_new_track() {
        let mut core = PlayerCore {
            generation: 7,
            state: PlaybackState::Playing,
            position: 12.5,
            duration: 200.0,
            ..PlayerCore::default()
        };
        assert_eq!(
            apply_duration_correction(&mut core, 6, 40.0),
            None,
            "a correction for the previous play is dropped"
        );
        assert_eq!(
            core.duration, 200.0,
            "a stale correction must not change the new track's length"
        );
        assert_eq!(
            apply_duration_correction(&mut core, 7, 180.25),
            Some((12.5, true)),
            "the current play's correction applies (and reports a Discord-worthy change)"
        );
        assert_eq!(core.duration, 180.25);
        assert_eq!(
            apply_duration_correction(&mut core, 7, 180.5),
            Some((12.5, false)),
            "a sub-half-second change is applied silently"
        );
    }

    /// A stale position tick must move neither the bar nor the crossfade arm.
    /// This is the outgoing track's end-of-song position arriving after the user
    /// skipped: applying it would jump the new seek bar to the end and arm a
    /// crossfade for a window the new track has not reached.
    #[test]
    fn a_stale_position_tick_cannot_move_the_bar_or_arm_a_crossfade() {
        let mut core = PlayerCore {
            queue: Vec::new(),
            generation: 7,
            state: PlaybackState::Playing,
            position: 100.0,
            duration: 200.0,
            crossfade: 10.0,
            ..PlayerCore::default()
        };

        // Stale idle (the audio thread is already at the outgoing track's end).
        assert_eq!(apply_position_tick(&mut core, 6, 195.0), None);
        assert_eq!(core.position, 100.0, "the new track's bar is untouched");
        assert_eq!(
            core.crossfade_armed_for, None,
            "a stale tick must not arm a crossfade"
        );

        // The current track's tick inside the window moves the bar and arms.
        assert_eq!(
            apply_position_tick(&mut core, 7, 195.0),
            Some((200.0, true))
        );
        assert_eq!(core.position, 195.0);
        assert_eq!(core.crossfade_armed_for, Some(7));

        // Current identity but paused/loading is still not a bar update.
        core.state = PlaybackState::Paused;
        assert_eq!(apply_position_tick(&mut core, 7, 120.0), None);
        assert_eq!(core.position, 195.0);
    }
}
