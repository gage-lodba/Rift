//! Bridge from the audio thread to the frontend: forward progress/duration,
//! arm crossfades, and advance the queue when a track ends.

use rift_types::{events, PlaybackState, Progress, RepeatMode};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::debug;

use super::nav::{play_next, stop};
use super::playback::begin_crossfade;
use crate::audio::AudioEvent;
use crate::util::LockExt;
use crate::AppState;

/// Forwards events from the audio thread to the frontend and advances the
/// queue when a track ends.
pub async fn event_loop(app: AppHandle, mut rx: UnboundedReceiver<AudioEvent>) {
    while let Some(ev) = rx.recv().await {
        match ev {
            AudioEvent::Duration(duration) => {
                let position = {
                    let state = app.state::<AppState>();
                    let mut core = state.player.core.lock_safe();
                    core.duration = duration;
                    core.position
                };
                let _ = app.emit(events::PROGRESS, Progress { position, duration });
            }
            AudioEvent::Position(position) => {
                let (duration, crossfade) = {
                    let state = app.state::<AppState>();
                    let mut core = state.player.core.lock_safe();
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
                let _ = app.emit(events::PROGRESS, Progress { position, duration });
                if crossfade {
                    debug!("nearing track end, starting crossfade");
                    begin_crossfade(&app);
                }
            }
            AudioEvent::Ended => {
                debug!("track ended, advancing queue");
                play_next(&app, false);
            }
            AudioEvent::Failed(msg) => {
                let _ = app.emit(events::ERROR, msg);
                stop(&app);
            }
        }
    }
}
