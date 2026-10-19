//! Bridge from the audio thread to the application: forward progress/duration,
//! arm crossfades, and advance the queue when a track ends.
//!
//! This loop runs as a task on the backend runtime. It used to take the shell's
//! `AppHandle`; it now takes the backend's shared state, and every former
//! `app.emit(...)` call is an event-bus send.

use std::sync::Arc;

use rift_types::{PlaybackState, Progress, RepeatMode};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::debug;

use super::nav::{play_next, stop};
use super::playback::begin_crossfade;
use crate::audio::AudioEvent;
use crate::backend::BackendInner;
use crate::event::BackendEvent;
use crate::util::LockExt;

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
                let (duration, crossfade) = {
                    let mut core = backend.player.core.lock_safe();
                    core.position = position;
                    // Arm a crossfade once, when the track is within the overlap
                    // window of its end. Repeat-one is excluded (a track can't
                    // overlap itself); keyed by generation so it fires at most
                    // once per track.
                    let arm = core.crossfade > 0.0
                        && core.state == PlaybackState::Playing
                        && core.repeat != RepeatMode::One
                        && core.duration > 0.0
                        && core.crossfade_armed_for != Some(core.generation)
                        // Don't re-arm while a crossfade prefetch is already in
                        // flight (e.g. after a seek cleared the arm flag).
                        && core.pending_next.is_none()
                        && core.duration - position <= core.crossfade;
                    if arm {
                        core.crossfade_armed_for = Some(core.generation);
                    }
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
